use crate::genesis::create_testnet_genesis;
use crate::storage::{NodeMeta, Storage, StorageError};
use imoney_consensus::ghostdag::{order_ghostdag_parents, GhostdagParams};
use imoney_core::constants::{MAX_BLOCK_PARENTS, SOMPI_PER_IM, TARGET_TIME_PER_BLOCK_MS};
use imoney_core::{Address, BlockHeader, Hash, Outpoint, TxOutput};
use imoney_emission::{block_subsidy_atoms, block_subsidy_im};
use imoney_pow::{compact_to_u256, is_valid_pow, MoneyPrinterPow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

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
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
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
    pub current_block_reward_im: f64,
    pub target_block_interval_sec: u64,
    pub mining_address: Option<String>,
}

/// Candidate block mining template.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MiningTemplate {
    pub version: u16,
    pub parents: Vec<Hash>,
    pub timestamp_ms: u64,
    pub bits: u32,
    pub target_hex: String,
    pub daa_score: u64,
    pub blue_score: u64,
    pub pre_pow_hash: Hash,
    pub reward_im: f64,
}

/// BlockDAG Ledger for Internet Money backed by ACID on-disk storage.
pub struct DagLedger {
    pub storage: Storage,
    pub blocks: HashMap<Hash, BlockHeader>,
    pub tips: HashSet<Hash>,
    pub blue_scores: HashMap<Hash, u64>,
    pub virtual_selected_parent: Hash,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    pub difficulty_bits: u32,
    pub mining_address: Option<Address>,
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
            let genesis_hash = genesis.pre_pow_hash().map_err(|e| StateError::Header(e.to_string()))?;

            let meta = NodeMeta {
                virtual_selected_parent: genesis_hash,
                virtual_blue_score: 0,
                virtual_daa_score: 0,
                difficulty_bits: genesis.bits,
            };

            storage.save_block(&genesis_hash, &genesis, 0, Some(&meta))?;

            let mut blocks = HashMap::new();
            let mut tips = HashSet::new();
            let mut blue_scores = HashMap::new();

            blocks.insert(genesis_hash, genesis.clone());
            tips.insert(genesis_hash);
            blue_scores.insert(genesis_hash, 0);

            Ok(Self {
                storage,
                blocks,
                tips,
                blue_scores,
                virtual_selected_parent: genesis_hash,
                virtual_blue_score: 0,
                virtual_daa_score: 0,
                difficulty_bits: genesis.bits,
                mining_address,
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
                blocks,
                tips,
                blue_scores,
                virtual_selected_parent: meta.virtual_selected_parent,
                virtual_blue_score: meta.virtual_blue_score,
                virtual_daa_score: meta.virtual_daa_score,
                difficulty_bits: meta.difficulty_bits,
                mining_address,
                ghostdag_params: GhostdagParams::default(),
            })
        }
    }

    /// Validates and inserts a newly mined block header into the BlockDAG and persistent storage.
    pub fn add_block(
        &mut self,
        header: BlockHeader,
        pow_engine: &MoneyPrinterPow,
    ) -> Result<Hash, StateError> {
        let pre_pow_hash = header.pre_pow_hash().map_err(|e| StateError::Header(e.to_string()))?;
        let block_hash = pow_engine.calculate_hash(&pre_pow_hash, header.nonce);

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
        if !is_valid_pow(&block_hash, header.bits) {
            return Err(StateError::InvalidPoW(block_hash));
        }

        // Run GHOSTDAG parent ordering
        let ghostdag = order_ghostdag_parents(&header.parents, &self.blue_scores, &self.ghostdag_params);

        // Update tips
        for parent in &header.parents {
            self.tips.remove(parent);
        }
        self.tips.insert(block_hash);

        // Update in-memory state
        self.blue_scores.insert(block_hash, ghostdag.blue_score);
        self.blocks.insert(block_hash, header.clone());

        // Update virtual tip if higher
        let is_new_selected = ghostdag.blue_score > self.virtual_blue_score;
        if is_new_selected {
            self.virtual_selected_parent = block_hash;
            self.virtual_blue_score = ghostdag.blue_score;
            self.virtual_daa_score = header.daa_score;
            self.difficulty_bits = header.bits;
        }

        // Persist to disk atomically
        let meta = if is_new_selected {
            Some(NodeMeta {
                virtual_selected_parent: self.virtual_selected_parent,
                virtual_blue_score: self.virtual_blue_score,
                virtual_daa_score: self.virtual_daa_score,
                difficulty_bits: self.difficulty_bits,
            })
        } else {
            None
        };

        self.storage.save_block(&block_hash, &header, ghostdag.blue_score, meta.as_ref())?;

        // Process Coinbase Reward (5 IM) to the configured mining address
        if let Some(ref addr) = self.mining_address {
            let reward_atoms = block_subsidy_atoms(header.daa_score);
            let outpoint = Outpoint {
                transaction_id: block_hash,
                index: 0,
            };
            let output = TxOutput {
                value_atoms: reward_atoms,
                script_public_key: addr.hash.0.to_vec(),
            };
            self.storage.add_utxo(&outpoint, &output)?;
        }

        Ok(block_hash)
    }

    /// Generates candidate block template for miners.
    pub fn get_mining_template(&self) -> MiningTemplate {
        let mut parents: Vec<Hash> = self.tips.iter().copied().collect();
        parents.sort();
        if parents.len() > MAX_BLOCK_PARENTS {
            parents.truncate(MAX_BLOCK_PARENTS);
        }

        let ghostdag = order_ghostdag_parents(&parents, &self.blue_scores, &self.ghostdag_params);
        let daa_score = self.virtual_daa_score + 1;
        let blue_score = ghostdag.blue_score;
        let bits = self.difficulty_bits;

        let dummy_header = BlockHeader {
            version: 1,
            parents: parents.clone(),
            hash_merkle_root: Hash::ZERO,
            accepted_id_merkle_root: Hash::ZERO,
            utxo_commitment: Hash::ZERO,
            timestamp_ms: chrono::Utc::now().timestamp_millis() as u64,
            bits,
            nonce: 0,
            daa_score,
            blue_score,
            blue_work: blue_score as u128 * 1000,
        };

        let pre_pow_hash = dummy_header.pre_pow_hash().unwrap_or(Hash::ZERO);
        let target = compact_to_u256(bits);

        MiningTemplate {
            version: 1,
            parents,
            timestamp_ms: dummy_header.timestamp_ms,
            bits,
            target_hex: hex::encode(target),
            daa_score,
            blue_score,
            pre_pow_hash,
            reward_im: block_subsidy_im(daa_score),
        }
    }

    /// Queries live spendable balance for an address in whole IM and atomic units.
    pub fn get_balance(&self, address: &Address) -> Result<(u64, f64), StorageError> {
        let atoms = self.storage.get_balance(address)?;
        let coins = (atoms as f64) / (SOMPI_PER_IM as f64);
        Ok((atoms, coins))
    }

    /// Queries list of UTXOs for an address.
    pub fn get_utxos(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput)>, StorageError> {
        self.storage.get_utxos(address)
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
            current_block_reward_im: block_subsidy_im(self.virtual_daa_score),
            target_block_interval_sec: TARGET_TIME_PER_BLOCK_MS / 1000,
            mining_address: self.mining_address.as_ref().map(|a| a.to_string()),
        }
    }
}

pub type SharedLedger = Arc<RwLock<DagLedger>>;
