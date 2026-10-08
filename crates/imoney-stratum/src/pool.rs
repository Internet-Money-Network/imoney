//! Pool accounting: who did how much of the work behind each block, and what they are owed.
//!
//! Every block the pool finds is paid to the pool's own address. When that reward has matured,
//! the pool keeps its fee and credits the rest to the miners whose shares came before the
//! block, in proportion to the work those shares prove (pay per last N shares: the shares
//! counted are the most recent ones adding up to twice a block's work, so joining just before
//! a block is found earns no more than steady mining). Credits are paid out once they pass a
//! minimum.
//!
//! This file holds only the bookkeeping, with no network or clock, so it can be tested exactly.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// Shares counted towards a block: the most recent ones adding up to this many blocks' work.
const WINDOW_BLOCKS: f64 = 2.0;
/// A found block whose reward has not appeared this many blocks later was not paid by the
/// network (it was merged as red, or not at all).
pub const GIVE_UP_AFTER_BLOCKS: u64 = 600;
/// A reward is credited, and a coin spent in a payout, only once it is this deep. Block
/// rewards can be spent after 20 blocks; the margin keeps the pool clear of the boundary,
/// where a reordering of recent blocks can leave a payment that spent a just-matured reward
/// out of the ledger.
pub const SETTLED_CONFIRMATIONS: u64 = 30;
/// A payout counts as made once it is this deep.
pub const PAYOUT_CONFIRMATIONS: u64 = 10;
/// Most miners paid in one transaction, which keeps it well under the transaction size limit.
pub const MAX_PAYEES_PER_PAYOUT: usize = 200;

/// A block the pool found, waiting for its reward to mature.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingBlock {
    pub block_hash: String,
    /// Transaction id of the coin the network creates for this block's reward.
    pub reward_tx_id: String,
    pub found_at_height: u64,
    /// Work credited to each address for this block.
    pub shares: Vec<(String, f64)>,
}

/// A payout that has been sent and not yet seen confirmed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Payout {
    pub tx_id: String,
    pub payees: Vec<(String, u64)>,
    pub sent_at_height: u64,
    /// The coins it spends, as (transaction id, index).
    #[serde(default)]
    pub inputs: Vec<(String, u32)>,
}

/// Everything the pool must remember across restarts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PoolState {
    /// Atoms owed to each miner, not yet paid.
    pub balances: BTreeMap<String, u64>,
    /// Atoms paid to each miner so far.
    pub paid: BTreeMap<String, u64>,
    pub pending: Vec<PendingBlock>,
    pub in_flight: Option<Payout>,
    /// Coins a payout that never reached the ledger tried to spend. The next payout spends at
    /// least one of them, so the lost one can never be accepted later and pay anyone twice.
    #[serde(default)]
    pub respend: Vec<(String, u32)>,
    pub blocks_found: u64,
    pub blocks_rewarded: u64,
    pub blocks_unpaid: u64,
    pub fee_earned_atoms: u64,
}

/// A coin of the pool's address, as the node lists it.
#[derive(Clone, Debug, Deserialize)]
pub struct Coin {
    pub transaction_id: String,
    pub index: u32,
    pub value_atoms: u64,
    #[serde(default = "yes")]
    pub spendable: bool,
    #[serde(default)]
    pub confirmations: u64,
}

impl Coin {
    /// Deep enough for the pool to rely on.
    pub fn settled(&self) -> bool {
        self.spendable && self.confirmations >= SETTLED_CONFIRMATIONS
    }
}

fn yes() -> bool {
    true
}

pub struct Accounting {
    pub state: PoolState,
    /// Fee in hundredths of a percent: 150 is 1.5%.
    fee_basis_points: u64,
    /// Recent shares, oldest first: who, and the work each proves.
    window: VecDeque<(String, f64)>,
}

