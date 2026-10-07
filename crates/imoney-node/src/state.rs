use crate::genesis::create_testnet_genesis;
use crate::storage::{BlockUpdate, NodeMeta, Storage, StorageError, StoredTxRecord};
use imoney_consensus::ghostdag::{order_ghostdag_parents, GhostdagParams};
use imoney_core::constants::{ATOMS_PER_IMN, MAX_BLOCK_BYTES, MAX_BLOCK_PARENTS, MAX_TX_BYTES, TARGET_TIME_PER_BLOCK_MS};
use imoney_core::{
    Address, Block, BlockError, BlockHeader, Encode, Hash, Network, Outpoint, ScriptPublicKey, Transaction, TxOutput,
};
use imoney_emission::{block_subsidy_atoms, block_subsidy_imn};
use imoney_pow::{compact_to_u256, is_valid_pow, MoneyPrinterPow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

/// Bytes of a block template kept free for the header and coinbase.
const TEMPLATE_RESERVED_BYTES: usize = 2_000;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("Block already exists in DAG: {0}")]
    BlockAlreadyExists(Hash),
    #[error("Unknown parent block hash: {0}")]
    UnknownParent(Hash),
    #[error("Block has no parents")]
    NoParents,
    #[error("Block exceeds maximum parent count: {0} > {1}")]
    TooManyParents(usize, usize),
    #[error("Invalid proof of work for block: {0}")]
    InvalidPoW(Hash),
    #[error("Header error: {0}")]
    Header(String),
    #[error("Invalid block: {0}")]
    Block(#[from] BlockError),
    #[error("Coinbase pays {0} atoms, above the {1}-atom subsidy")]
    CoinbaseTooLarge(u128, u64),
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("Transaction error: {0}")]
    Transaction(String),
}

/// JSON-serializable node status report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    pub network: String,
    pub total_blocks: usize,
    pub virtual_selected_parent: String,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    pub tips: Vec<String>,
    pub current_bits: String,
    pub current_block_reward_imn: f64,
    pub target_block_interval_sec: u64,
    pub mining_address: Option<String>,
    pub mempool_size: usize,
}

/// Candidate block mining template.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MiningTemplate {
    /// The full candidate block with `header.nonce` set to 0.
    pub block: Block,
    pub target_hex: String,
    pub pre_pow_hash: Hash,
    pub reward_imn: f64,
}

impl MiningTemplate {
    /// The finished block for a nonce that satisfies the target.
    pub fn into_block(mut self, nonce: u64) -> Block {
        self.block.header.nonce = nonce;
        self.block
    }
}

/// BlockDAG Ledger for Internet Money backed by ACID on-disk storage.
pub struct DagLedger {
    pub storage: Storage,
    pub network: Network,
    pub blocks: HashMap<Hash, BlockHeader>,
    pub tips: HashSet<Hash>,
    pub blue_scores: HashMap<Hash, u64>,
    pub virtual_selected_parent: Hash,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    pub difficulty_bits: u32,
    /// Default payout address for block templates.
    pub mining_address: Option<Address>,
    pub mempool: HashMap<Hash, Transaction>,
    ghostdag_params: GhostdagParams,
}


