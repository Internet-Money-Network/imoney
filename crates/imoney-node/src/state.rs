use crate::genesis::{create_devnet_genesis, create_testnet_genesis};
use crate::mempool::{Mempool, MempoolError};
use crate::storage::{
    AcceptanceData, AcceptedTx, BlockMeta, HistoryItem, HistoryRow, Storage, StorageError, TxRecord, UtxoEntry,
    WriteBatch, META_PRUNED_FLOOR, META_SINK, META_VIRTUAL_ACCEPTANCE,
};
use imoney_consensus::{work_from_bits, DaaParams, Dag, DagBlock, GhostdagData, GhostdagError, GhostdagParams};
use imoney_core::constants::{
    ATOMS_PER_IMN, BLOCKS_PER_HALVING_ERA, BLOCKS_PER_YEAR, MAX_BLOCK_BYTES, MAX_BLOCK_PARENTS, MAX_TX_BYTES,
    TARGET_TIME_PER_BLOCK_MS,
};
use imoney_core::serialize::tagged_hash;
use imoney_core::{
    Address, Block, BlockError, BlockHeader, Decode, Encode, Hash, Network, Outpoint, ScriptPublicKey, Transaction,
    TxOutput,
};
use imoney_emission::{block_subsidy_atoms, block_subsidy_imn};
use imoney_pow::{compact_to_u256, is_valid_pow, HallmarkPow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{broadcast, RwLock};

/// Selected-chain blocks pruned per database transaction.
const PRUNE_CHUNK: usize = 500;

/// Confirmations after which this node reports a payment as final, unless configured otherwise.
pub const DEFAULT_FINAL_CONFIRMATIONS: u64 = 60;
/// How long after a reorganisation the node keeps warning that the network is unsettled.
pub const NETWORK_ALERT_WINDOW_MS: u64 = 30 * 60 * 1000;

/// A selected-chain switch that undoes at least this many blocks is recorded as a warning sign.
pub const REORG_ALARM_DEPTH: usize = 3;

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
    /// Not proof of a bad block: the sender's clock, or ours, may simply be off.
    #[error("Block timestamp is too far in the future")]
    TimestampInFuture,
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
    #[error("Parent {0} is more than the finality depth behind the block's other parents")]
    ParentTooOld(Hash),
    #[error("The database holds a different network's chain. Use another --data-dir, or delete this one to start again.")]
    WrongNetwork,
}

/// Something that changed in the ledger, pushed to subscribers such as WebSocket clients.
#[derive(Clone, Debug)]
pub enum LedgerEvent {
    /// A transaction entered the mempool. Carries its outputs so listeners can match addresses.
    PendingTx { tx_id: Hash, outputs: Vec<TxOutput>, invoice_id: Option<String> },
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
    /// Blue-score depth below the tip beyond which the selected chain is never replaced.
    /// A heavier chain that forks off deeper than this is ignored.
    pub finality_depth: u64,
    /// The first block. Its hash identifies the network and seeds its proof of work.
    pub genesis: Block,
    /// The name nodes report for this network.
    pub name: &'static str,
}

impl ConsensusParams {
    /// The public test network.
    pub fn testnet() -> Self {
        Self {
            ghostdag: GhostdagParams::default(),
            daa: DaaParams::new(crate::genesis::TESTNET_GENESIS_BITS),
            coinbase_maturity: 20,
            // Thirty seconds. The difficulty rule reads timestamps over a 100-second half
            // life; in simulation a miner stamping blocks two minutes ahead made blocks come
            // about 5% too fast and doubled the swings in difficulty, and at thirty seconds
            // both effects vanish. Nodes are expected to keep their clocks set.
            max_future_ms: 30_000,
            // 12 hours of 5-second blocks
            finality_depth: 8_640,
            genesis: create_testnet_genesis(),
            name: "testnet-2",
        }
    }

