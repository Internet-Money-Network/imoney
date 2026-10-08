use super::*;
use imoney_core::constants::BLOCKS_PER_HALVING_ERA;
use ed25519_dalek::SigningKey;
use imoney_core::{AddressType, TxInput};
use imoney_pow::{PowMode, PowParams};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// Reward maturity used by these tests.
const MATURITY: u64 = 2;

fn address_of(key: &SigningKey) -> Address {
    Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, key.verifying_key().as_bytes())
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn test_pow() -> MoneyPrinterPow {
    MoneyPrinterPow::new(PowParams::tiny(), Hash([1u8; 32]), PowMode::Full)
}

fn test_params() -> ConsensusParams {
    let mut params = ConsensusParams::testnet();
    params.coinbase_maturity = MATURITY;
    params.daa.retarget = false;
    params
}

fn fresh_db(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("imoney-test-{}-{}.redb", name, std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Relay policy for tests: 1 atom per byte, so the fees used below stay small round numbers.
fn test_mempool() -> crate::mempool::Mempool {
    crate::mempool::Mempool::default().with_min_fee_rate(1)
}

fn open(name: &str) -> DagLedger {
    let mut ledger = DagLedger::open_with_params(fresh_db(name), None, test_params()).expect("open ledger");
    ledger.mempool = test_mempool();
    ledger
}

/// Finds a nonce for a block whose header is otherwise final.
fn solve(mut block: Block, pow: &MoneyPrinterPow) -> Block {
    block.header.hash_merkle_root = Block::compute_merkle_root(&block.transactions);
    let pre_pow_hash = block.header.pre_pow_hash().unwrap();
    let (nonce, _) = pow
        .mine(&pre_pow_hash, block.header.bits, 0, 1_000_000, Arc::new(AtomicBool::new(false)))
        .expect("test difficulty must be minable");
    block.header.nonce = nonce;
    block
}

/// Mines a block on explicit parents carrying explicit transactions, without adding it.
fn mine_on(ledger: &DagLedger, pow: &MoneyPrinterPow, parents: &[Hash], payout: &Address, txs: Vec<Transaction>) -> Block {
    solve(ledger.build_block(parents, Some(payout), txs).expect("build block"), pow)
}

/// Mines the ledger's current template (tips and mempool) and adds it.
fn mine_tip(ledger: &mut DagLedger, pow: &MoneyPrinterPow, payout: &Address) -> Hash {
    let block = solve(ledger.get_mining_template(Some(payout)).block, pow);
    ledger.add_block(block, pow).expect("add block")
}

/// A ledger where `miner` owns one mature reward, with other blocks paid to a throwaway key.
fn funded_ledger(name: &str, miner: &SigningKey) -> (DagLedger, MoneyPrinterPow) {
    let mut ledger = open(name);
    let pow = test_pow();
    mine_tip(&mut ledger, &pow, &address_of(miner));
    for _ in 0..=MATURITY {
        mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    }
    assert_eq!(ledger.get_spendable_utxos(&address_of(miner)).unwrap().len(), 1);
    (ledger, pow)
}

fn pay(ledger: &DagLedger, from: &SigningKey, to: &Address, amount: u64, fee: u64) -> Transaction {
    let utxos = ledger.get_spendable_utxos(&address_of(from)).unwrap();
    Transaction::build_payment(from, Network::Testnet, to, amount, fee, utxos, None).unwrap()
}

fn balance(ledger: &DagLedger, address: &Address) -> u64 {
    ledger.get_balance(address).unwrap().0
}

/// The running supply total must always equal an actual count of every coin.
fn assert_supply_total_is_exact(ledger: &DagLedger) {
    assert_eq!(ledger.storage.total_utxo_atoms().unwrap(), ledger.storage.scan_utxo_atoms().unwrap());
}

/// An address's whole history, newest first.
fn history(ledger: &DagLedger, address: &Address) -> Vec<HistoryRow> {
    ledger.address_history(address, usize::MAX, None).unwrap().into_iter().map(|(row, _)| row).collect()
}

/// Everything an address's history says it received, less what it says it sent, must be
/// exactly the address's balance.
fn assert_history_adds_up(ledger: &DagLedger, address: &Address) {
    let net: i128 = history(ledger, address)
        .iter()
        .map(|row| row.received_atoms as i128 - row.sent_atoms as i128)
        .sum();
    assert_eq!(net, balance(ledger, address) as i128, "history does not add up to the balance");
}

/// Total subsidy of the blocks whose rewards exist in the current ledger state.
/// Rewards exist for every block in the virtual block's past that was merged as blue.
fn expected_supply(ledger: &DagLedger) -> u128 {
    let mut total = 0u128;
    let mut data = &ledger.virtual_ghostdag;
    loop {
        for blue in &data.mergeset_blues {
            let header = &ledger.blocks[blue];
            if !header.parents.is_empty() {
                total += block_subsidy_atoms(header.daa_score) as u128;
            }
        }
        if data.is_genesis() {
            return total;
        }
        data = &ledger.dag.get(&data.selected_parent).ghostdag;
    }
}

#[test]
fn network_stats_reflect_the_recent_blocks() {
    let payer = key(170);
    let shop = address_of(&key(171));
    let (mut ledger, pow) = funded_ledger("stats", &payer);
    let payment = pay(&ledger, &payer, &shop, 12_345, 1_000);
    let payment_id = ledger.broadcast_transaction(payment).unwrap();
    let carrying = mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    mine_tip(&mut ledger, &pow, &address_of(&key(200)));

    let stats = ledger.network_stats().unwrap();
    assert_eq!(stats.window_blocks, ledger.blocks.len() - 1); // every block except genesis
    assert_eq!(stats.difficulty, 2.0); // the easiest target: one hash in two succeeds
    assert_eq!(stats.difficulty_bits, "0x207fffff");
    assert_eq!(stats.transactions_in_window, 1);
    assert_eq!(stats.recent_transactions.len(), 1);
    assert_eq!(stats.recent_transactions[0].tx_id, payment_id.to_hex());
    assert_eq!(stats.recent_transactions[0].block_hash, carrying.to_hex());
    assert_eq!(stats.circulating_supply_atoms, ledger.storage.total_utxo_atoms().unwrap());
    assert_eq!(stats.block_reward_imn, 5.0);
    assert!(stats.annual_inflation_percent > 0.0);
    assert_eq!(stats.blocks_until_halving, Some(BLOCKS_PER_HALVING_ERA - ledger.virtual_daa_score));
    // Blocks in these tests are mined back to back, so only the signs are meaningful
    assert!(stats.hashrate_hps >= 0.0 && stats.average_block_time_sec >= 0.0);
}

#[test]
fn sync_paging_returns_every_block_parents_first() {
    let mut ledger = open("sync-paging");
    let pow = test_pow();
    let miner = address_of(&key(110));
    for _ in 0..25 {
        mine_tip(&mut ledger, &pow, &miner);
    }

    let locator = ledger.locator();
    assert_eq!(locator.first(), Some(&ledger.virtual_selected_parent));
    assert_eq!(locator.last(), Some(&ledger.genesis_hash));
    assert!(locator.len() < 20, "locator must thin out: {}", locator.len());

    // A peer that only has genesis pages through everything, 7 blocks at a time
    let mut cursor = (ledger.sync_start_level(&[Hash([9u8; 32]), ledger.genesis_hash]), Hash([0xff; 32]));
    let mut received: Vec<Hash> = Vec::new();
    loop {
        let (blocks, next) = ledger.blocks_after(cursor, 7, usize::MAX).unwrap();
        for block in &blocks {
            assert!(block.header.parents.iter().all(|p| *p == ledger.genesis_hash || received.contains(p)));
            received.push(block.hash());
        }
        match next {
            Some(position) => cursor = position,
            None => break,
        }
    }
    assert_eq!(received.len(), 25);

    // A peer that already has the tip is sent nothing
    let up_to_date = (ledger.sync_start_level(&locator), Hash([0xff; 32]));
    assert_eq!(ledger.blocks_after(up_to_date, 7, usize::MAX).unwrap(), (Vec::new(), None));
}

#[test]
fn mempool_rejects_double_spend_of_same_utxo() {
    let miner = key(3);
    let (mut ledger, _) = funded_ledger("double-spend", &miner);

    let first = pay(&ledger, &miner, &address_of(&key(4)), 1_000, 500);
    let second = pay(&ledger, &miner, &address_of(&key(5)), 1_000, 500);

    ledger.broadcast_transaction(first).expect("first spend is admitted");
    let err = ledger.broadcast_transaction(second).expect_err("second spend must be rejected");
    assert!(err.to_string().contains("already spent"), "{}", err);
    assert_eq!(ledger.mempool.len(), 1);
}

#[test]
fn mempool_rejects_output_total_overflow() {
    let miner = key(6);
    let (mut ledger, _) = funded_ledger("overflow", &miner);
    let (outpoint, _) = ledger.get_spendable_utxos(&address_of(&miner)).unwrap().remove(0);

    let script = ScriptPublicKey::pay_to_address(&address_of(&miner));
    let mut tx = Transaction {
        version: 1,
        inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new(), sequence: 0 }],
        // Wraps to 1 atom with unchecked u64 addition
        outputs: vec![
            TxOutput { value_atoms: u64::MAX, script_public_key: script.clone() },
            TxOutput { value_atoms: 2, script_public_key: script },
        ],
        lock_time: 0,
        subnetwork_id: [0u8; 20],
        gas: 0,
        payload: Vec::new(),
        service: None,
    };
    tx.sign_input(Network::Testnet, 0, &miner).unwrap();

    let err = ledger.broadcast_transaction(tx).expect_err("overflowing outputs must be rejected");
    assert!(err.to_string().contains("overflows"), "{}", err);
}