impl Accounting {
    pub fn new(state: PoolState, fee_basis_points: u64) -> Self {
        Self { state, fee_basis_points: fee_basis_points.min(10_000), window: VecDeque::new() }
    }

    /// Records an accepted share. `block_work` bounds how much history is worth keeping.
    pub fn record_share(&mut self, address: &str, work: f64, block_work: f64) {
        self.window.push_back((address.to_string(), work));
        // Keep a margin beyond the window, as difficulty moves
        let keep = block_work * WINDOW_BLOCKS * 4.0;
        let mut total: f64 = self.window.iter().map(|(_, w)| w).sum();
        while self.window.len() > 1 && total - self.window[0].1 >= keep {
            total -= self.window.pop_front().expect("not empty").1;
        }
    }

    /// The work each address contributed to the block just found: the most recent shares
    /// adding up to `WINDOW_BLOCKS` times the block's work.
    fn shares_for_block(&self, block_work: f64) -> Vec<(String, f64)> {
        let mut left = block_work * WINDOW_BLOCKS;
        let mut by_address: BTreeMap<&str, f64> = BTreeMap::new();
        for (address, work) in self.window.iter().rev() {
            if left <= 0.0 {
                break;
            }
            let counted = work.min(left);
            *by_address.entry(address).or_default() += counted;
            left -= counted;
        }
        by_address.into_iter().map(|(address, work)| (address.to_string(), work)).collect()
    }

    /// Records a block the node accepted from this pool.
    pub fn block_found(&mut self, block_hash: String, reward_tx_id: String, height: u64, block_work: f64) {
        let shares = self.shares_for_block(block_work);
        self.state.blocks_found += 1;
        self.state.pending.push(PendingBlock { block_hash, reward_tx_id, found_at_height: height, shares });
    }

    /// Credits the blocks whose reward is now spendable and gives up on those the network
    /// never paid. Returns how many blocks were credited.
    pub fn settle(&mut self, coins: &[Coin], height: u64) -> usize {
        let mut credited = 0;
        let pending = std::mem::take(&mut self.state.pending);
        for block in pending {
            match coins.iter().find(|coin| coin.transaction_id == block.reward_tx_id) {
                Some(coin) if coin.settled() => {
                    let (credits, fee) = split(coin.value_atoms, self.fee_basis_points, &block.shares);
                    for (address, atoms) in credits {
                        *self.state.balances.entry(address).or_default() += atoms;
                    }
                    self.state.fee_earned_atoms += fee;
                    self.state.blocks_rewarded += 1;
                    credited += 1;
                }
                // Still maturing
                Some(_) => self.state.pending.push(block),
                None if height > block.found_at_height + GIVE_UP_AFTER_BLOCKS => self.state.blocks_unpaid += 1,
                None => self.state.pending.push(block),
            }
        }
        credited
    }

    /// The miners owed at least `minimum`, largest first, up to one transaction's worth.
    pub fn due(&self, minimum: u64) -> Vec<(String, u64)> {
        let mut due: Vec<(String, u64)> =
            self.state.balances.iter().filter(|(_, atoms)| **atoms >= minimum.max(1)).map(|(a, v)| (a.clone(), *v)).collect();
        due.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        due.truncate(MAX_PAYEES_PER_PAYOUT);
        due
    }

    /// Notes a payout as sent. The balances are reduced now, so it cannot be sent twice; if
    /// the payment turns out never to have reached the network, `payout_lost` restores them.
    pub fn payout_sent(&mut self, tx_id: String, payees: Vec<(String, u64)>, inputs: Vec<(String, u32)>, height: u64) {
        for (address, atoms) in &payees {
            if let Some(balance) = self.state.balances.get_mut(address) {
                *balance = balance.saturating_sub(*atoms);
                if *balance == 0 {
                    self.state.balances.remove(address);
                }
            }
        }
        self.state.in_flight = Some(Payout { tx_id, payees, sent_at_height: height, inputs });
    }