impl DagLedger {
    /// Opens the persistent ledger from disk or creates it if new.
    pub fn open(db_path: impl AsRef<Path>, mining_address: Option<Address>) -> Result<Self, StateError> {
        let storage = Storage::open(db_path)?;
        let (blocks, blue_scores, maybe_meta) = storage.load_state()?;

        if blocks.is_empty() {
            // Genesis initialization
            let genesis = create_testnet_genesis();
            let genesis_hash = genesis.hash();

            let meta = NodeMeta {
                virtual_selected_parent: genesis_hash,
                virtual_blue_score: 0,
                virtual_daa_score: 0,
                difficulty_bits: genesis.header.bits,
            };

            storage.apply_block(&BlockUpdate {
                hash: genesis_hash,
                block: &genesis,
                blue_score: 0,
                meta: Some(&meta),
                spent: &[],
                created: &[],
                records: &[],
            })?;

            let mut blocks = HashMap::new();
            let mut tips = HashSet::new();
            let mut blue_scores = HashMap::new();

            blocks.insert(genesis_hash, genesis.header.clone());
            tips.insert(genesis_hash);
            blue_scores.insert(genesis_hash, 0);

            Ok(Self {
                storage,
                network: Network::Testnet,
                blocks,
                tips,
                blue_scores,
                virtual_selected_parent: genesis_hash,
                virtual_blue_score: 0,
                virtual_daa_score: 0,
                difficulty_bits: genesis.header.bits,
                mining_address,
                mempool: HashMap::new(),
                ghostdag_params: GhostdagParams::default(),
            })
        } else {
            // Recover from disk
            let meta = maybe_meta.expect("Metadata must exist if blocks exist");

            // Calculate tips: all blocks that are not parents of any other block
            let mut non_tips = HashSet::new();
            for header in blocks.values() {
                for p in &header.parents {
                    non_tips.insert(*p);
                }
            }
            let tips: HashSet<Hash> = blocks.keys().filter(|h| !non_tips.contains(h)).copied().collect();

            Ok(Self {
                storage,
                network: Network::Testnet,
                blocks,
                tips,
                blue_scores,
                virtual_selected_parent: meta.virtual_selected_parent,
                virtual_blue_score: meta.virtual_blue_score,
                virtual_daa_score: meta.virtual_daa_score,
                difficulty_bits: meta.difficulty_bits,
                mining_address,
                mempool: HashMap::new(),
                ghostdag_params: GhostdagParams::default(),
            })
        }
    }

    /// Checks that a transaction can spend its inputs, given a view of the UTXO set.
    /// Returns the fee it pays. Failures are reported as `StateError::Transaction`.
    fn check_spend(
        &self,
        tx: &Transaction,
        lookup: impl Fn(&Outpoint) -> Result<Option<TxOutput>, StorageError>,
    ) -> Result<u64, StateError> {
        if tx.inputs.is_empty() {
            return Err(StateError::Transaction("Transaction has no inputs".to_string()));
        }
        if tx.outputs.is_empty() {
            return Err(StateError::Transaction("Transaction has no outputs".to_string()));
        }
        let size = tx.to_bytes().len();
        if size > MAX_TX_BYTES {
            return Err(StateError::Transaction(format!(
                "Transaction is {} bytes, above the {}-byte limit",
                size, MAX_TX_BYTES
            )));
        }

        let mut total_input_atoms: u64 = 0;
        let mut spent_outpoints = HashSet::new();

        for (i, input) in tx.inputs.iter().enumerate() {
            if !spent_outpoints.insert(&input.previous_outpoint) {
                return Err(StateError::Transaction("Duplicate input in transaction".to_string()));
            }

            let utxo = lookup(&input.previous_outpoint)?
                .ok_or_else(|| StateError::Transaction(format!("UTXO not found: {:?}", input.previous_outpoint)))?;

            // Verify signature against public key and address hash
            tx.verify_input(self.network, i, &utxo.script_public_key)
                .map_err(|e| StateError::Transaction(format!("Signature check failed on input {}: {}", i, e)))?;

            total_input_atoms = total_input_atoms
                .checked_add(utxo.value_atoms)
                .ok_or_else(|| StateError::Transaction("Input total overflows".to_string()))?;
        }

        let total_output_atoms = tx
            .outputs
            .iter()
            .try_fold(0u64, |acc, o| acc.checked_add(o.value_atoms))
            .ok_or_else(|| StateError::Transaction("Output total overflows".to_string()))?;
        if total_input_atoms < total_output_atoms {
            return Err(StateError::Transaction(format!(
                "Inputs {} atoms < outputs {} atoms",
                total_input_atoms, total_output_atoms
            )));
        }

        Ok(total_input_atoms - total_output_atoms)
    }