#[test]
fn block_rewards_cannot_be_spent_before_maturity() {
    let miner = key(7);
    let mut ledger = open("immature");
    let pow = test_pow();
    mine_tip(&mut ledger, &pow, &address_of(&miner));

    // The reward is visible in the balance but not yet spendable
    assert_eq!(balance(&ledger, &address_of(&miner)), block_subsidy_atoms(1));
    let listed = ledger.get_utxos_with_status(&address_of(&miner)).unwrap();
    assert_eq!((listed[0].2, listed[0].3), (false, 1), "one confirmation, not yet spendable");
    assert!(ledger.get_spendable_utxos(&address_of(&miner)).unwrap().is_empty());

    let utxos = ledger.get_utxos(&address_of(&miner)).unwrap();
    let early = Transaction::build_payment(&miner, Network::Testnet, &address_of(&key(8)), 1_000, 500, utxos, None).unwrap();
    let err = ledger.broadcast_transaction(early.clone()).expect_err("immature spend must be rejected");
    assert!(err.to_string().contains("immature"), "{}", err);

    for _ in 0..MATURITY {
        mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    }
    ledger.broadcast_transaction(early).expect("mature reward is spendable");
}

#[test]
fn payment_confirms_and_fee_goes_to_the_miner_of_its_block() {
    let miner = key(10);
    let alice = address_of(&key(11));
    let fee_collector = address_of(&key(12));
    let (mut ledger, pow) = funded_ledger("fees", &miner);

    let payment = pay(&ledger, &miner, &alice, 40_000, 1_000);
    let payment_id = ledger.broadcast_transaction(payment).unwrap();
    assert_eq!(ledger.get_transaction(&payment_id).unwrap().unwrap().confirmations, 0);

    let carrying_block = mine_tip(&mut ledger, &pow, &fee_collector);
    assert!(ledger.mempool.is_empty());
    assert_eq!(balance(&ledger, &alice), 40_000);

    // The block's miner receives the subsidy plus the 1,000-atom fee
    let reward = ledger.storage.get_utxo(&reward_outpoint(&carrying_block)).unwrap().unwrap();
    assert_eq!(reward.output.value_atoms, block_subsidy_atoms(ledger.blocks[&carrying_block].daa_score) + 1_000);
    assert_eq!(balance(&ledger, &fee_collector), reward.output.value_atoms);

    let info = ledger.get_transaction(&payment_id).unwrap().unwrap();
    assert_eq!(info.block_hash, Some(carrying_block));
    assert_eq!(info.confirmations, 1);
    mine_tip(&mut ledger, &pow, &fee_collector);
    assert_eq!(ledger.get_transaction(&payment_id).unwrap().unwrap().confirmations, 2);

    // Fees move coins; they never create or destroy them
    assert_eq!(ledger.storage.total_utxo_atoms().unwrap(), expected_supply(&ledger));
}

