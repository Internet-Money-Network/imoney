use crate::genesis::create_testnet_genesis;
use crate::mempool::{Mempool, MempoolError};
use crate::storage::{
    AcceptanceData, AcceptedTx, BlockMeta, Storage, StorageError, TxRecord, UtxoEntry, WriteBatch, META_SINK,
    META_VIRTUAL_ACCEPTANCE,
};
use imoney_consensus::{work_from_bits, DaaParams, Dag, DagBlock, GhostdagData, GhostdagError, GhostdagParams};
use imoney_core::constants::{ATOMS_PER_IMN, MAX_BLOCK_BYTES, MAX_BLOCK_PARENTS, MAX_TX_BYTES, TARGET_TIME_PER_BLOCK_MS};
use imoney_core::serialize::tagged_hash;
use imoney_core::{
    Address, Block, BlockError, BlockHeader, Decode, Encode, Hash, Network, Outpoint, ScriptPublicKey, Transaction,
    TxOutput,
};
use imoney_emission::{block_subsidy_atoms, block_subsidy_imn};
use imoney_pow::{compact_to_u256, is_valid_pow, MoneyPrinterPow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{broadcast, RwLock};

/// Bytes of a block template kept free for the header and coinbase.
const TEMPLATE_RESERVED_BYTES: usize = 2_000;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("Block already exists in DAG: {0}")]
    BlockAlreadyExists(Hash),
    #[error("Block exceeds maximum parent count: {0} > {1}")]
    TooManyParents(usize, usize),
    #[error("Invalid block parents: {0}")]
    Ghostdag(#[from] GhostdagError),
    #[error("Invalid proof of work for block: {0}")]
    InvalidPoW(Hash),
    #[error("Header error: {0}")]
    Header(String),
    #[error("Invalid block: {0}")]
    Block(#[from] BlockError),
    #[error("Invalid coinbase: {0}")]
    Coinbase(String),
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("Transaction error: {0}")]
    Transaction(String),
    #[error("Transaction not admitted: {0}")]
    Mempool(#[from] MempoolError),
}

/// Something that changed in the ledger, pushed to subscribers such as WebSocket clients.
#[derive(Clone, Debug)]
pub enum LedgerEvent {
    /// A transaction entered the mempool. Carries its outputs so listeners can match addresses.
    PendingTx { tx_id: Hash, outputs: Vec<TxOutput> },
    /// A block was added and the ledger moved to a new virtual state.
    BlockAdded { hash: Hash, blue_score: u64 },
}

/// The consensus rules of a network.
#[derive(Clone, Debug)]
pub struct ConsensusParams {
    pub ghostdag: GhostdagParams,
    pub daa: DaaParams,
    /// Blue-score depth before a block reward may be spent.
    pub coinbase_maturity: u64,
    /// How far ahead of the local clock a block timestamp may be.
    pub max_future_ms: u64,
}

impl ConsensusParams {
    pub fn testnet() -> Self {
        Self {
            ghostdag: GhostdagParams::default(),
            daa: DaaParams::new(crate::genesis::TESTNET_GENESIS_BITS),
            coinbase_maturity: 20,
            max_future_ms: 120_000,
        }
    }
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
    /// Name this as a payment's service address to give this node half of the fee.
    pub service_address: Option<String>,
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

/// A transaction as the ledger currently sees it.
pub struct TxInfo {
    pub tx: Transaction,
    /// The block carrying the transaction, once it has been accepted.
    pub block_hash: Option<Hash>,
    /// 0 while pending; grows as blue blocks are added after acceptance.
    pub confirmations: u64,
}

/// The outpoint of the reward the ledger creates for a blue block.
pub fn reward_outpoint(block_hash: &Hash) -> Outpoint {
    Outpoint {
        transaction_id: tagged_hash("IMN 2026 block reward", &[&block_hash.0]),
        index: 0,
    }
}

/// Uncommitted ledger changes layered over the database.
struct LedgerView {
    storage: Storage,
    batch: WriteBatch,
}

impl LedgerView {
    fn new(storage: Storage) -> Self {
        Self { storage, batch: WriteBatch::default() }
    }

    fn get(&self, outpoint: &Outpoint) -> Result<Option<UtxoEntry>, StorageError> {
        if let Some(entry) = self.batch.utxo_puts.get(outpoint) {
            return Ok(Some(entry.clone()));
        }
        if self.batch.utxo_deletes.contains(outpoint) {
            return Ok(None);
        }
        self.storage.get_utxo(outpoint)
    }

    fn create(&mut self, outpoint: Outpoint, entry: UtxoEntry) {
        self.batch.utxo_puts.insert(outpoint, entry);
    }

    fn spend(&mut self, outpoint: &Outpoint) {
        self.batch.utxo_puts.remove(outpoint);
        self.batch.utxo_deletes.insert(outpoint.clone());
    }

    fn put_record(&mut self, tx_id: Hash, record: TxRecord) {
        self.batch.record_puts.insert(tx_id, record);
    }

    fn delete_record(&mut self, tx_id: &Hash) {
        self.batch.record_puts.remove(tx_id);
        self.batch.record_deletes.insert(*tx_id);
    }

    /// Reverses a previously applied acceptance.
    fn undo(&mut self, acceptance: &AcceptanceData) {
        for (outpoint, _) in &acceptance.created {
            self.spend(outpoint);
        }
        for (outpoint, entry) in &acceptance.spent {
            self.create(outpoint.clone(), entry.clone());
        }
        for accepted in &acceptance.accepted {
            self.delete_record(&accepted.tx_id);
        }
    }

    /// Applies an acceptance that was computed earlier against the same starting state.
    fn redo(&mut self, acceptance: &AcceptanceData) {
        for (outpoint, _) in &acceptance.spent {
            self.spend(outpoint);
        }
        for (outpoint, entry) in &acceptance.created {
            self.create(outpoint.clone(), entry.clone());
        }
        for accepted in &acceptance.accepted {
            self.put_record(
                accepted.tx_id,
                TxRecord {
                    block_hash: accepted.block_hash,
                    tx_index: accepted.tx_index,
                    accepting_blue_score: acceptance.blue_score,
                },
            );
        }
    }
}

/// The virtual block: an imaginary block on top of all tips whose state is "the ledger now".
struct VirtualState {
    sink: Hash,
    parents: Vec<Hash>,
    ghostdag: GhostdagData,
    acceptance: AcceptanceData,
    daa_score: u64,
    bits: u32,
}

/// BlockDAG Ledger for Internet Money backed by ACID on-disk storage.
pub struct DagLedger {
    pub storage: Storage,
    pub network: Network,
    pub params: ConsensusParams,
    pub dag: Dag,
    pub blocks: HashMap<Hash, BlockHeader>,
    pub tips: HashSet<Hash>,
    /// The tip the selected chain ends in.
    pub virtual_selected_parent: Hash,
    pub virtual_parents: Vec<Hash>,
    pub virtual_ghostdag: GhostdagData,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    /// Difficulty the next block must have.
    pub difficulty_bits: u32,
    /// Default payout address for block templates.
    pub mining_address: Option<Address>,
    /// Address this node asks wallets to name as the service address of payments it serves.
    pub service_address: Option<Address>,
    pub mempool: Mempool,
    /// Ledger change notifications. Sending never blocks; slow subscribers skip events.
    pub events: broadcast::Sender<LedgerEvent>,
    pub genesis_hash: Hash,
    /// Every block ordered by `(level, hash)`: an order in which parents precede children,
    /// used to page through the DAG when another node syncs from this one.
    level_index: BTreeSet<(u64, Hash)>,
    virtual_acceptance: AcceptanceData,
}


impl DagLedger {
    /// Opens the persistent testnet ledger from disk or creates it if new.
    pub fn open(db_path: impl AsRef<Path>, mining_address: Option<Address>) -> Result<Self, StateError> {
        Self::open_with_params(db_path, mining_address, ConsensusParams::testnet())
    }

    pub fn open_with_params(
        db_path: impl AsRef<Path>,
        mining_address: Option<Address>,
        params: ConsensusParams,
    ) -> Result<Self, StateError> {
        let storage = Storage::open(db_path)?;
        let loaded = storage.load_blocks()?;

        let mut ledger = Self {
            storage,
            network: Network::Testnet,
            dag: Dag::new(params.ghostdag.clone()),
            params,
            blocks: HashMap::new(),
            tips: HashSet::new(),
            virtual_selected_parent: Hash::ZERO,
            virtual_parents: Vec::new(),
            virtual_ghostdag: GhostdagData::default(),
            virtual_blue_score: 0,
            virtual_daa_score: 0,
            difficulty_bits: 0,
            service_address: mining_address.clone(),
            mining_address,
            mempool: Mempool::default(),
            events: broadcast::channel(1024).0,
            genesis_hash: create_testnet_genesis().hash(),
            level_index: BTreeSet::new(),
            virtual_acceptance: AcceptanceData::default(),
        };

        let mut view = LedgerView::new(ledger.storage.clone());
        if loaded.is_empty() {
            // Genesis initialization
            let genesis = create_testnet_genesis();
            let genesis_hash = genesis.hash();
            ledger
                .dag
                .insert_genesis(genesis_hash, genesis.header.timestamp_ms, genesis.header.bits);
            let meta = BlockMeta { level: 0, ghostdag: GhostdagData::default() };
            view.batch.blocks.push((genesis_hash, genesis.to_bytes()));
            view.batch.block_meta.push((genesis_hash, meta.to_bytes()));
            ledger.blocks.insert(genesis_hash, genesis.header.clone());
            ledger.level_index.insert((0, genesis_hash));
            ledger.tips.insert(genesis_hash);
            ledger.virtual_selected_parent = genesis_hash;

            let virtual_state = ledger.resolve_virtual(&mut view, Some((&genesis_hash, &genesis)))?;
            ledger.storage.commit(&view.batch)?;
            ledger.set_virtual(virtual_state);
        } else {
            // Recover from disk
            let mut non_tips = HashSet::new();
            for (hash, header, meta) in loaded {
                for p in &header.parents {
                    non_tips.insert(*p);
                }
                ledger.level_index.insert((meta.level, hash));
                ledger.dag.restore(
                    hash,
                    DagBlock {
                        parents: header.parents.clone(),
                        level: meta.level,
                        timestamp_ms: header.timestamp_ms,
                        bits: header.bits,
                        daa_score: header.daa_score,
                        work: work_from_bits(header.bits),
                        ghostdag: meta.ghostdag,
                    },
                );
                ledger.blocks.insert(hash, header);
            }
            // Tips: all blocks that are not parents of any other block
            ledger.tips = ledger.blocks.keys().filter(|h| !non_tips.contains(h)).copied().collect();

            let sink_bytes = ledger.storage.get_metadata(META_SINK)?.expect("Metadata must exist if blocks exist");
            ledger.virtual_selected_parent = <Hash as Decode>::from_bytes(&sink_bytes).map_err(StorageError::from)?;
            if let Some(bytes) = ledger.storage.get_metadata(META_VIRTUAL_ACCEPTANCE)? {
                ledger.virtual_acceptance = AcceptanceData::from_bytes(&bytes).map_err(StorageError::from)?;
            }

            // The stored UTXO set already reflects the virtual state; recomputing it is a no-op
            // on disk and rebuilds the in-memory view of the virtual block.
            let virtual_state = ledger.resolve_virtual(&mut view, None)?;
            ledger.storage.commit(&view.batch)?;
            ledger.set_virtual(virtual_state);
        }

        Ok(ledger)
    }

    fn set_virtual(&mut self, state: VirtualState) {
        self.virtual_selected_parent = state.sink;
        self.virtual_parents = state.parents;
        self.virtual_blue_score = state.ghostdag.blue_score;
        self.virtual_ghostdag = state.ghostdag;
        self.virtual_daa_score = state.daa_score;
        self.difficulty_bits = state.bits;
        self.virtual_acceptance = state.acceptance;
    }

    fn load_block(&self, hash: &Hash, pending: Option<(&Hash, &Block)>) -> Result<Block, StorageError> {
        if let Some((pending_hash, block)) = pending {
            if pending_hash == hash {
                return Ok(block.clone());
            }
        }
        self.storage.get_block(hash)?.ok_or(StorageError::MissingBlock(*hash))
    }

    /// Checks that a transaction can spend its inputs, given a view of the UTXO set and the
    /// blue score at which it would be accepted. Returns the fee it pays.
    /// Failures are reported as `StateError::Transaction`.
    fn check_spend(
        &self,
        tx: &Transaction,
        lookup: impl Fn(&Outpoint) -> Result<Option<UtxoEntry>, StorageError>,
        spending_blue_score: u64,
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

            if utxo.is_coinbase && utxo.blue_score + self.params.coinbase_maturity > spending_blue_score {
                return Err(StateError::Transaction(format!(
                    "Block reward is immature: spendable at blue score {}",
                    utxo.blue_score + self.params.coinbase_maturity
                )));
            }

            // Verify signature against public key and address hash
            tx.verify_input(self.network, i, &utxo.output.script_public_key)
                .map_err(|e| StateError::Transaction(format!("Signature check failed on input {}: {}", i, e)))?;

            total_input_atoms = total_input_atoms
                .checked_add(utxo.output.value_atoms)
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

    /// Applies the transactions of every block merged by a block with the given GHOSTDAG data,
    /// in consensus order, and returns the exact change made.
    ///
    /// A transaction that cannot be spent at this point (already spent by an earlier block in the
    /// order, bad signature, immature reward) is skipped. A transaction that names a service
    /// script pays it half of its fee. Each blue block's miner receives the subsidy plus the
    /// rest of the fees of that block's accepted transactions; red blocks earn nothing.
    fn accept_mergeset(
        &self,
        view: &mut LedgerView,
        ghostdag: &GhostdagData,
        pending: Option<(&Hash, &Block)>,
    ) -> Result<AcceptanceData, StateError> {
        let blue_score = ghostdag.blue_score;
        let mut acceptance = AcceptanceData { blue_score, ..Default::default() };
        let mut created: Vec<(Outpoint, UtxoEntry)> = Vec::new();
        let mut spent_here: HashSet<Outpoint> = HashSet::new();
        let mut created_here: HashSet<Outpoint> = HashSet::new();

        for (block_hash, is_blue) in ghostdag.ordered_mergeset(&self.dag) {
            let block = self.load_block(&block_hash, pending)?;
            let mut fees: u64 = 0;

            for (index, tx) in block.transactions.iter().enumerate().skip(1) {
                let fee = match self.check_spend(tx, |outpoint| view.get(outpoint), blue_score) {
                    Ok(fee) => fee,
                    Err(StateError::Transaction(_)) => continue, // Not spendable here: skip it
                    Err(e) => return Err(e),
                };
                let service_share = tx.service_share(fee);
                fees = fees.saturating_add(fee - service_share);

                for input in &tx.inputs {
                    let outpoint = &input.previous_outpoint;
                    if created_here.contains(outpoint) {
                        // Created and spent within this acceptance: never reaches the UTXO set
                        spent_here.insert(outpoint.clone());
                    } else if let Some(entry) = view.get(outpoint)? {
                        acceptance.spent.push((outpoint.clone(), entry));
                    }
                    view.spend(outpoint);
                }

                let tx_id = tx.id();
                for (idx, output) in tx.outputs.iter().enumerate() {
                    let outpoint = Outpoint { transaction_id: tx_id, index: idx as u32 };
                    let entry = UtxoEntry { output: output.clone(), blue_score, is_coinbase: false };
                    view.create(outpoint.clone(), entry.clone());
                    created_here.insert(outpoint.clone());
                    created.push((outpoint, entry));
                }

                if let (Some(script), true) = (&tx.service, service_share > 0) {
                    let outpoint = tx.service_outpoint();
                    let entry = UtxoEntry {
                        output: TxOutput { value_atoms: service_share, script_public_key: script.clone() },
                        blue_score,
                        is_coinbase: false,
                    };
                    view.create(outpoint.clone(), entry.clone());
                    created_here.insert(outpoint.clone());
                    created.push((outpoint, entry));
                }

                view.put_record(
                    tx_id,
                    TxRecord { block_hash, tx_index: index as u32, accepting_blue_score: blue_score },
                );
                acceptance.accepted.push(AcceptedTx { tx_id, block_hash, tx_index: index as u32 });
            }

            if is_blue {
                if let Some(payout) = block.transactions[0].outputs.first() {
                    let outpoint = reward_outpoint(&block_hash);
                    let entry = UtxoEntry {
                        output: TxOutput {
                            value_atoms: payout.value_atoms.saturating_add(fees),
                            script_public_key: payout.script_public_key.clone(),
                        },
                        blue_score,
                        is_coinbase: true,
                    };
                    view.create(outpoint.clone(), entry.clone());
                    created.push((outpoint, entry));
                }
            }
        }

        acceptance.created = created.into_iter().filter(|(outpoint, _)| !spent_here.contains(outpoint)).collect();
        Ok(acceptance)
    }

    /// Chooses the virtual block's parents: the sink plus as many other tips as can be merged.
    fn pick_virtual_parents(&self, sink: &Hash) -> Vec<Hash> {
        let mut others: Vec<Hash> = self.tips.iter().filter(|t| *t != sink).copied().collect();
        others.sort_by_key(|h| std::cmp::Reverse(self.dag.sort_key(h)));

        let mut parents = vec![*sink];
        for tip in others {
            if parents.len() == MAX_BLOCK_PARENTS {
                break;
            }
            parents.push(tip);
            // A tip that would push the mergeset past its limit is left for a later block
            if self.dag.ghostdag(&parents).is_err() {
                parents.pop();
            }
        }
        parents
    }

    /// Moves the UTXO set in `view` from the current virtual state to the virtual state of the
    /// current tips: undoes the old virtual block, switches the selected chain if a heavier one
    /// exists (undoing and applying whole blocks), then applies the new virtual block.
    fn resolve_virtual(
        &self,
        view: &mut LedgerView,
        pending: Option<(&Hash, &Block)>,
    ) -> Result<VirtualState, StateError> {
        let old_sink = self.virtual_selected_parent;
        let new_sink = self.dag.best_of(self.tips.iter()).expect("the DAG always has a tip");

        view.undo(&self.virtual_acceptance);

        // Walk both selected chains back to their common block
        let mut to_undo = Vec::new();
        let mut to_apply = Vec::new();
        let (mut old_cursor, mut new_cursor) = (old_sink, new_sink);
        while old_cursor != new_cursor {
            let old_score = self.dag.get(&old_cursor).ghostdag.blue_score;
            let new_score = self.dag.get(&new_cursor).ghostdag.blue_score;
            if old_score >= new_score {
                to_undo.push(old_cursor);
                old_cursor = self.dag.get(&old_cursor).ghostdag.selected_parent;
            }
            if new_score >= old_score {
                to_apply.push(new_cursor);
                new_cursor = self.dag.get(&new_cursor).ghostdag.selected_parent;
            }
        }

        for hash in &to_undo {
            let acceptance = self.storage.get_acceptance(hash)?.ok_or(StorageError::MissingBlock(*hash))?;
            view.undo(&acceptance);
        }
        for hash in to_apply.iter().rev() {
            match self.storage.get_acceptance(hash)? {
                Some(acceptance) => view.redo(&acceptance),
                None => {
                    let acceptance = self.accept_mergeset(view, &self.dag.get(hash).ghostdag, pending)?;
                    view.batch.acceptance.push((*hash, acceptance.to_bytes()));
                }
            }
        }

        let parents = self.pick_virtual_parents(&new_sink);
        let ghostdag = self.dag.ghostdag(&parents)?;
        let acceptance = self.accept_mergeset(view, &ghostdag, pending)?;

        view.batch.metadata.push((META_SINK, new_sink.0.to_vec()));
        view.batch.metadata.push((META_VIRTUAL_ACCEPTANCE, acceptance.to_bytes()));

        Ok(VirtualState {
            sink: new_sink,
            daa_score: self.dag.daa_score(&ghostdag),
            bits: self.dag.expected_bits(&ghostdag, &self.params.daa),
            parents,
            ghostdag,
            acceptance,
        })
    }

    /// Validates a newly mined block and adds it to the BlockDAG and persistent storage.
    ///
    /// Every consensus field in the header is recomputed and compared, never trusted. The ledger
    /// is then moved to the state implied by the new set of tips, in one database transaction.
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
        if header.version != 1 {
            return Err(StateError::Header(format!("Unsupported version {}", header.version)));
        }

        // Validate parents and work out where the block sits in the DAG
        if header.parents.len() > MAX_BLOCK_PARENTS {
            return Err(StateError::TooManyParents(header.parents.len(), MAX_BLOCK_PARENTS));
        }
        self.dag.check_parents(&header.parents)?;
        let ghostdag = self.dag.ghostdag(&header.parents)?;

        // Every consensus field must equal what this node computes
        let expected_daa_score = self.dag.daa_score(&ghostdag);
        let expected_bits = self.dag.expected_bits(&ghostdag, &self.params.daa);
        if header.blue_score != ghostdag.blue_score {
            return Err(StateError::Header(format!(
                "Blue score {} should be {}",
                header.blue_score, ghostdag.blue_score
            )));
        }
        if header.blue_work != ghostdag.blue_work {
            return Err(StateError::Header(format!(
                "Blue work {} should be {}",
                header.blue_work, ghostdag.blue_work
            )));
        }
        if header.daa_score != expected_daa_score {
            return Err(StateError::Header(format!(
                "DAA score {} should be {}",
                header.daa_score, expected_daa_score
            )));
        }
        if header.bits != expected_bits {
            return Err(StateError::Header(format!(
                "Difficulty bits 0x{:08x} should be 0x{:08x}",
                header.bits, expected_bits
            )));
        }
        if header.accepted_id_merkle_root != Hash::ZERO || header.utxo_commitment != Hash::ZERO {
            return Err(StateError::Header("Reserved commitment fields must be zero".to_string()));
        }

        let past_median_time = self.dag.past_median_time(&ghostdag);
        if header.timestamp_ms <= past_median_time {
            return Err(StateError::Header(format!(
                "Timestamp {} is not after the past median time {}",
                header.timestamp_ms, past_median_time
            )));
        }
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        if header.timestamp_ms > now_ms + self.params.max_future_ms {
            return Err(StateError::Header("Timestamp is too far in the future".to_string()));
        }

        // Validate Proof of Work
        let pre_pow_hash = header.pre_pow_hash().map_err(|e| StateError::Header(e.to_string()))?;
        let pow_hash = pow_engine.calculate_hash(&pre_pow_hash, header.nonce);
        if !is_valid_pow(&pow_hash, header.bits) {
            return Err(StateError::InvalidPoW(block_hash));
        }

        // Validate the body: coinbase first, sizes, merkle root
        block.validate_structure()?;

        // The coinbase names the miner's payout: at most one output, worth at most the subsidy.
        // The ledger adds the block's fees when the block is merged as blue.
        let coinbase = &block.transactions[0];
        if coinbase.payload.len() < 8 || coinbase.payload[..8] != header.daa_score.to_be_bytes() {
            return Err(StateError::Coinbase("payload must start with the block's DAA score".to_string()));
        }
        if coinbase.outputs.len() > 1 {
            return Err(StateError::Coinbase("more than one output".to_string()));
        }
        let subsidy = block_subsidy_atoms(header.daa_score);
        if coinbase.outputs.iter().any(|o| o.value_atoms > subsidy) {
            return Err(StateError::Coinbase(format!("pays more than the {}-atom subsidy", subsidy)));
        }

        // Add the block to the in-memory DAG, then move the ledger to the new virtual state
        let removed_tips: Vec<Hash> = header.parents.iter().filter(|p| self.tips.contains(*p)).copied().collect();
        self.dag.insert(
            block_hash,
            header.parents.clone(),
            header.timestamp_ms,
            header.bits,
            header.daa_score,
            ghostdag.clone(),
        );
        for parent in &removed_tips {
            self.tips.remove(parent);
        }
        self.tips.insert(block_hash);

        let mut view = LedgerView::new(self.storage.clone());
        let meta = BlockMeta { level: self.dag.get(&block_hash).level, ghostdag };
        view.batch.blocks.push((block_hash, block.to_bytes()));
        view.batch.block_meta.push((block_hash, meta.to_bytes()));

        let result = self
            .resolve_virtual(&mut view, Some((&block_hash, &block)))
            .and_then(|virtual_state| {
                self.storage.commit(&view.batch)?;
                Ok(virtual_state)
            });
        let virtual_state = match result {
            Ok(virtual_state) => virtual_state,
            Err(e) => {
                // Nothing was written: take the block back out of memory
                self.dag.remove(&block_hash);
                self.tips.remove(&block_hash);
                self.tips.extend(removed_tips);
                return Err(e);
            }
        };
        self.blocks.insert(block_hash, block.header.clone());
        self.level_index.insert((self.dag.get(&block_hash).level, block_hash));
        self.set_virtual(virtual_state);

        // Drop pending transactions that were accepted or can no longer be spent
        let mut mempool = std::mem::take(&mut self.mempool);
        mempool.retain(|tx| {
            self.check_spend(tx, |outpoint| self.storage.get_utxo(outpoint), self.virtual_blue_score).is_ok()
        });
        self.mempool = mempool;

        let _ = self.events.send(LedgerEvent::BlockAdded { hash: block_hash, blue_score: self.virtual_blue_score });

        Ok(block_hash)
    }


    /// Validates and admits a signed transaction into the mempool.
    pub fn broadcast_transaction(&mut self, tx: Transaction) -> Result<Hash, StateError> {
        let tx_id = tx.id();
        if self.mempool.contains(&tx_id) {
            return Ok(tx_id);
        }

        let fee = self.check_spend(&tx, |outpoint| self.storage.get_utxo(outpoint), self.virtual_blue_score)?;

        // The mempool applies relay policy: minimum fee, no conflicting spends, size cap
        let outputs = tx.outputs.clone();
        self.mempool.insert(tx, fee)?;
        let _ = self.events.send(LedgerEvent::PendingTx { tx_id, outputs });
        Ok(tx_id)
    }

    /// Builds an unmined block on the given parents with every consensus field filled in.
    pub fn build_block(
        &self,
        parents: &[Hash],
        payout: Option<&Address>,
        transactions: Vec<Transaction>,
    ) -> Result<Block, StateError> {
        let mut parents = parents.to_vec();
        parents.sort();
        self.dag.check_parents(&parents)?;
        let ghostdag = self.dag.ghostdag(&parents)?;
        let daa_score = self.dag.daa_score(&ghostdag);

        let coinbase_outputs = payout
            .map(|address| TxOutput {
                value_atoms: block_subsidy_atoms(daa_score),
                script_public_key: ScriptPublicKey::pay_to_address(address),
            })
            .into_iter()
            .collect();
        let mut body = vec![Transaction::coinbase(daa_score, coinbase_outputs, &[])];
        body.extend(transactions);

        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let header = BlockHeader {
            version: 1,
            parents,
            hash_merkle_root: Block::compute_merkle_root(&body),
            accepted_id_merkle_root: Hash::ZERO,
            utxo_commitment: Hash::ZERO,
            timestamp_ms: now_ms.max(self.dag.past_median_time(&ghostdag) + 1),
            bits: self.dag.expected_bits(&ghostdag, &self.params.daa),
            nonce: 0,
            daa_score,
            blue_score: ghostdag.blue_score,
            blue_work: ghostdag.blue_work,
        };
        Ok(Block { header, transactions: body })
    }

    /// Generates a candidate block for miners on top of the current tips. The coinbase pays
    /// `payout`, or the node's configured mining address when none is given.
    pub fn get_mining_template(&self, payout: Option<&Address>) -> MiningTemplate {
        // Fill the block with the best-paying pending transactions
        let transactions = self.mempool.select_for_block(MAX_BLOCK_BYTES - TEMPLATE_RESERVED_BYTES);

        let block = self
            .build_block(&self.virtual_parents, payout.or(self.mining_address.as_ref()), transactions)
            .expect("the virtual parents always form a valid block");
        let pre_pow_hash = block.header.pre_pow_hash().unwrap_or(Hash::ZERO);

        MiningTemplate {
            target_hex: hex::encode(compact_to_u256(block.header.bits)),
            pre_pow_hash,
            reward_imn: block_subsidy_imn(block.header.daa_score),
            block,
        }
    }

    /// Queries the balance of an address in whole IMN and atomic units, including rewards
    /// that are not yet mature.
    pub fn get_balance(&self, address: &Address) -> Result<(u64, f64), StorageError> {
        let atoms = self.storage.get_balance(address)?;
        let coins = (atoms as f64) / (ATOMS_PER_IMN as f64);
        Ok((atoms, coins))
    }

    /// Queries list of UTXOs for an address.
    pub fn get_utxos(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput)>, StorageError> {
        Ok(self
            .storage
            .get_utxos(address)?
            .into_iter()
            .map(|(outpoint, entry)| (outpoint, entry.output))
            .collect())
    }

    /// Queries the UTXOs of an address that can be spent right now (excludes immature rewards).
    pub fn get_spendable_utxos(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput)>, StorageError> {
        let maturity = self.params.coinbase_maturity;
        Ok(self
            .storage
            .get_utxos(address)?
            .into_iter()
            .filter(|(_, entry)| !entry.is_coinbase || entry.blue_score + maturity <= self.virtual_blue_score)
            .map(|(outpoint, entry)| (outpoint, entry.output))
            .collect())
    }

    /// Queries a transaction by ID, checking the pending mempool first, then accepted transactions.
    pub fn get_transaction(&self, tx_id: &Hash) -> Result<Option<TxInfo>, StorageError> {
        if let Some(tx) = self.mempool.get(tx_id) {
            return Ok(Some(TxInfo { tx: tx.clone(), block_hash: None, confirmations: 0 }));
        }
        let Some(record) = self.storage.get_tx_record(tx_id)? else {
            return Ok(None);
        };
        let block = self
            .storage
            .get_block(&record.block_hash)?
            .ok_or(StorageError::MissingBlock(record.block_hash))?;
        let Some(tx) = block.transactions.get(record.tx_index as usize) else {
            return Ok(None);
        };
        Ok(Some(TxInfo {
            tx: tx.clone(),
            block_hash: Some(record.block_hash),
            confirmations: self.virtual_blue_score.saturating_sub(record.accepting_blue_score) + 1,
        }))
    }

    /// Selected-chain block hashes from the tip back to genesis, dense near the tip and
    /// exponentially sparser further back. A peer uses it to find the newest block we share.
    pub fn locator(&self) -> Vec<Hash> {
        let mut locator = Vec::new();
        let mut cursor = self.virtual_selected_parent;
        let mut step = 1usize;
        loop {
            locator.push(cursor);
            if cursor == self.genesis_hash {
                return locator;
            }
            for _ in 0..step {
                let parent = self.dag.get(&cursor).ghostdag.selected_parent;
                if parent == Hash::ZERO {
                    break;
                }
                cursor = parent;
            }
            if locator.len() >= 10 {
                step *= 2;
            }
        }
    }

    /// The level of the first locator entry this node has, or 0 when it has none of them.
    pub fn sync_start_level(&self, locator: &[Hash]) -> u64 {
        locator
            .iter()
            .find_map(|hash| self.dag.try_get(hash))
            .map_or(0, |block| block.level)
    }

    /// Blocks after `cursor` in `(level, hash)` order, up to the given limits.
    /// Also returns the cursor for the next call, or `None` when nothing is left.
    pub fn blocks_after(
        &self,
        cursor: (u64, Hash),
        max_blocks: usize,
        max_bytes: usize,
    ) -> Result<(Vec<Block>, Option<(u64, Hash)>), StorageError> {
        let mut blocks = Vec::new();
        let mut bytes = 0usize;
        let mut last = None;
        for position in self.level_index.range((Bound::Excluded(cursor), Bound::Unbounded)) {
            if blocks.len() >= max_blocks || bytes >= max_bytes {
                // More remain: resume after the last block sent
                return Ok((blocks, last));
            }
            let block = self.storage.get_block(&position.1)?.ok_or(StorageError::MissingBlock(position.1))?;
            bytes += block.to_bytes().len();
            blocks.push(block);
            last = Some(*position);
        }
        Ok((blocks, None))
    }

    /// The most recent blocks by DAA score, newest first.
    pub fn recent_blocks(&self, limit: usize) -> Vec<(Hash, &BlockHeader)> {
        let mut blocks: Vec<(Hash, &BlockHeader)> = self.blocks.iter().map(|(hash, header)| (*hash, header)).collect();
        blocks.sort_by_key(|(hash, header)| (std::cmp::Reverse(header.daa_score), *hash));
        blocks.truncate(limit);
        blocks
    }

    /// Node diagnostic overview.
    pub fn get_info(&self) -> NodeInfo {
        NodeInfo {
            network: match self.network {
                Network::Mainnet => "mainnet",
                Network::Testnet => "testnet-1",
            }
            .to_string(),
            total_blocks: self.blocks.len(),
            virtual_selected_parent: self.virtual_selected_parent.to_hex(),
            virtual_blue_score: self.virtual_blue_score,
            virtual_daa_score: self.virtual_daa_score,
            tips: self.tips.iter().map(|t| t.to_hex()).collect(),
            current_bits: format!("0x{:08x}", self.difficulty_bits),
            current_block_reward_imn: block_subsidy_imn(self.virtual_daa_score),
            target_block_interval_sec: TARGET_TIME_PER_BLOCK_MS / 1000,
            mining_address: self.mining_address.as_ref().map(|a| a.to_string()),
            service_address: self.service_address.as_ref().map(|a| a.to_string()),
            mempool_size: self.mempool.len(),
        }
    }
}



pub type SharedLedger = Arc<RwLock<DagLedger>>;

#[cfg(test)]
mod tests;