    /// Validates a newly mined block and applies it to the BlockDAG and persistent storage.
    ///
    /// The block's own transactions are what change the ledger: the coinbase pays whoever the
    /// miner named, and each transaction that is spendable at this point is accepted. A transaction
    /// that is not spendable (for example because a parallel block already spent its inputs) is
    /// skipped without invalidating the block.
    pub fn add_block(
        &mut self,
        block: Block,
        pow_engine: &MoneyPrinterPow,
    ) -> Result<Hash, StateError> {
        let header = &block.header;
        let block_hash = header.hash();

        if self.blocks.contains_key(&block_hash) {
            return Err(StateError::BlockAlreadyExists(block_hash));
        }

        // Validate parents
        if header.parents.is_empty() {
            return Err(StateError::NoParents);
        }
        if header.parents.len() > MAX_BLOCK_PARENTS {
            return Err(StateError::TooManyParents(header.parents.len(), MAX_BLOCK_PARENTS));
        }
        for parent in &header.parents {
            if !self.blocks.contains_key(parent) {
                return Err(StateError::UnknownParent(*parent));
            }
        }

        // Validate Proof of Work
        let pre_pow_hash = header.pre_pow_hash().map_err(|e| StateError::Header(e.to_string()))?;
        let pow_hash = pow_engine.calculate_hash(&pre_pow_hash, header.nonce);
        if !is_valid_pow(&pow_hash, header.bits) {
            return Err(StateError::InvalidPoW(block_hash));
        }

        // Validate the body: coinbase first, sizes, merkle root
        block.validate_structure()?;

        // The coinbase may create at most the block subsidy. Fees are not yet paid out.
        let coinbase = &block.transactions[0];
        let subsidy = block_subsidy_atoms(header.daa_score);
        let coinbase_total: u128 = coinbase.outputs.iter().map(|o| o.value_atoms as u128).sum();
        if coinbase_total > subsidy as u128 {
            return Err(StateError::CoinbaseTooLarge(coinbase_total, subsidy));
        }

        // Work out the UTXO changes without touching storage yet
        let mut spent: Vec<Outpoint> = Vec::new();
        let mut spent_set: HashSet<Outpoint> = HashSet::new();
        let mut created: Vec<(Outpoint, TxOutput)> = Vec::new();
        let mut created_map: HashMap<Outpoint, TxOutput> = HashMap::new();
        let mut records: Vec<StoredTxRecord> = Vec::new();

        for tx in &block.transactions {
            if !tx.is_coinbase() {
                let view = |outpoint: &Outpoint| {
                    if spent_set.contains(outpoint) {
                        return Ok(None);
                    }
                    match created_map.get(outpoint) {
                        Some(output) => Ok(Some(output.clone())),
                        None => self.storage.get_utxo(outpoint),
                    }
                };
                match self.check_spend(tx, view) {
                    Ok(_fee) => {}
                    Err(StateError::Transaction(_)) => continue, // Not spendable here: skip it
                    Err(e) => return Err(e),
                }
                for input in &tx.inputs {
                    spent_set.insert(input.previous_outpoint.clone());
                    spent.push(input.previous_outpoint.clone());
                }
            }

            let tx_id = tx.id();
            for (idx, output) in tx.outputs.iter().enumerate() {
                let outpoint = Outpoint {
                    transaction_id: tx_id,
                    index: idx as u32,
                };
                created_map.insert(outpoint.clone(), output.clone());
                created.push((outpoint, output.clone()));
            }
            records.push(StoredTxRecord {
                tx: tx.clone(),
                block_hash,
                daa_score: header.daa_score,
                timestamp_ms: header.timestamp_ms,
            });
        }
        // Outputs created and spent inside this block never reach the UTXO set
        created.retain(|(outpoint, _)| !spent_set.contains(outpoint));
        spent.retain(|outpoint| !created_map.contains_key(outpoint));

        // Run GHOSTDAG parent ordering
        let ghostdag = order_ghostdag_parents(&header.parents, &self.blue_scores, &self.ghostdag_params);

        let is_new_selected = ghostdag.blue_score > self.virtual_blue_score;
        let meta = is_new_selected.then(|| NodeMeta {
            virtual_selected_parent: block_hash,
            virtual_blue_score: ghostdag.blue_score,
            virtual_daa_score: header.daa_score,
            difficulty_bits: header.bits,
        });

        // Persist the block and its UTXO changes atomically
        self.storage.apply_block(&BlockUpdate {
            hash: block_hash,
            block: &block,
            blue_score: ghostdag.blue_score,
            meta: meta.as_ref(),
            spent: &spent,
            created: &created,
            records: &records,
        })?;

        // Update in-memory state
        for parent in &header.parents {
            self.tips.remove(parent);
        }
        self.tips.insert(block_hash);
        self.blue_scores.insert(block_hash, ghostdag.blue_score);
        self.blocks.insert(block_hash, header.clone());

        if let Some(meta) = meta {
            self.virtual_selected_parent = meta.virtual_selected_parent;
            self.virtual_blue_score = meta.virtual_blue_score;
            self.virtual_daa_score = meta.virtual_daa_score;
            self.difficulty_bits = meta.difficulty_bits;
        }

        // Drop confirmed transactions, and pending ones whose inputs this block spent
        for record in &records {
            self.mempool.remove(&record.tx.id());
        }
        let storage = &self.storage;
        self.mempool.retain(|_, tx| {
            tx.inputs
                .iter()
                .all(|input| !matches!(storage.get_utxo(&input.previous_outpoint), Ok(None)))
        });

        Ok(block_hash)
    }