#[test]
fn two_nodes_reach_the_same_ledger_from_the_same_blocks() {
    let miner = key(13);
    let alice = address_of(&key(14));
    let (mut node_a, pow) = funded_ledger("node-a", &miner);
    let payment = pay(&node_a, &miner, &alice, 40_000, 1_000);
    node_a.broadcast_transaction(payment).unwrap();
    mine_tip(&mut node_a, &pow, &address_of(&miner));

    // Node B has its own payout address and an empty mempool, and sees only the blocks
    let mut node_b =
        DagLedger::open_with_params(fresh_db("node-b"), Some(address_of(&key(15))), test_params()).unwrap();
    let mut blocks: Vec<&BlockHeader> = node_a.blocks.values().filter(|h| !h.parents.is_empty()).collect();
    blocks.sort_by_key(|h| h.daa_score);
    for header in blocks {
        let block = node_a.storage.get_block(&header.hash()).unwrap().unwrap();
        node_b.add_block(block, &pow).unwrap();
    }

    assert_eq!(node_a.virtual_selected_parent, node_b.virtual_selected_parent);
    for address in [&alice, &address_of(&miner), &address_of(&key(15)), &address_of(&key(200))] {
        assert_eq!(balance(&node_a, address), balance(&node_b, address));
    }
    assert_eq!(balance(&node_b, &alice), 40_000);
    assert_eq!(balance(&node_b, &address_of(&key(15))), 0);
    assert_eq!(node_b.storage.total_utxo_atoms().unwrap(), expected_supply(&node_b));
}

#[test]
fn conflicting_parallel_blocks_resolve_the_same_way_in_any_arrival_order() {
    let miner = key(20);
    let (alice, bob) = (address_of(&key(21)), address_of(&key(22)));
    let (ledger, pow) = funded_ledger("conflict-source", &miner);
    let fork_point = ledger.virtual_selected_parent;

    // Two blocks on the same parent, each spending the miner's only coin to a different person
    let to_alice = pay(&ledger, &miner, &alice, 50_000, 500);
    let to_bob = pay(&ledger, &miner, &bob, 60_000, 500);
    let block_a = mine_on(&ledger, &pow, &[fork_point], &address_of(&key(23)), vec![to_alice.clone()]);
    let block_b = mine_on(&ledger, &pow, &[fork_point], &address_of(&key(24)), vec![to_bob.clone()]);
    let prefix: Vec<Block> = {
        let mut headers: Vec<&BlockHeader> = ledger.blocks.values().filter(|h| !h.parents.is_empty()).collect();
        headers.sort_by_key(|h| h.daa_score);
        headers.iter().map(|h| ledger.storage.get_block(&h.hash()).unwrap().unwrap()).collect()
    };

    let replay = |name: &str, order: [&Block; 2]| {
        let mut node = open(name);
        for block in prefix.iter().chain(order) {
            node.add_block(block.clone(), &pow).unwrap();
        }
        node
    };
    let node_x = replay("conflict-x", [&block_a, &block_b]);
    let node_y = replay("conflict-y", [&block_b, &block_a]);

    // Exactly one of the two payments was accepted, and both nodes agree which
    let paid = (balance(&node_x, &alice), balance(&node_x, &bob));
    assert!(paid == (50_000, 0) || paid == (0, 60_000), "{:?}", paid);
    assert_eq!(paid, (balance(&node_y, &alice), balance(&node_y, &bob)));
    assert_eq!(node_x.virtual_selected_parent, node_y.virtual_selected_parent);
    assert_eq!(node_x.tips, node_y.tips);
    assert_eq!(node_x.virtual_blue_score, node_y.virtual_blue_score);

    // Both blocks are blue, so both miners are paid; only the winner's block carries a fee
    assert_eq!(node_x.virtual_ghostdag.mergeset_blues.len(), 2);
    for node in [&node_x, &node_y] {
        assert_eq!(node.storage.total_utxo_atoms().unwrap(), expected_supply(node));
        let accepted = [&to_alice, &to_bob]
            .iter()
            .filter(|tx| node.get_transaction(&tx.id()).unwrap().is_some())
            .count();
        assert_eq!(accepted, 1);
    }
}

#[test]
fn heavier_side_chain_replaces_the_selected_chain_and_its_payments() {
    let miner = key(30);
    let (alice, bob) = (address_of(&key(31)), address_of(&key(32)));
    let (mut node, pow) = funded_ledger("reorg", &miner);
    let fork_point = node.virtual_selected_parent;
    let other = address_of(&key(33));

    let to_alice = pay(&node, &miner, &alice, 50_000, 500);
    let to_bob = pay(&node, &miner, &bob, 60_000, 500);

    // The node first sees a block paying Alice
    let block_a = mine_on(&node, &pow, &[fork_point], &other, vec![to_alice.clone()]);
    let hash_a = node.add_block(block_a.clone(), &pow).unwrap();
    assert_eq!(node.virtual_selected_parent, hash_a);
    assert_eq!(balance(&node, &alice), 50_000);
    assert_eq!(history(&node, &alice).iter().map(|row| row.id).collect::<Vec<_>>(), vec![to_alice.id()]);
    assert!(node.get_transaction(&to_alice.id()).unwrap().is_some());

    // A competing branch of two blocks pays Bob with the same coin and overtakes it
    let block_b1 = mine_on(&node, &pow, &[fork_point], &other, vec![to_bob.clone()]);
    let hash_b1 = node.add_block(block_b1.clone(), &pow).unwrap();
    let block_b2 = mine_on(&node, &pow, &[hash_b1], &other, Vec::new());
    let hash_b2 = node.add_block(block_b2.clone(), &pow).unwrap();

    assert_eq!(node.virtual_selected_parent, hash_b2);
    assert_eq!(balance(&node, &alice), 0);
    assert_eq!(balance(&node, &bob), 60_000);
    // The undone payment leaves Alice's history; Bob's shows the one that replaced it
    assert!(history(&node, &alice).is_empty());
    assert_eq!(history(&node, &bob).iter().map(|row| row.id).collect::<Vec<_>>(), vec![to_bob.id()]);
    assert!(node.get_transaction(&to_alice.id()).unwrap().is_none());
    assert_eq!(node.get_transaction(&to_bob.id()).unwrap().unwrap().block_hash, Some(hash_b1));
    // The losing block is still merged as blue, so its miner keeps the subsidy
    assert_eq!(node.tips, HashSet::from([hash_a, hash_b2]));
    assert_eq!(node.storage.total_utxo_atoms().unwrap(), expected_supply(&node));

    // A node that only ever saw the winning branch first ends in the identical state
    let mut fresh = open("reorg-fresh");
    let mut prefix: Vec<&BlockHeader> = node
        .blocks
        .values()
        .filter(|h| !h.parents.is_empty() && ![hash_a, hash_b1, hash_b2].contains(&h.hash()))
        .collect();
    prefix.sort_by_key(|h| h.daa_score);
    for header in prefix {
        fresh.add_block(node.storage.get_block(&header.hash()).unwrap().unwrap(), &pow).unwrap();
    }
    for block in [block_b1, block_b2, block_a] {
        fresh.add_block(block, &pow).unwrap();
    }
    assert_eq!(fresh.virtual_selected_parent, node.virtual_selected_parent);
    for address in [&alice, &bob, &other, &address_of(&miner)] {
        assert_eq!(balance(&fresh, address), balance(&node, address));
        assert_eq!(history(&fresh, address), history(&node, address));
        assert_history_adds_up(&node, address);
    }
    assert_eq!(fresh.storage.total_utxo_atoms().unwrap(), node.storage.total_utxo_atoms().unwrap());
    // After payments, a reorganisation and merges, the running totals still match a full count
    assert_supply_total_is_exact(&node);
    assert_supply_total_is_exact(&fresh);
}