    /// Private test networks and unit tests: the same rules with a trivial starting difficulty.
    pub fn devnet() -> Self {
        Self {
            daa: DaaParams::new(crate::genesis::DEVNET_GENESIS_BITS),
            genesis: create_devnet_genesis(),
            name: "devnet",
            ..Self::testnet()
        }
    }
}

/// JSON-serializable node status report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    pub network: String,
    /// Identifies the network; also the base seed of its proof of work.
    pub genesis_hash: String,
    pub total_blocks: usize,
    pub virtual_selected_parent: String,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    pub tips: Vec<String>,
    pub current_bits: String,
    pub current_block_reward_imn: f64,
    pub target_block_interval_sec: u64,
    /// Blocks below this depth can no longer be reorganised away.
    pub finality_depth: u64,
    /// True when this node keeps every block and can serve the full history to others.
    pub archival: bool,
    /// Selected-chain blocks at or below this blue score have had their transactions deleted.
    pub pruned_below_blue_score: u64,
    /// True when a heavier chain exists that this node refuses because it forks below the
    /// finality point. The network may be split; operators should investigate.
    pub finality_conflict: bool,
    /// Depth and time of the most recent selected-chain switch that undid several blocks.
    pub last_reorg_depth: Option<usize>,
    pub last_reorg_at_ms: Option<u64>,
    /// True while the network looks unsettled (a finality conflict, or a recent reorganisation).
    /// Payments are not reported as final while this is set.
    pub network_alert: bool,
    /// Confirmations after which this node reports a payment as final.
    pub final_confirmations: u64,
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

/// Blocks looked at when estimating hashrate, block time and transaction rate.
pub const STATS_WINDOW: usize = 144;

/// A payment recently carried by a block, for display.
#[derive(Clone, Debug, Serialize)]
pub struct RecentTransaction {
    pub tx_id: String,
    pub block_hash: String,
    pub amount_atoms: u64,
    pub timestamp_ms: u64,
}

/// Network-wide figures derived from the recent blocks and the current ledger.
#[derive(Clone, Debug, Serialize)]
pub struct NetworkStats {
    /// Expected number of hashes needed to mine a block at the current difficulty.
    pub difficulty: f64,
    pub difficulty_bits: String,
    /// Estimated hashes per second across all miners: work done over the window divided by its duration.
    pub hashrate_hps: f64,
    /// Average seconds between blocks over the window.
    pub average_block_time_sec: f64,
    /// Blocks the estimates are based on (up to `STATS_WINDOW`).
    pub window_blocks: usize,
    pub window_seconds: f64,
    /// Payments carried by the blocks in the window, and the resulting rate.
    pub transactions_in_window: usize,
    pub transactions_per_second: f64,
    /// Every unspent coin, including mining rewards that are still maturing.
    pub circulating_supply_atoms: u128,
    pub circulating_supply_imn: f64,
    pub block_reward_imn: f64,
    /// New coins per year at the current reward, as a percentage of the circulating supply.
    pub annual_inflation_percent: f64,
    /// DAA score at which the block reward next halves; absent once the permanent floor is reached.
    pub next_halving_daa_score: Option<u64>,
    pub blocks_until_halving: Option<u64>,
    pub days_until_halving: Option<f64>,
    pub recent_transactions: Vec<RecentTransaction>,
}

/// One payment towards an invoice, as the ledger currently sees it.
pub struct InvoicePayment {
    pub tx_id: Hash,
    /// Atoms this transaction pays to the invoice's address.
    pub amount_atoms: u64,
    /// 0 while pending; 1 once accepted, then one more per blue block added on top.
    pub confirmations: u64,
}