    /// Validates and admits a signed transaction into the mempool.
    pub fn broadcast_transaction(&mut self, tx: Transaction) -> Result<Hash, StateError> {
        let tx_id = tx.id();
        if self.mempool.contains_key(&tx_id) {
            return Ok(tx_id);
        }

        self.check_spend(&tx, |outpoint| self.storage.get_utxo(outpoint))?;

        // Reject a transaction that spends an outpoint already claimed by a pending transaction
        for pending in self.mempool.values() {
            for pending_input in &pending.inputs {
                if tx.inputs.iter().any(|input| input.previous_outpoint == pending_input.previous_outpoint) {
                    return Err(StateError::Transaction(format!(
                        "Input already spent by pending transaction {}",
                        pending.id()
                    )));
                }
            }
        }

        self.mempool.insert(tx_id, tx);
        Ok(tx_id)
    }


    /// Generates a candidate block for miners. The coinbase pays `payout`, or the node's
    /// configured mining address when none is given.
    pub fn get_mining_template(&self, payout: Option<&Address>) -> MiningTemplate {
        let mut parents: Vec<Hash> = self.tips.iter().copied().collect();
        parents.sort();
        if parents.len() > MAX_BLOCK_PARENTS {
            parents.truncate(MAX_BLOCK_PARENTS);
        }

        let ghostdag = order_ghostdag_parents(&parents, &self.blue_scores, &self.ghostdag_params);
        let daa_score = self.virtual_daa_score + 1;
        let blue_score = ghostdag.blue_score;
        let bits = self.difficulty_bits;

        let coinbase_outputs = payout
            .or(self.mining_address.as_ref())
            .map(|address| TxOutput {
                value_atoms: block_subsidy_atoms(daa_score),
                script_public_key: ScriptPublicKey::pay_to_address(address),
            })
            .into_iter()
            .collect();
        let mut transactions = vec![Transaction::coinbase(daa_score, coinbase_outputs, &[])];

        // Fill the block with pending transactions in a deterministic order
        let mut pending: Vec<(&Hash, &Transaction)> = self.mempool.iter().collect();
        pending.sort_by_key(|(id, _)| **id);
        let mut used_bytes = TEMPLATE_RESERVED_BYTES;
        for (_, tx) in pending {
            let size = tx.to_bytes().len();
            if used_bytes + size > MAX_BLOCK_BYTES {
                continue;
            }
            used_bytes += size;
            transactions.push(tx.clone());
        }

        let header = BlockHeader {
            version: 1,
            parents,
            hash_merkle_root: Block::compute_merkle_root(&transactions),
            accepted_id_merkle_root: Hash::ZERO,
            utxo_commitment: Hash::ZERO,
            timestamp_ms: chrono::Utc::now().timestamp_millis() as u64,
            bits,
            nonce: 0,
            daa_score,
            blue_score,
            blue_work: blue_score as u128 * 1000,
        };

        let pre_pow_hash = header.pre_pow_hash().unwrap_or(Hash::ZERO);
        let target = compact_to_u256(bits);

        MiningTemplate {
            block: Block { header, transactions },
            target_hex: hex::encode(target),
            pre_pow_hash,
            reward_imn: block_subsidy_imn(daa_score),
        }
    }