#[test]
fn address_history_lists_rewards_and_payments_newest_first() {
    let miner = key(34);
    let (alice, pool) = (address_of(&key(35)), address_of(&key(36)));
    let (mut ledger, pow) = funded_ledger("history", &miner);
    let reward = balance(&ledger, &address_of(&miner));

    // The miner pays Alice twice, each payment returning change to the miner
    for amount in [70_000, 30_000] {
        let payment = pay(&ledger, &miner, &alice, amount, 500);
        ledger.broadcast_transaction(payment).unwrap();
        mine_tip(&mut ledger, &pow, &pool);
    }

    let alice_rows = history(&ledger, &alice);
    assert_eq!(alice_rows.iter().map(|row| row.received_atoms).collect::<Vec<_>>(), vec![30_000, 70_000]);
    assert!(alice_rows.iter().all(|row| row.sent_atoms == 0 && !row.is_reward && row.timestamp_ms > 0));

    // The miner: one reward, then two payments, each costing the amount and the fee
    let miner_rows = history(&ledger, &address_of(&miner));
    assert_eq!(miner_rows.len(), 3);
    assert!(miner_rows[2].is_reward);
    assert_eq!(miner_rows[2].received_atoms, reward);
    assert_eq!(miner_rows[0].sent_atoms - miner_rows[0].received_atoms, 30_500);
    assert_eq!(miner_rows[1].sent_atoms - miner_rows[1].received_atoms, 70_500);

    // Paging: one row at a time, continuing after the row just read, reaches every row once
    let mut paged = Vec::new();
    let mut before: Option<Vec<u8>> = None;
    loop {
        let page = ledger.address_history(&address_of(&miner), 1, before.as_deref()).unwrap();
        let Some((row, confirmations)) = page.into_iter().next() else { break };
        assert!(confirmations >= 1);
        before = Some(row.position());
        paged.push(row);
    }
    assert_eq!(paged, miner_rows);

    for address in [&alice, &pool, &address_of(&miner), &address_of(&key(200))] {
        assert_history_adds_up(&ledger, address);
    }

    // The history is on disk, not rebuilt: it is the same after a restart
    let path = std::env::temp_dir().join(format!("imoney-test-history-{}.redb", std::process::id()));
    drop(ledger);
    let reopened = DagLedger::open_with_params(path, None, test_params()).unwrap();
    assert_eq!(history(&reopened, &address_of(&miner)), miner_rows);
}

#[test]
fn multisig_address_receives_and_spends_with_enough_signatures() {
    use imoney_core::MultisigScript;

    let miner = key(40);
    let holders = [key(41), key(42), key(43)];
    let public_keys: Vec<[u8; 32]> = holders.iter().map(|k| k.verifying_key().to_bytes()).collect();
    let script = MultisigScript::new(2, &public_keys).unwrap();
    let vault = script.address(Network::Testnet);
    let shop = address_of(&key(44));
    let (mut ledger, pow) = funded_ledger("multisig", &miner);

    // Paying a multi-signature address is an ordinary payment
    let deposit = pay(&ledger, &miner, &vault, 900_000, 500);
    ledger.broadcast_transaction(deposit).unwrap();
    mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    assert_eq!(balance(&ledger, &vault), 900_000);

    let coins = ledger.get_spendable_utxos(&vault).unwrap();
    let mut payment =
        Transaction::build_multisig_payment(&script, Network::Testnet, &shop, 250_000, 1_000, coins, Some("INV-9"), None).unwrap();

    // Unsigned, and then signed by only one holder, it is refused
    assert!(ledger.broadcast_transaction(payment.clone()).is_err());
    let first = payment.multisig_sign(Network::Testnet, 0, &holders[2]);
    assert!(payment.set_multisig_signatures(0, &script, std::slice::from_ref(&first)).is_err());

    // Two holders, signing separately, are enough
    let second = payment.multisig_sign(Network::Testnet, 0, &holders[0]);
    payment.set_multisig_signatures(0, &script, &[first, second]).unwrap();
    ledger.broadcast_transaction(payment.clone()).unwrap();
    mine_tip(&mut ledger, &pow, &address_of(&key(200)));

    assert_eq!(balance(&ledger, &shop), 250_000);
    assert_eq!(balance(&ledger, &vault), 900_000 - 250_000 - 1_000);
    assert_eq!(ledger.invoice_payments("INV-9", &shop).unwrap().len(), 1);
    assert_history_adds_up(&ledger, &vault);
    assert_eq!(history(&ledger, &vault).len(), 2);
    assert_supply_total_is_exact(&ledger);
}

