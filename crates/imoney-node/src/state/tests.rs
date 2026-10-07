use super::*;
use ed25519_dalek::SigningKey;
use imoney_core::{AddressType, TxInput};
use imoney_pow::MoneyPrinterContext;
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
    MoneyPrinterPow::new(Arc::new(MoneyPrinterContext::new(&Hash([1u8; 32]), 1024)))
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

fn open(name: &str) -> DagLedger {
    DagLedger::open_with_params(fresh_db(name), None, test_params()).expect("open ledger")
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
    assert!(node.get_transaction(&to_alice.id()).unwrap().is_some());

    // A competing branch of two blocks pays Bob with the same coin and overtakes it
    let block_b1 = mine_on(&node, &pow, &[fork_point], &other, vec![to_bob.clone()]);
    let hash_b1 = node.add_block(block_b1.clone(), &pow).unwrap();
    let block_b2 = mine_on(&node, &pow, &[hash_b1], &other, Vec::new());
    let hash_b2 = node.add_block(block_b2.clone(), &pow).unwrap();

    assert_eq!(node.virtual_selected_parent, hash_b2);
    assert_eq!(balance(&node, &alice), 0);
    assert_eq!(balance(&node, &bob), 60_000);
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
    }
    assert_eq!(fresh.storage.total_utxo_atoms().unwrap(), node.storage.total_utxo_atoms().unwrap());
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
        LedgerEvent::PendingTx { tx_id, outputs } => {
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