    /// The payout in flight is deep in the ledger.
    pub fn payout_confirmed(&mut self) {
        if let Some(payout) = self.state.in_flight.take() {
            self.state.respend.clear();
            for (address, atoms) in payout.payees {
                *self.state.paid.entry(address).or_default() += atoms;
            }
        }
    }

    /// The payout in flight is not in the ledger: the miners are owed again.
    pub fn payout_lost(&mut self) {
        if let Some(payout) = self.state.in_flight.take() {
            self.state.respend = payout.inputs;
            for (address, atoms) in payout.payees {
                *self.state.balances.entry(address).or_default() += atoms;
            }
        }
    }

    /// Total owed to miners, including a payout not yet confirmed.
    pub fn owed(&self) -> u64 {
        let in_flight: u64 = self.state.in_flight.iter().flat_map(|p| p.payees.iter().map(|(_, v)| *v)).sum();
        self.state.balances.values().sum::<u64>() + in_flight
    }
}

/// Divides a block reward: the pool's fee, and the rest among the shares in proportion to
/// their work. Whole atoms only; what rounding leaves over goes to the pool.
pub fn split(reward_atoms: u64, fee_basis_points: u64, shares: &[(String, f64)]) -> (Vec<(String, u64)>, u64) {
    let total_work: f64 = shares.iter().map(|(_, work)| work).sum();
    if total_work <= 0.0 {
        return (Vec::new(), reward_atoms);
    }
    let fee = (reward_atoms as u128 * fee_basis_points as u128 / 10_000) as u64;
    let for_miners = reward_atoms - fee;
    let credits: Vec<(String, u64)> = shares
        .iter()
        .map(|(address, work)| (address.clone(), ((for_miners as f64) * (work / total_work)).floor() as u64))
        .filter(|(_, atoms)| *atoms > 0)
        .collect();
    let credited: u64 = credits.iter().map(|(_, atoms)| atoms).sum();
    // Floating-point rounding must never credit more than there is
    if credited > for_miners {
        return (Vec::new(), reward_atoms);
    }
    (credits, reward_atoms - credited)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK_WORK: f64 = 1_000.0;
    const REWARD: u64 = 500_000_000;

    fn coin(tx_id: &str, spendable: bool) -> Coin {
        let confirmations = if spendable { SETTLED_CONFIRMATIONS } else { 5 };
        Coin { transaction_id: tx_id.to_string(), index: 0, value_atoms: REWARD, spendable, confirmations }
    }

    #[test]
    fn the_fee_is_one_and_a_half_percent_and_nothing_is_lost() {
        let shares = vec![("alice".to_string(), 3.0), ("bob".to_string(), 1.0)];
        let (credits, fee) = split(REWARD, 150, &shares);
        let credited: u64 = credits.iter().map(|(_, atoms)| atoms).sum();
        assert_eq!(credited + fee, REWARD);
        assert_eq!(fee, 7_500_000);
        assert_eq!(credits, vec![("alice".to_string(), 369_375_000), ("bob".to_string(), 123_125_000)]);

        // Amounts that do not divide evenly: the leftover atoms go to the pool, never from nowhere
        let thirds = vec![("a".to_string(), 1.0), ("b".to_string(), 1.0), ("c".to_string(), 1.0)];
        let (credits, fee) = split(1_000, 150, &thirds);
        assert_eq!(credits.iter().map(|(_, v)| v).sum::<u64>() + fee, 1_000);
        assert!(credits.iter().all(|(_, v)| *v == 328));
        // Nobody to pay: the pool keeps the reward rather than inventing a payee
        assert_eq!(split(REWARD, 150, &[]), (Vec::new(), REWARD));
    }

    #[test]
    fn a_block_pays_the_recent_shares_in_proportion_once_its_reward_matures() {
        let mut pool = Accounting::new(PoolState::default(), 150);
        // Old work that has scrolled out of the window by the time the block is found
        pool.record_share("early", 5_000.0, BLOCK_WORK);
        for _ in 0..15 {
            pool.record_share("alice", 100.0, BLOCK_WORK);
        }
        for _ in 0..5 {
            pool.record_share("bob", 100.0, BLOCK_WORK);
        }
        pool.block_found("block1".into(), "reward1".into(), 100, BLOCK_WORK);
        assert_eq!(pool.state.pending[0].shares, vec![("alice".to_string(), 1_500.0), ("bob".to_string(), 500.0)]);

        // Nothing is owed while the reward is missing or immature
        assert_eq!(pool.settle(&[], 110), 0);
        assert_eq!(pool.settle(&[coin("reward1", false)], 115), 0);
        // Spendable by the network's rule is not yet deep enough for the pool
        let just_matured = Coin { confirmations: 20, ..coin("reward1", true) };
        assert_eq!(pool.settle(&[just_matured], 120), 0);
        assert!(pool.state.balances.is_empty());

        assert_eq!(pool.settle(&[coin("reward1", true)], 125), 1);
        assert_eq!(pool.state.balances["alice"], 369_375_000);
        assert_eq!(pool.state.balances["bob"], 123_125_000);
        assert_eq!(pool.state.fee_earned_atoms, 7_500_000);
        assert_eq!(pool.owed() + pool.state.fee_earned_atoms, REWARD);
        // Settling again credits nothing more
        assert_eq!(pool.settle(&[coin("reward1", true)], 130), 0);
        assert_eq!(pool.owed() + pool.state.fee_earned_atoms, REWARD);
    }

    #[test]
    fn a_block_the_network_never_paid_is_dropped_without_crediting_anyone() {
        let mut pool = Accounting::new(PoolState::default(), 150);
        pool.record_share("alice", 100.0, BLOCK_WORK);
        pool.block_found("red".into(), "never".into(), 100, BLOCK_WORK);
        pool.settle(&[], 100 + GIVE_UP_AFTER_BLOCKS);
        assert_eq!(pool.state.pending.len(), 1);
        pool.settle(&[], 101 + GIVE_UP_AFTER_BLOCKS);
        assert!(pool.state.pending.is_empty() && pool.state.balances.is_empty());
        assert_eq!((pool.state.blocks_found, pool.state.blocks_rewarded, pool.state.blocks_unpaid), (1, 0, 1));
    }

    #[test]
    fn payouts_cannot_be_sent_twice_and_come_back_if_lost() {
        let mut pool = Accounting::new(PoolState::default(), 150);
        pool.state.balances.insert("alice".into(), 300_000_000);
        pool.state.balances.insert("bob".into(), 40_000_000);

        // Only balances over the minimum are due
        let due = pool.due(100_000_000);
        assert_eq!(due, vec![("alice".to_string(), 300_000_000)]);
        pool.payout_sent("tx1".into(), due, vec![("coin-a".into(), 0)], 200);
        assert!(pool.due(100_000_000).is_empty());
        assert_eq!(pool.owed(), 340_000_000);

        // The payment never arrived: Alice is owed again
        pool.payout_lost();
        assert_eq!(pool.state.balances["alice"], 300_000_000);
        assert!(pool.state.paid.is_empty());
        // ...and the next payout must spend the lost one's coin, so the two cannot both happen
        assert_eq!(pool.state.respend, vec![("coin-a".to_string(), 0)]);

        let due = pool.due(100_000_000);
        pool.payout_sent("tx2".into(), due, vec![("coin-a".into(), 0)], 210);
        pool.payout_confirmed();
        assert!(pool.state.respend.is_empty());
        assert_eq!(pool.state.paid["alice"], 300_000_000);
        assert_eq!(pool.owed(), 40_000_000);

        // What is remembered across a restart is exactly the state
        let saved = serde_json::to_string(&pool.state).unwrap();
        assert_eq!(serde_json::from_str::<PoolState>(&saved).unwrap(), pool.state);
    }
}