#[test]
fn header_fields_are_recomputed_not_trusted() {
    let miner = address_of(&key(40));
    let mut ledger = open("header-lies");
    let pow = test_pow();
    mine_tip(&mut ledger, &pow, &miner);
    let honest = ledger.get_mining_template(Some(&miner)).block;

    let mut reject = |mutate: fn(&mut Block), expected: &str| {
        let mut block = honest.clone();
        mutate(&mut block);
        let err = ledger.add_block(solve(block, &pow), &pow).expect_err(expected);
        assert!(err.to_string().contains(expected), "expected '{}' in '{}'", expected, err);
    };
    reject(|b| b.header.blue_score += 1, "Blue score");
    reject(|b| b.header.blue_work += 1, "Blue work");
    reject(|b| b.header.daa_score += 1, "DAA score");
    reject(|b| b.header.bits = 0x1f00ffff, "Difficulty bits");
    reject(|b| b.header.timestamp_ms = 1, "past median time");
    reject(|b| b.header.timestamp_ms += 10 * 60 * 1000, "future");
    reject(|b| b.header.utxo_commitment = Hash([1u8; 32]), "Reserved");
    reject(|b| b.header.parents.push(Hash([9u8; 32])), "Unknown parent");
    reject(|b| b.transactions[0].outputs[0].value_atoms += 1, "subsidy");
    reject(|b| b.transactions[0].payload[7] ^= 1, "DAA score");

    ledger.add_block(solve(honest, &pow), &pow).expect("the untouched block is valid");
}

#[test]
fn block_rejected_when_body_does_not_match_header() {
    let miner = key(41);
    let mut ledger = open("bad-merkle");
    let pow = test_pow();

    // Redirect the coinbase after mining: the header no longer commits to the body
    let mut block = solve(ledger.get_mining_template(Some(&address_of(&miner))).block, &pow);
    block.transactions[0].outputs[0].script_public_key = ScriptPublicKey::pay_to_address(&address_of(&key(42)));

    let err = ledger.add_block(block, &pow).expect_err("tampered body must be rejected");
    assert!(matches!(err, StateError::Block(BlockError::MerkleRootMismatch)), "{}", err);
    assert_eq!(ledger.blocks.len(), 1);
    assert_eq!(ledger.dag.len(), 1);
}

#[test]
fn unspendable_transaction_in_block_is_skipped_not_applied() {
    let miner = key(50);
    let thief = key(51);
    let (mut ledger, pow) = funded_ledger("skip-invalid", &miner);
    let (outpoint, utxo) = ledger.get_spendable_utxos(&address_of(&miner)).unwrap().remove(0);

    // The thief signs a spend of the miner's coin with their own key
    let mut theft = Transaction {
        version: 1,
        inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new(), sequence: 0 }],
        outputs: vec![TxOutput {
            value_atoms: utxo.value_atoms,
            script_public_key: ScriptPublicKey::pay_to_address(&address_of(&thief)),
        }],
        lock_time: 0,
        subnetwork_id: [0u8; 20],
        gas: 0,
        payload: Vec::new(),
        service: None,
    };
    theft.sign_input(Network::Testnet, 0, &thief).unwrap();
    let theft_id = theft.id();

    let tip = ledger.virtual_selected_parent;
    let block = mine_on(&ledger, &pow, &[tip], &address_of(&thief), vec![theft]);
    let daa_score = block.header.daa_score;
    ledger.add_block(block, &pow).expect("block itself is valid");

    assert_eq!(balance(&ledger, &address_of(&miner)), utxo.value_atoms);
    assert_eq!(balance(&ledger, &address_of(&thief)), block_subsidy_atoms(daa_score)); // reward only
    assert!(ledger.get_transaction(&theft_id).unwrap().is_none());
}

#[test]
fn ledger_recovers_state_after_restart() {
    let miner = key(60);
    let alice = address_of(&key(61));
    let path = fresh_db("restart");
    let pow = test_pow();

    let (tip, supply, miner_balance) = {
        let mut ledger = DagLedger::open_with_params(&path, None, test_params()).unwrap();
        ledger.mempool = test_mempool();
        mine_tip(&mut ledger, &pow, &address_of(&miner));
        for _ in 0..=MATURITY {
            mine_tip(&mut ledger, &pow, &address_of(&key(200)));
        }
        let payment = pay(&ledger, &miner, &alice, 7_000, 500);
        ledger.broadcast_transaction(payment).unwrap();
        let tip = mine_tip(&mut ledger, &pow, &address_of(&key(200)));
        (tip, ledger.storage.total_utxo_atoms().unwrap(), balance(&ledger, &address_of(&miner)))
    };

    let mut reopened = DagLedger::open_with_params(&path, None, test_params()).unwrap();
    assert_eq!(reopened.virtual_selected_parent, tip);
    assert_eq!(reopened.tips, HashSet::from([tip]));
    assert_eq!(reopened.blocks.len(), MATURITY as usize + 4);
    assert_eq!(balance(&reopened, &alice), 7_000);
    assert_eq!(balance(&reopened, &address_of(&miner)), miner_balance);
    assert_eq!(reopened.storage.total_utxo_atoms().unwrap(), supply);

    // The reopened ledger keeps working
    mine_tip(&mut reopened, &pow, &address_of(&key(200)));
    assert_eq!(reopened.storage.total_utxo_atoms().unwrap(), expected_supply(&reopened));
}

#[test]
fn mempool_enforces_minimum_fee_and_template_prefers_higher_fees() {
    let (alice, bob) = (key(70), key(71));
    let mut ledger = open("fee-policy");
    let pow = test_pow();
    mine_tip(&mut ledger, &pow, &address_of(&alice));
    mine_tip(&mut ledger, &pow, &address_of(&bob));
    for _ in 0..=MATURITY {
        mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    }

    let too_cheap = pay(&ledger, &alice, &address_of(&key(72)), 1_000, 10);
    let err = ledger.broadcast_transaction(too_cheap).expect_err("fee below the relay minimum");
    assert!(err.to_string().contains("below the minimum"), "{}", err);

    let modest = pay(&ledger, &alice, &address_of(&key(72)), 1_000, 400);
    let generous = pay(&ledger, &bob, &address_of(&key(72)), 1_000, 4_000);
    ledger.broadcast_transaction(modest.clone()).unwrap();
    ledger.broadcast_transaction(generous.clone()).unwrap();

    let template = ledger.get_mining_template(Some(&address_of(&key(200)))).block;
    let ids: Vec<Hash> = template.transactions.iter().skip(1).map(|tx| tx.id()).collect();
    assert_eq!(ids, vec![generous.id(), modest.id()]);
}