/// One page of blocks for a syncing peer, and the position to continue from if more remain.
pub type BlockPage = (Vec<Block>, Option<(u64, Hash)>);

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

    fn put_invoice(&mut self, accepted: &AcceptedTx) {
        if let Some(invoice_id) = &accepted.invoice_id {
            self.batch.invoice_puts.insert((invoice_id.clone(), accepted.tx_id));
        }
    }

    fn delete_invoice(&mut self, accepted: &AcceptedTx) {
        if let Some(invoice_id) = &accepted.invoice_id {
            let entry = (invoice_id.clone(), accepted.tx_id);
            self.batch.invoice_puts.remove(&entry);
            self.batch.invoice_deletes.insert(entry);
        }
    }

    /// Reverses a previously applied acceptance.
    fn undo(&mut self, acceptance: &AcceptanceData) {
        for item in &acceptance.history {
            self.batch.delete_history(acceptance.blue_score, item);
        }
        for (outpoint, _) in &acceptance.created {
            self.spend(outpoint);
        }
        for (outpoint, entry) in &acceptance.spent {
            self.create(outpoint.clone(), entry.clone());
        }
        for accepted in &acceptance.accepted {
            self.delete_record(&accepted.tx_id);
            self.delete_invoice(accepted);
        }
    }

    /// Applies an acceptance that was computed earlier against the same starting state.
    fn redo(&mut self, acceptance: &AcceptanceData) {
        for item in &acceptance.history {
            self.batch.put_history(acceptance.blue_score, item);
        }
        for (outpoint, _) in &acceptance.spent {
            self.spend(outpoint);
        }
        for (outpoint, entry) in &acceptance.created {
            self.create(outpoint.clone(), entry.clone());
        }
        for accepted in &acceptance.accepted {
            self.put_invoice(accepted);
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
    /// A heavier tip was passed over because following it would break finality.
    finality_conflict: bool,
    /// Selected-chain blocks undone to reach this state.
    reorg_depth: usize,
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
    /// When set, block bodies deeper than this blue-score depth are deleted. Headers and the
    /// current ledger are kept, so the node still validates everything new.
    pub prune_depth: Option<u64>,
    /// Selected-chain blocks at or below this blue score have been pruned.
    pub pruned_floor: u64,
    pub finality_conflict: bool,
    /// Confirmations after which this node reports a payment as final.
    pub final_confirmations: u64,
    /// Depth and local time (ms) of the last selected-chain switch of `REORG_ALARM_DEPTH` or more.
    pub last_reorg: Option<(usize, u64)>,
    /// Every block ordered by `(level, hash)`: an order in which parents precede children,
    /// used to page through the DAG when another node syncs from this one.
    level_index: BTreeSet<(u64, Hash)>,
    virtual_acceptance: AcceptanceData,
}


impl DagLedger {
    /// Opens the ledger of the network described by `params` from disk, or creates it if new.
    pub fn open_with_params(
        db_path: impl AsRef<Path>,
        mining_address: Option<Address>,
        params: ConsensusParams,
    ) -> Result<Self, StateError> {
        let storage = Storage::open(db_path)?;
        let loaded = storage.load_blocks()?;
        let genesis = params.genesis.clone();
        if !loaded.is_empty() && !loaded.iter().any(|(hash, _, _)| *hash == genesis.hash()) {
            return Err(StateError::WrongNetwork);
        }

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
            genesis_hash: genesis.hash(),
            prune_depth: None,
            pruned_floor: 0,
            finality_conflict: false,
            final_confirmations: DEFAULT_FINAL_CONFIRMATIONS,
            last_reorg: None,
            level_index: BTreeSet::new(),
            virtual_acceptance: AcceptanceData::default(),
        };

        let mut view = LedgerView::new(ledger.storage.clone());
        if loaded.is_empty() {
            // Genesis initialization
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
            if let Some(bytes) = ledger.storage.get_metadata(META_PRUNED_FLOOR)? {
                ledger.pruned_floor = bytes.try_into().map(u64::from_be_bytes).unwrap_or(0);
            }
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
        if state.finality_conflict && !self.finality_conflict {
            eprintln!("[!] A heavier chain forks below the finality point and is being ignored. The network may be split.");
        }
        self.finality_conflict = state.finality_conflict;
        if state.reorg_depth >= REORG_ALARM_DEPTH {
            eprintln!("[!] Selected chain reorganised: {} blocks replaced.", state.reorg_depth);
            self.last_reorg = Some((state.reorg_depth, chrono::Utc::now().timestamp_millis() as u64));
        }
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

                // What this transaction takes from and gives to each locking script
                let tx_id = tx.id();
                let mut moved: Vec<HistoryItem> = Vec::new();
                let mut record = |script: &ScriptPublicKey, received: u64, sent: u64| {
                    let item = match moved.iter().position(|item| &item.script == script) {
                        Some(found) => &mut moved[found],
                        None => {
                            moved.push(HistoryItem {
                                script: script.clone(),
                                id: tx_id,
                                received_atoms: 0,
                                sent_atoms: 0,
                                is_reward: false,
                                timestamp_ms: block.header.timestamp_ms,
                            });
                            moved.last_mut().expect("just pushed")
                        }
                    };
                    item.received_atoms = item.received_atoms.saturating_add(received);
                    item.sent_atoms = item.sent_atoms.saturating_add(sent);
                };

                for input in &tx.inputs {
                    let outpoint = &input.previous_outpoint;
                    if let Some(entry) = view.get(outpoint)? {
                        record(&entry.output.script_public_key, 0, entry.output.value_atoms);
                        if created_here.contains(outpoint) {
                            // Created and spent within this acceptance: never reaches the UTXO set
                            spent_here.insert(outpoint.clone());
                        } else {
                            acceptance.spent.push((outpoint.clone(), entry));
                        }
                    }
                    view.spend(outpoint);
                }

                for (idx, output) in tx.outputs.iter().enumerate() {
                    record(&output.script_public_key, output.value_atoms, 0);
                    let outpoint = Outpoint { transaction_id: tx_id, index: idx as u32 };
                    let entry = UtxoEntry { output: output.clone(), blue_score, is_coinbase: false };
                    view.create(outpoint.clone(), entry.clone());
                    created_here.insert(outpoint.clone());
                    created.push((outpoint, entry));
                }

                if let (Some(script), true) = (&tx.service, service_share > 0) {
                    record(script, service_share, 0);
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
                let accepted = AcceptedTx {
                    tx_id,
                    block_hash,
                    tx_index: index as u32,
                    invoice_id: tx.invoice_id().map(str::to_string),
                };
                view.put_invoice(&accepted);
                acceptance.accepted.push(accepted);
                for item in moved {
                    view.batch.put_history(blue_score, &item);
                    acceptance.history.push(item);
                }
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
                    let item = HistoryItem {
                        script: entry.output.script_public_key.clone(),
                        id: outpoint.transaction_id,
                        received_atoms: entry.output.value_atoms,
                        sent_atoms: 0,
                        is_reward: true,
                        timestamp_ms: block.header.timestamp_ms,
                    };
                    view.batch.put_history(blue_score, &item);
                    acceptance.history.push(item);
                    view.create(outpoint.clone(), entry.clone());
                    created.push((outpoint, entry));
                }
            }
        }

        acceptance.created = created.into_iter().filter(|(outpoint, _)| !spent_here.contains(outpoint)).collect();
        Ok(acceptance)
    }

    /// The selected-chain ancestor of `from` with the highest blue score not above `blue_score`.
    fn chain_ancestor_at_or_below(&self, from: Hash, blue_score: u64) -> Hash {
        let mut cursor = from;
        loop {
            let data = &self.dag.get(&cursor).ghostdag;
            if data.blue_score <= blue_score || data.is_genesis() {
                return cursor;
            }
            cursor = data.selected_parent;
        }
    }

    /// The block on the current selected chain, `finality_depth` below the sink, that every
    /// future selected chain must pass through.
    pub fn finality_point(&self) -> Hash {
        let sink = self.virtual_selected_parent;
        let sink_score = self.dag.get(&sink).ghostdag.blue_score;
        self.chain_ancestor_at_or_below(sink, sink_score.saturating_sub(self.params.finality_depth))
    }

    /// True when `parent` is too far behind a block whose best parent has `best_blue_score`.
    fn too_old_to_be_a_parent(&self, parent: &Hash, best_blue_score: u64) -> bool {
        self.dag.get(parent).ghostdag.blue_score + self.params.finality_depth < best_blue_score
    }

    /// Checks a new block's parent list. Besides the structural rules, no parent may be more
    /// than the finality depth behind the best one. A block that old can no longer change the
    /// ledger, and without the limit a block naming an ancient parent would make every node
    /// walk the DAG all the way back to it.
    fn check_parents(&self, parents: &[Hash]) -> Result<(), StateError> {
        // Only known parents can be measured; an unknown one is reported by the checks below
        if parents.iter().all(|p| self.dag.contains(p)) {
            let best = parents.iter().map(|p| self.dag.get(p).ghostdag.blue_score).max().unwrap_or(0);
            if let Some(old) = parents.iter().find(|p| self.too_old_to_be_a_parent(p, best)) {
                return Err(StateError::ParentTooOld(*old));
            }
        }
        Ok(self.dag.check_parents(parents)?)
    }

    /// Chooses the virtual block's parents: the sink plus as many other tips as can be merged.
    fn pick_virtual_parents(&self, sink: &Hash) -> Vec<Hash> {
        let sink_score = self.dag.get(sink).ghostdag.blue_score;
        // A tip left behind for longer than the finality depth is never merged
        let mut others: Vec<Hash> = self
            .tips
            .iter()
            .filter(|t| *t != sink && !self.too_old_to_be_a_parent(t, sink_score))
            .copied()
            .collect();
        others.sort_by_key(|h| std::cmp::Reverse(self.dag.sort_key(h)));

        let mut parents = vec![*sink];
        for tip in others {
            if parents.len() == MAX_BLOCK_PARENTS {
                break;
            }
            parents.push(tip);
            // A tip that would push the mergeset past its limit is left for a later block.
            // A tip heavier than the sink (one refused for finality) is never merged: it would
            // become the virtual block's selected parent and take the ledger with it.
            let keeps_sink_selected = self.dag.ghostdag(&parents).is_ok_and(|data| data.selected_parent == *sink);
            if !keeps_sink_selected {
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

        // Only tips whose selected chain runs through the finality point may become the sink
        let finality_point = self.finality_point();
        let finality_score = self.dag.get(&finality_point).ghostdag.blue_score;
        let eligible: Vec<Hash> = self
            .tips
            .iter()
            .filter(|tip| self.chain_ancestor_at_or_below(**tip, finality_score) == finality_point)
            .copied()
            .collect();
        let heaviest = self.dag.best_of(self.tips.iter()).expect("the DAG always has a tip");
        let new_sink = self.dag.best_of(eligible.iter()).unwrap_or(old_sink);
        let finality_conflict = heaviest != new_sink;

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
            finality_conflict,
            reorg_depth: to_undo.len(),
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
        pow_engine: &HallmarkPow,
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
        self.check_parents(&header.parents)?;
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
            return Err(StateError::TimestampInFuture);
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

        // Drop pending transactions that were accepted or can no longer be spent. A block can
        // only invalidate a pending transaction by removing a coin it spends, so looking up the
        // coins this block removed is enough; the rest of the pool is untouched.
        for outpoint in &view.batch.utxo_deletes {
            if !view.batch.utxo_puts.contains_key(outpoint) {
                self.mempool.remove_spender(outpoint);
            }
        }

        let _ = self.events.send(LedgerEvent::BlockAdded { hash: block_hash, blue_score: self.virtual_blue_score });

        // The block is committed; a pruning failure must not turn that into an error
        if let Err(e) = self.prune() {
            eprintln!("[-] Pruning failed: {}", e);
        }

        Ok(block_hash)
    }


    /// Turns on pruning and prunes whatever is already deep enough. The depth must exceed the
    /// finality depth, so nothing that a reorganisation could still need is ever deleted.
    pub fn enable_pruning(&mut self, depth: u64) -> Result<usize, StateError> {
        if depth <= self.params.finality_depth {
            return Err(StateError::Header(format!(
                "Prune depth {} must be greater than the finality depth {}",
                depth, self.params.finality_depth
            )));
        }
        self.prune_depth = Some(depth);
        self.prune()
    }

    /// Deletes the transactions of blocks buried deeper than the prune depth, along with the
    /// stored ledger changes and transaction lookups that go with them. Returns the number of
    /// selected-chain blocks processed.
    ///
    /// Only blocks merged by a selected-chain block below the finality point are touched. Every
    /// chain the node could still switch to passes through that point, so those blocks are in
    /// its past and their contents are never needed again. Headers are kept.
    pub fn prune(&mut self) -> Result<usize, StateError> {
        let Some(depth) = self.prune_depth else {
            return Ok(0);
        };
        let sink = self.virtual_selected_parent;
        let target = self.dag.get(&sink).ghostdag.blue_score.saturating_sub(depth);
        if target <= self.pruned_floor {
            return Ok(0);
        }

        // Selected-chain blocks with floor < blue score <= target, highest first
        let mut chain = Vec::new();
        let mut cursor = self.chain_ancestor_at_or_below(sink, target);
        loop {
            let data = &self.dag.get(&cursor).ghostdag;
            if data.blue_score <= self.pruned_floor || data.is_genesis() {
                break;
            }
            chain.push(cursor);
            cursor = data.selected_parent;
        }

        // Lowest first, so the floor only ever rises past blocks that are fully pruned
        for chunk in chain.rchunks(PRUNE_CHUNK) {
            let mut batch = WriteBatch::default();
            let mut floor = self.pruned_floor;
            for hash in chunk {
                let data = &self.dag.get(hash).ghostdag;
                floor = floor.max(data.blue_score);
                for merged in data.mergeset_blues.iter().chain(&data.mergeset_reds) {
                    // Genesis keeps its body: it is how a network is identified
                    let Some(header) = self.blocks.get(merged).filter(|h| !h.parents.is_empty()) else {
                        continue;
                    };
                    let stub = Block { header: header.clone(), transactions: Vec::new() };
                    batch.blocks.push((*merged, stub.to_bytes()));
                }
                if let Some(acceptance) = self.storage.get_acceptance(hash)? {
                    for accepted in &acceptance.accepted {
                        batch.record_deletes.insert(accepted.tx_id);
                        if let Some(invoice_id) = &accepted.invoice_id {
                            batch.invoice_deletes.insert((invoice_id.clone(), accepted.tx_id));
                        }
                    }
                    for item in &acceptance.history {
                        batch.delete_history(acceptance.blue_score, item);
                    }
                    batch.acceptance_deletes.push(*hash);
                }
            }
            batch.metadata.push((META_PRUNED_FLOOR, floor.to_be_bytes().to_vec()));
            self.storage.commit(&batch)?;
            self.pruned_floor = floor;
        }
        Ok(chain.len())
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
        let invoice_id = tx.invoice_id().map(str::to_string);
        self.mempool.insert(tx, fee)?;
        let _ = self.events.send(LedgerEvent::PendingTx { tx_id, outputs, invoice_id });
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
        self.check_parents(&parents)?;
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

    /// Queries the UTXOs of an address, each with whether it can be spent right now and how
    /// many confirmations it has (1 once accepted, one more per blue block added on top).
    pub fn get_utxos_with_status(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput, bool, u64)>, StorageError> {
        let maturity = self.params.coinbase_maturity;
        Ok(self
            .storage
            .get_utxos(address)?
            .into_iter()
            .map(|(outpoint, entry)| {
                let spendable = !entry.is_coinbase || entry.blue_score + maturity <= self.virtual_blue_score;
                let confirmations = self.virtual_blue_score.saturating_sub(entry.blue_score) + 1;
                (outpoint, entry.output, spendable, confirmations)
            })
            .collect())
    }

    /// What an address received and sent, newest first, each row with its confirmations.
    /// `before` continues after a row already read (see `HistoryRow::position`).
    pub fn address_history(
        &self,
        address: &Address,
        limit: usize,
        before: Option<&[u8]>,
    ) -> Result<Vec<(HistoryRow, u64)>, StorageError> {
        Ok(self
            .storage
            .get_history(address, limit, before)?
            .into_iter()
            .map(|row| {
                let confirmations = self.virtual_blue_score.saturating_sub(row.accepting_blue_score) + 1;
                (row, confirmations)
            })
            .collect())
    }

    /// True while the network looks unsettled: this node is refusing a heavier chain, or the
    /// selected chain was reorganised several blocks deep within the last half hour.
    pub fn network_alert(&self) -> bool {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        self.finality_conflict
            || self.last_reorg.is_some_and(|(_, at)| now_ms.saturating_sub(at) < NETWORK_ALERT_WINDOW_MS)
    }

    /// Every payment that names `invoice_id`, pending or accepted, with how much each pays `address`.
    pub fn invoice_payments(&self, invoice_id: &str, address: &Address) -> Result<Vec<InvoicePayment>, StorageError> {
        let script = ScriptPublicKey::pay_to_address(address);
        let paid_to_address = |tx: &Transaction| -> u64 {
            tx.outputs
                .iter()
                .filter(|o| o.script_public_key == script)
                .fold(0u64, |sum, o| sum.saturating_add(o.value_atoms))
        };

        let mut payments: Vec<InvoicePayment> = self
            .mempool
            .by_invoice(invoice_id)
            .map(|tx| InvoicePayment { tx_id: tx.id(), amount_atoms: paid_to_address(tx), confirmations: 0 })
            .collect();
        for tx_id in self.storage.get_invoice_tx_ids(invoice_id)? {
            if let Some(info) = self.get_transaction(&tx_id)? {
                payments.push(InvoicePayment {
                    tx_id,
                    amount_atoms: paid_to_address(&info.tx),
                    confirmations: info.confirmations,
                });
            }
        }
        payments.retain(|payment| payment.amount_atoms > 0);
        Ok(payments)
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
    ) -> Result<BlockPage, StorageError> {
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

    /// Computes network-wide figures from the most recent blocks. Reads those blocks from disk,
    /// so callers should cache the result per tip.
    pub fn network_stats(&self) -> Result<NetworkStats, StorageError> {
        let window = self.dag.block_window(&self.virtual_ghostdag, STATS_WINDOW);
        let mut total_work: u128 = 0;
        let mut oldest: Option<(u64, u128)> = None;
        let mut newest_time = 0u64;
        let mut transactions_in_window = 0usize;
        let mut recent_transactions = Vec::new();

        // The window is newest first
        for hash in &window {
            let block = self.dag.get(hash);
            total_work = total_work.saturating_add(block.work);
            newest_time = newest_time.max(block.timestamp_ms);
            if oldest.is_none_or(|(time, _)| block.timestamp_ms < time) {
                oldest = Some((block.timestamp_ms, block.work));
            }
            if let Some(body) = self.storage.get_block(hash)? {
                for tx in body.transactions.iter().skip(1) {
                    transactions_in_window += 1;
                    if recent_transactions.len() < 10 {
                        recent_transactions.push(RecentTransaction {
                            tx_id: tx.id().to_hex(),
                            block_hash: hash.to_hex(),
                            amount_atoms: tx.outputs.iter().fold(0u64, |sum, o| sum.saturating_add(o.value_atoms)),
                            timestamp_ms: block.timestamp_ms,
                        });
                    }
                }
            }
        }

        // The window spans the time after its oldest block, so that block's work is not part of it
        let (oldest_time, oldest_work) = oldest.unwrap_or((newest_time, 0));
        let window_seconds = newest_time.saturating_sub(oldest_time) as f64 / 1000.0;
        let intervals = window.len().saturating_sub(1);
        let (hashrate_hps, average_block_time_sec, transactions_per_second) = if window_seconds > 0.0 && intervals > 0 {
            (
                (total_work - oldest_work) as f64 / window_seconds,
                window_seconds / intervals as f64,
                transactions_in_window as f64 / window_seconds,
            )
        } else {
            (0.0, 0.0, 0.0)
        };

        let circulating_supply_atoms = self.storage.total_utxo_atoms()?;
        let circulating_supply_imn = circulating_supply_atoms as f64 / ATOMS_PER_IMN as f64;
        let block_reward_imn = block_subsidy_imn(self.virtual_daa_score);
        let annual_inflation_percent = if circulating_supply_imn > 0.0 {
            100.0 * block_reward_imn * BLOCKS_PER_YEAR as f64 / circulating_supply_imn
        } else {
            0.0
        };

        // The reward halves at the end of each of the first four eras, then stays at the floor
        let era = self.virtual_daa_score / BLOCKS_PER_HALVING_ERA;
        let next_halving_daa_score = (era < 4).then(|| (era + 1) * BLOCKS_PER_HALVING_ERA);
        let blocks_until_halving = next_halving_daa_score.map(|at| at - self.virtual_daa_score);
        let days_until_halving =
            blocks_until_halving.map(|blocks| blocks as f64 * TARGET_TIME_PER_BLOCK_MS as f64 / 86_400_000.0);

        Ok(NetworkStats {
            difficulty: work_from_bits(self.difficulty_bits) as f64,
            difficulty_bits: format!("0x{:08x}", self.difficulty_bits),
            hashrate_hps,
            average_block_time_sec,
            window_blocks: window.len(),
            window_seconds,
            transactions_in_window,
            transactions_per_second,
            circulating_supply_atoms,
            circulating_supply_imn,
            block_reward_imn,
            annual_inflation_percent,
            next_halving_daa_score,
            blocks_until_halving,
            days_until_halving,
            recent_transactions,
        })
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
                Network::Testnet => self.params.name,
            }
            .to_string(),
            genesis_hash: self.genesis_hash.to_hex(),
            total_blocks: self.blocks.len(),
            virtual_selected_parent: self.virtual_selected_parent.to_hex(),
            virtual_blue_score: self.virtual_blue_score,
            virtual_daa_score: self.virtual_daa_score,
            tips: self.tips.iter().map(|t| t.to_hex()).collect(),
            current_bits: format!("0x{:08x}", self.difficulty_bits),
            current_block_reward_imn: block_subsidy_imn(self.virtual_daa_score),
            target_block_interval_sec: TARGET_TIME_PER_BLOCK_MS / 1000,
            finality_depth: self.params.finality_depth,
            archival: self.prune_depth.is_none(),
            pruned_below_blue_score: self.pruned_floor,
            finality_conflict: self.finality_conflict,
            last_reorg_depth: self.last_reorg.map(|(depth, _)| depth),
            last_reorg_at_ms: self.last_reorg.map(|(_, at)| at),
            network_alert: self.network_alert(),
            final_confirmations: self.final_confirmations,
            mining_address: self.mining_address.as_ref().map(|a| a.to_string()),
            service_address: self.service_address.as_ref().map(|a| a.to_string()),
            mempool_size: self.mempool.len(),
        }
    }
}



pub type SharedLedger = Arc<RwLock<DagLedger>>;

#[cfg(test)]
mod tests;