    /// Queries live spendable balance for an address in whole IMN and atomic units.
    pub fn get_balance(&self, address: &Address) -> Result<(u64, f64), StorageError> {
        let atoms = self.storage.get_balance(address)?;
        let coins = (atoms as f64) / (ATOMS_PER_IMN as f64);
        Ok((atoms, coins))
    }

    /// Queries list of UTXOs for an address.
    pub fn get_utxos(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput)>, StorageError> {
        self.storage.get_utxos(address)
    }

    /// Queries a transaction record by hash, checking pending mempool first, then persistent storage.
    pub fn get_transaction(&self, tx_id: &Hash) -> Result<Option<(Transaction, Option<Hash>, Option<u64>)>, StorageError> {
        if let Some(tx) = self.mempool.get(tx_id) {
            return Ok(Some((tx.clone(), None, None)));
        }
        if let Some(record) = self.storage.get_transaction(tx_id)? {
            return Ok(Some((record.tx, Some(record.block_hash), Some(record.daa_score))));
        }
        Ok(None)
    }

    /// Node diagnostic overview.
    pub fn get_info(&self) -> NodeInfo {
        NodeInfo {
            network: "testnet-1".to_string(),
            total_blocks: self.blocks.len(),
            virtual_selected_parent: self.virtual_selected_parent.to_hex(),
            virtual_blue_score: self.virtual_blue_score,
            virtual_daa_score: self.virtual_daa_score,
            tips: self.tips.iter().map(|t| t.to_hex()).collect(),
            current_bits: format!("0x{:08x}", self.difficulty_bits),
            current_block_reward_imn: block_subsidy_imn(self.virtual_daa_score),
            target_block_interval_sec: TARGET_TIME_PER_BLOCK_MS / 1000,
            mining_address: self.mining_address.as_ref().map(|a| a.to_string()),
            mempool_size: self.mempool.len(),
        }
    }
}