#[test]
fn ledger_announces_pending_transactions_and_new_blocks() {
    let miner = key(80);
    let alice = address_of(&key(81));
    let (mut ledger, pow) = funded_ledger("events", &miner);
    let mut events = ledger.events.subscribe();

    let payment = pay(&ledger, &miner, &alice, 9_000, 500);
    let payment_id = ledger.broadcast_transaction(payment).unwrap();
    match events.try_recv().expect("pending event") {
        LedgerEvent::PendingTx { tx_id, outputs, invoice_id } => {
            assert_eq!(invoice_id, None);
            assert_eq!(tx_id, payment_id);
            assert_eq!(outputs[0].value_atoms, 9_000);
            assert_eq!(outputs[0].script_public_key, ScriptPublicKey::pay_to_address(&alice));
        }
        other => panic!("unexpected event {:?}", other),
    }

    let hash = mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    match events.try_recv().expect("block event") {
        LedgerEvent::BlockAdded { hash: announced, blue_score } => {
            assert_eq!(announced, hash);
            assert_eq!(blue_score, ledger.virtual_blue_score);
        }
        other => panic!("unexpected event {:?}", other),
    }
}

#[test]
fn address_index_tracks_many_outputs_across_spends() {
    let miner = key(90);
    let (alice, bob) = (address_of(&key(91)), address_of(&key(92)));
    let (mut ledger, pow) = funded_ledger("address-index", &miner);

    // Three payments in a row, each spending the previous change
    for (i, to) in [&alice, &bob, &alice].into_iter().enumerate() {
        let payment = pay(&ledger, &miner, to, 1_000 * (i as u64 + 1), 500);
        ledger.broadcast_transaction(payment).unwrap();
        mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    }

    assert_eq!(ledger.get_utxos(&alice).unwrap().len(), 2);
    assert_eq!(balance(&ledger, &alice), 4_000);
    assert_eq!(balance(&ledger, &bob), 2_000);
    // The miner is left with exactly one change output
    assert_eq!(ledger.get_utxos(&address_of(&miner)).unwrap().len(), 1);
    assert_eq!(balance(&ledger, &address_of(&miner)), block_subsidy_atoms(1) - 6_000 - 1_500);
    assert_eq!(balance(&ledger, &address_of(&key(93))), 0);
}

#[test]
fn fee_is_split_between_the_miner_and_the_named_service_address() {
    let payer = key(100);
    let (alice, node_operator, block_miner) = (address_of(&key(101)), address_of(&key(102)), address_of(&key(103)));
    let (mut ledger, pow) = funded_ledger("fee-split", &payer);

    let utxos = ledger.get_spendable_utxos(&address_of(&payer)).unwrap();
    let payment =
        Transaction::build_payment(&payer, Network::Testnet, &alice, 20_000, 1_001, utxos, Some(&node_operator)).unwrap();
    let service_outpoint = payment.service_outpoint();
    ledger.broadcast_transaction(payment).unwrap();
    let carrying_block = mine_tip(&mut ledger, &pow, &block_miner);

    // Half the fee, rounded down, goes to the service address; the miner gets the rest
    assert_eq!(balance(&ledger, &node_operator), 500);
    assert_eq!(ledger.storage.get_utxo(&service_outpoint).unwrap().unwrap().output.value_atoms, 500);
    let subsidy = block_subsidy_atoms(ledger.blocks[&carrying_block].daa_score);
    assert_eq!(balance(&ledger, &block_miner), subsidy + 501);
    assert_eq!(balance(&ledger, &alice), 20_000);
    assert_eq!(ledger.storage.total_utxo_atoms().unwrap(), expected_supply(&ledger));

    // The service share is ordinary money: spendable at once, not held for maturity
    assert_eq!(ledger.get_spendable_utxos(&node_operator).unwrap().len(), 1);
    assert_eq!(ledger.get_info().service_address, None);
}

#[test]
fn consolidation_turns_many_outputs_into_one() {
    let miner = key(120);
    let me = address_of(&miner);
    let mut ledger = open("consolidate");
    let pow = test_pow();
    for _ in 0..4 {
        mine_tip(&mut ledger, &pow, &me);
    }
    for _ in 0..=MATURITY {
        mine_tip(&mut ledger, &pow, &address_of(&key(200)));
    }
    let utxos = ledger.get_spendable_utxos(&me).unwrap();
    assert_eq!(utxos.len(), 4);
    let before = balance(&ledger, &me);

    let merge = Transaction::build_consolidation(&miner, Network::Testnet, 2_000, utxos, None).unwrap();
    ledger.broadcast_transaction(merge).unwrap();
    mine_tip(&mut ledger, &pow, &address_of(&key(200)));

    assert_eq!(ledger.get_utxos(&me).unwrap().len(), 1);
    assert_eq!(balance(&ledger, &me), before - 2_000);
    assert_eq!(ledger.storage.total_utxo_atoms().unwrap(), expected_supply(&ledger));
}

/// Extends `parent` with `count` blocks one after another, adding each to the ledger.
fn extend(ledger: &mut DagLedger, pow: &MoneyPrinterPow, parent: Hash, count: usize, payout: &Address) -> Hash {
    let mut tip = parent;
    for _ in 0..count {
        let block = mine_on(ledger, pow, &[tip], payout, Vec::new());
        tip = ledger.add_block(block, pow).unwrap();
    }
    tip
}

#[test]
fn chain_forking_below_the_finality_point_is_refused_however_heavy() {
    let mut params = test_params();
    params.finality_depth = 5;
    let mut ledger = DagLedger::open_with_params(fresh_db("finality"), None, params).unwrap();
    let pow = test_pow();
    let (honest, attacker) = (address_of(&key(130)), address_of(&key(131)));

    let genesis = ledger.genesis_hash;
    let early = extend(&mut ledger, &pow, genesis, 2, &honest);
    let honest_tip = extend(&mut ledger, &pow, early, 10, &honest);
    assert_eq!(ledger.virtual_selected_parent, honest_tip);
    let honest_balance = balance(&ledger, &honest);

    // A longer chain from 10 blocks back: beyond the 5-block finality depth
    let attack_tip = extend(&mut ledger, &pow, early, 14, &attacker);
    assert!(ledger.dag.get(&attack_tip).ghostdag.blue_work > ledger.dag.get(&honest_tip).ghostdag.blue_work);
    assert_eq!(ledger.virtual_selected_parent, honest_tip, "the honest chain must stay selected");
    assert!(ledger.finality_conflict);
    assert!(ledger.get_info().finality_conflict);
    // Nothing the honest chain earned is disturbed, and the refused chain earns nothing
    assert_eq!(balance(&ledger, &honest), honest_balance);
    assert_eq!(balance(&ledger, &attacker), 0);
    assert_eq!(ledger.last_reorg, None);
    assert_eq!(ledger.virtual_parents, vec![honest_tip]);
    assert_eq!(ledger.storage.total_utxo_atoms().unwrap(), expected_supply(&ledger));

    // The honest chain keeps growing on top of its own tip
    let next = extend(&mut ledger, &pow, honest_tip, 1, &honest);
    assert_eq!(ledger.virtual_selected_parent, next);
}

#[test]
fn heavier_chain_forking_above_the_finality_point_wins_and_raises_the_reorg_alarm() {
    let mut params = test_params();
    params.finality_depth = 8;
    let mut ledger = DagLedger::open_with_params(fresh_db("reorg-alarm"), None, params).unwrap();
    let pow = test_pow();
    let (first, second) = (address_of(&key(132)), address_of(&key(133)));

    let genesis = ledger.genesis_hash;
    let base = extend(&mut ledger, &pow, genesis, 6, &first);
    let losing_tip = extend(&mut ledger, &pow, base, 4, &first);
    assert_eq!(ledger.virtual_selected_parent, losing_tip);

    // Forks 4 blocks back, inside the finality depth, and grows longer
    let winning_tip = extend(&mut ledger, &pow, base, 6, &second);
    assert_eq!(ledger.virtual_selected_parent, winning_tip);
    assert!(!ledger.finality_conflict);
    let (depth, _) = ledger.last_reorg.expect("a 4-block reorg is recorded");
    assert_eq!(depth, 4);
    assert_eq!(ledger.get_info().last_reorg_depth, Some(4));
    assert_eq!(ledger.storage.total_utxo_atoms().unwrap(), expected_supply(&ledger));
}

#[test]
fn invoice_payment_is_tracked_from_seen_to_buried_and_undone_by_a_reorg() {
    let payer = key(140);
    let (shop, other_shop) = (address_of(&key(141)), address_of(&key(142)));
    let (mut ledger, pow) = funded_ledger("invoice", &payer);
    let miner = address_of(&key(200));
    let fork_point = ledger.virtual_selected_parent;

    let utxos = ledger.get_spendable_utxos(&address_of(&payer)).unwrap();
    let payment =
        Transaction::build_invoice_payment(&payer, Network::Testnet, &shop, 75_000, 1_000, utxos, None, Some("INV-1042")).unwrap();
    let payment_id = payment.id();
    // The payer's attempt to take the money back: the same coin sent elsewhere
    let double_spend = pay(&ledger, &payer, &other_shop, 75_000, 1_000);
    assert!(ledger.invoice_payments("INV-1042", &shop).unwrap().is_empty());

    // Seen: in the mempool, no confirmations
    ledger.broadcast_transaction(payment.clone()).unwrap();
    let seen = ledger.invoice_payments("INV-1042", &shop).unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].tx_id, seen[0].amount_atoms, seen[0].confirmations), (payment_id, 75_000, 0));
    // Another invoice, or the same invoice at another shop's address, sees nothing
    assert!(ledger.invoice_payments("INV-1043", &shop).unwrap().is_empty());
    assert!(ledger.invoice_payments("INV-1042", &other_shop).unwrap().is_empty());

    // Included, then buried deeper with each block
    let carrying = mine_on(&ledger, &pow, &[fork_point], &miner, vec![payment]);
    let carrying_hash = ledger.add_block(carrying, &pow).unwrap();
    assert_eq!(ledger.invoice_payments("INV-1042", &shop).unwrap()[0].confirmations, 1);
    extend(&mut ledger, &pow, carrying_hash, 2, &miner);
    let buried = ledger.invoice_payments("INV-1042", &shop).unwrap();
    assert_eq!(buried.len(), 1);
    assert_eq!(buried[0].confirmations, 3);

    // A heavier branch that spends the same coin elsewhere replaces the chain
    let rival = mine_on(&ledger, &pow, &[fork_point], &miner, vec![double_spend]);
    let rival_hash = ledger.add_block(rival, &pow).unwrap();
    extend(&mut ledger, &pow, rival_hash, 4, &miner);
    assert_eq!(balance(&ledger, &other_shop), 75_000);
    assert_eq!(balance(&ledger, &shop), 0);
    assert!(ledger.get_transaction(&payment_id).unwrap().is_none());
    assert!(ledger.invoice_payments("INV-1042", &shop).unwrap().is_empty());
    assert!(ledger.network_alert(), "a 3-block reorganisation raises the alert");
}