pub type SharedLedger = Arc<RwLock<DagLedger>>;

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use imoney_core::{AddressType, TxInput};
    use imoney_pow::MoneyPrinterContext;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn address_of(key: &SigningKey) -> Address {
        Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, key.verifying_key().as_bytes())
    }

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn test_pow() -> MoneyPrinterPow {
        MoneyPrinterPow::new(Arc::new(MoneyPrinterContext::new(&Hash([1u8; 32]), 1024)))
    }

    fn fresh_db(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("imoney-test-{}-{}.redb", name, std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Mines the ledger's current template, paying `payout`, without adding the block.
    fn mine(ledger: &DagLedger, pow: &MoneyPrinterPow, payout: &Address) -> Block {
        let template = ledger.get_mining_template(Some(payout));
        let (nonce, _) = pow
            .mine(&template.pre_pow_hash, template.block.header.bits, 0, 1_000_000, Arc::new(AtomicBool::new(false)))
            .expect("devnet difficulty must be minable");
        template.into_block(nonce)
    }

    /// Re-mines a block after its contents were changed by a test.
    fn remine(mut block: Block, pow: &MoneyPrinterPow) -> Block {
        block.header.hash_merkle_root = Block::compute_merkle_root(&block.transactions);
        let pre_pow_hash = block.header.pre_pow_hash().unwrap();
        let (nonce, _) = pow
            .mine(&pre_pow_hash, block.header.bits, 0, 1_000_000, Arc::new(AtomicBool::new(false)))
            .unwrap();
        block.header.nonce = nonce;
        block
    }

    /// Opens a fresh ledger and mines one block so `miner` owns a coinbase UTXO.
    fn ledger_with_one_block(name: &str, miner: &SigningKey) -> (DagLedger, MoneyPrinterPow) {
        let mut ledger = DagLedger::open(fresh_db(name), None).expect("open ledger");
        let pow = test_pow();
        let block = mine(&ledger, &pow, &address_of(miner));
        ledger.add_block(block, &pow).expect("add block");
        (ledger, pow)
    }

    fn pay(ledger: &DagLedger, from: &SigningKey, to: &Address, amount: u64, fee: u64) -> Transaction {
        let utxos = ledger.get_utxos(&address_of(from)).unwrap();
        Transaction::build_payment(from, Network::Testnet, to, amount, fee, utxos).unwrap()
    }

    #[test]
    fn mempool_rejects_double_spend_of_same_utxo() {
        let miner = key(3);
        let (mut ledger, _) = ledger_with_one_block("double-spend", &miner);
        assert_eq!(ledger.get_utxos(&address_of(&miner)).unwrap().len(), 1);

        let first = pay(&ledger, &miner, &address_of(&key(4)), 1_000, 100);
        let second = pay(&ledger, &miner, &address_of(&key(5)), 1_000, 100);

        ledger.broadcast_transaction(first).expect("first spend is admitted");
        let err = ledger.broadcast_transaction(second).expect_err("second spend must be rejected");
        assert!(err.to_string().contains("already spent"), "{}", err);
        assert_eq!(ledger.mempool.len(), 1);
    }

    #[test]
    fn mempool_rejects_output_total_overflow() {
        let miner = key(6);
        let (mut ledger, _) = ledger_with_one_block("overflow", &miner);
        let (outpoint, _) = ledger.get_utxos(&address_of(&miner)).unwrap().remove(0);

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
        };
        tx.sign_input(Network::Testnet, 0, &miner).unwrap();

        let err = ledger.broadcast_transaction(tx).expect_err("overflowing outputs must be rejected");
        assert!(err.to_string().contains("overflows"), "{}", err);
    }

    #[test]
    fn two_nodes_reach_the_same_ledger_from_the_same_blocks() {
        let miner = key(10);
        let alice = address_of(&key(11));
        let subsidy = block_subsidy_atoms(1);

        // Node A mines a block, takes a payment into its mempool, and mines it into a second block
        let (mut node_a, pow) = ledger_with_one_block("node-a", &miner);
        let payment = pay(&node_a, &miner, &alice, 40_000, 1_000);
        let payment_id = node_a.broadcast_transaction(payment).unwrap();
        let block_2 = mine(&node_a, &pow, &address_of(&miner));
        assert_eq!(block_2.transactions.len(), 2);
        let hash_2 = node_a.add_block(block_2.clone(), &pow).unwrap();
        assert!(node_a.mempool.is_empty());

        // Node B has a different payout address and an empty mempool, and sees only the blocks
        let mut node_b = DagLedger::open(fresh_db("node-b"), Some(address_of(&key(12)))).unwrap();
        let block_1 = node_a.storage.get_block(&block_2.header.parents[0]).unwrap().unwrap();
        node_b.add_block(block_1, &pow).unwrap();
        node_b.add_block(block_2, &pow).unwrap();

        for node in [&node_a, &node_b] {
            assert_eq!(node.get_balance(&alice).unwrap().0, 40_000);
            assert_eq!(node.get_balance(&address_of(&miner)).unwrap().0, 2 * subsidy - 41_000);
            assert_eq!(node.get_balance(&address_of(&key(12))).unwrap().0, 0);
            // The 1,000-atom fee is burned until fee payout is implemented
            assert_eq!(node.storage.total_utxo_atoms().unwrap(), (2 * subsidy - 1_000) as u128);
            let (_, confirmed_in, _) = node.get_transaction(&payment_id).unwrap().unwrap();
            assert_eq!(confirmed_in, Some(hash_2));
        }
        assert_eq!(node_a.virtual_selected_parent, node_b.virtual_selected_parent);
    }

    #[test]
    fn block_rejected_when_coinbase_exceeds_subsidy() {
        let miner = key(20);
        let (mut ledger, pow) = ledger_with_one_block("greedy-coinbase", &miner);

        let mut block = mine(&ledger, &pow, &address_of(&miner));
        block.transactions[0].outputs[0].value_atoms += 1;
        let block = remine(block, &pow);

        let err = ledger.add_block(block, &pow).expect_err("oversized coinbase must be rejected");
        assert!(matches!(err, StateError::CoinbaseTooLarge(..)), "{}", err);
    }

    #[test]
    fn block_rejected_when_body_does_not_match_header() {
        let miner = key(21);
        let (mut ledger, pow) = ledger_with_one_block("bad-merkle", &miner);

        // Redirect the coinbase after mining: the header no longer commits to the body
        let mut block = mine(&ledger, &pow, &address_of(&miner));
        block.transactions[0].outputs[0].script_public_key = ScriptPublicKey::pay_to_address(&address_of(&key(22)));

        let err = ledger.add_block(block, &pow).expect_err("tampered body must be rejected");
        assert!(matches!(err, StateError::Block(BlockError::MerkleRootMismatch)), "{}", err);
    }

    #[test]
    fn unspendable_transaction_in_block_is_skipped_not_applied() {
        let miner = key(30);
        let thief = key(31);
        let (mut ledger, pow) = ledger_with_one_block("skip-invalid", &miner);
        let (outpoint, utxo) = ledger.get_utxos(&address_of(&miner)).unwrap().remove(0);

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
        };
        theft.sign_input(Network::Testnet, 0, &thief).unwrap();
        let theft_id = theft.id();

        let mut block = mine(&ledger, &pow, &address_of(&thief));
        block.transactions.push(theft);
        let block = remine(block, &pow);
        ledger.add_block(block, &pow).expect("block itself is valid");

        let subsidy = block_subsidy_atoms(1);
        assert_eq!(ledger.get_balance(&address_of(&miner)).unwrap().0, subsidy);
        assert_eq!(ledger.get_balance(&address_of(&thief)).unwrap().0, subsidy); // coinbase only
        assert!(ledger.get_transaction(&theft_id).unwrap().is_none());
    }

    #[test]
    fn ledger_recovers_blocks_and_balances_after_restart() {
        let miner = key(40);
        let path = fresh_db("restart");
        let pow = test_pow();
        let tip = {
            let mut ledger = DagLedger::open(&path, None).unwrap();
            let block = mine(&ledger, &pow, &address_of(&miner));
            ledger.add_block(block, &pow).unwrap()
        };

        let reopened = DagLedger::open(&path, None).unwrap();
        assert_eq!(reopened.blocks.len(), 2);
        assert_eq!(reopened.virtual_selected_parent, tip);
        assert_eq!(reopened.tips, HashSet::from([tip]));
        assert_eq!(reopened.get_balance(&address_of(&miner)).unwrap().0, block_subsidy_atoms(1));
        assert_eq!(reopened.storage.get_block(&tip).unwrap().unwrap().hash(), tip);
    }
}