#[test]
fn pruning_deletes_old_block_contents_but_not_the_ledger() {
    let payer = key(150);
    let shop = address_of(&key(151));
    let miner = address_of(&key(200));
    let path = fresh_db("prune");
    let pow = test_pow();
    let mut params = test_params();
    params.finality_depth = 5;

    let (old_block, old_payment, supply, balances) = {
        let mut ledger = DagLedger::open_with_params(&path, None, params.clone()).unwrap();
        ledger.mempool = test_mempool();
        mine_tip(&mut ledger, &pow, &address_of(&payer));
        for _ in 0..=MATURITY {
            mine_tip(&mut ledger, &pow, &miner);
        }
        // An early invoice payment, then a long stretch of blocks on top
        let utxos = ledger.get_spendable_utxos(&address_of(&payer)).unwrap();
        let payment =
            Transaction::build_invoice_payment(&payer, Network::Testnet, &shop, 30_000, 1_000, utxos, None, Some("OLD-1")).unwrap();
        let payment_id = ledger.broadcast_transaction(payment).unwrap();
        let carrying = mine_tip(&mut ledger, &pow, &miner);
        for _ in 0..40 {
            mine_tip(&mut ledger, &pow, &miner);
        }
        assert!(ledger.storage.get_block(&carrying).unwrap().is_some());
        assert_eq!(ledger.invoice_payments("OLD-1", &shop).unwrap().len(), 1);

        // A depth inside the finality window is refused
        assert!(ledger.enable_pruning(5).is_err());
        let pruned = ledger.enable_pruning(10).unwrap();
        assert!(pruned > 20, "{}", pruned);
        assert!(!ledger.get_info().archival);
        assert_eq!(ledger.pruned_floor, ledger.virtual_blue_score - 1 - 10);

        // Old contents and lookups are gone; the header and every balance remain
        assert!(ledger.storage.get_block(&carrying).unwrap().is_none());
        assert!(ledger.blocks.contains_key(&carrying));
        assert!(ledger.get_transaction(&payment_id).unwrap().is_none());
        assert!(ledger.invoice_payments("OLD-1", &shop).unwrap().is_empty());
        assert_eq!(balance(&ledger, &shop), 30_000);
        assert_eq!(ledger.storage.total_utxo_atoms().unwrap(), expected_supply(&ledger));
        // Recent blocks are untouched
        let tip = ledger.virtual_selected_parent;
        assert!(ledger.storage.get_block(&tip).unwrap().is_some());

        // The node keeps working, pruning as it goes
        let floor = ledger.pruned_floor;
        for _ in 0..5 {
            mine_tip(&mut ledger, &pow, &miner);
        }
        assert_eq!(ledger.pruned_floor, floor + 5);
        let recent = pay(&ledger, &payer, &shop, 5_000, 1_000);
        ledger.broadcast_transaction(recent).unwrap();
        mine_tip(&mut ledger, &pow, &miner);
        assert_eq!(balance(&ledger, &shop), 35_000);

        let balances = (balance(&ledger, &shop), balance(&ledger, &address_of(&payer)), balance(&ledger, &miner));
        (carrying, payment_id, ledger.storage.total_utxo_atoms().unwrap(), balances)
    };

    // A pruned database reopens and carries on
    let mut reopened = DagLedger::open_with_params(&path, None, params).unwrap();
    assert!(reopened.pruned_floor > 0);
    assert!(reopened.storage.get_block(&old_block).unwrap().is_none());
    assert!(reopened.get_transaction(&old_payment).unwrap().is_none());
    assert_eq!(reopened.storage.total_utxo_atoms().unwrap(), supply);
    assert_eq!((balance(&reopened, &shop), balance(&reopened, &address_of(&payer)), balance(&reopened, &miner)), balances);
    reopened.enable_pruning(10).unwrap();
    mine_tip(&mut reopened, &pow, &miner);
    assert_supply_total_is_exact(&reopened);
    assert_eq!(reopened.storage.total_utxo_atoms().unwrap(), expected_supply(&reopened));
}

/// Measures how much disk a block really takes. Not a pass/fail check, so it is skipped by default:
/// `cargo test --release -p imoney-node -- --ignored disk_use --nocapture`
#[test]
#[ignore]
fn disk_use_per_block() {
    let path = fresh_db("disk-use");
    let pow = test_pow();
    let miner = address_of(&key(160));
    let mut ledger = DagLedger::open_with_params(&path, None, test_params()).unwrap();
    let size = |path: &PathBuf| std::fs::metadata(path).unwrap().len();

    for _ in 0..2_000 {
        mine_tip(&mut ledger, &pow, &miner);
    }
    let at_2000 = size(&path);
    for _ in 0..6_000 {
        mine_tip(&mut ledger, &pow, &miner);
    }
    let per_block = (size(&path) - at_2000) / 6_000;
    println!("empty blocks: {} bytes each on disk, {:.1} MB a day", per_block, per_block as f64 * 17_280.0 / 1e6);

    // The same with pruning on from the start (depths shortened so pruning is active throughout)
    let pruned_path = fresh_db("disk-use-pruned");
    let mut params = test_params();
    params.finality_depth = 50;
    let mut pruned = DagLedger::open_with_params(&pruned_path, None, params).unwrap();
    pruned.enable_pruning(150).unwrap();
    for _ in 0..2_000 {
        mine_tip(&mut pruned, &pow, &miner);
    }
    let at_2000 = size(&pruned_path);
    for _ in 0..6_000 {
        mine_tip(&mut pruned, &pow, &miner);
    }
    let per_block = (size(&pruned_path) - at_2000) / 6_000;
    println!("pruned node: {} bytes each on disk, {:.1} MB a day", per_block, per_block as f64 * 17_280.0 / 1e6);
}

#[test]
fn block_removes_only_the_pending_transactions_it_invalidates() {
    let (alice, bob) = (key(180), key(181));
    let shop = address_of(&key(182));
    let miner = address_of(&key(200));
    let mut ledger = open("mempool-targeted");
    let pow = test_pow();
    mine_tip(&mut ledger, &pow, &address_of(&alice));
    mine_tip(&mut ledger, &pow, &address_of(&bob));
    for _ in 0..=MATURITY {
        mine_tip(&mut ledger, &pow, &miner);
    }

    // Alice has two competing payments; only one is in this node's mempool. Bob's is unrelated.
    let alice_pending = pay(&ledger, &alice, &shop, 1_000, 500);
    let alice_rival = pay(&ledger, &alice, &shop, 2_000, 500);
    let bob_pending = pay(&ledger, &bob, &shop, 3_000, 500);
    ledger.broadcast_transaction(alice_pending.clone()).unwrap();
    ledger.broadcast_transaction(bob_pending.clone()).unwrap();

    // A block arrives carrying Alice's other payment
    let tip = ledger.virtual_selected_parent;
    let block = mine_on(&ledger, &pow, &[tip], &miner, vec![alice_rival]);
    ledger.add_block(block, &pow).unwrap();

    assert!(!ledger.mempool.contains(&alice_pending.id()), "its coin was spent by the block");
    assert!(ledger.mempool.contains(&bob_pending.id()), "an unrelated payment stays");
    assert_eq!(ledger.mempool.len(), 1);

    // The next block confirms Bob's payment and empties the pool
    mine_tip(&mut ledger, &pow, &miner);
    assert!(ledger.mempool.is_empty());
    assert_eq!(balance(&ledger, &shop), 5_000);
    assert_supply_total_is_exact(&ledger);
}
