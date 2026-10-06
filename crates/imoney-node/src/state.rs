use crate::genesis::create_testnet_genesis;
use imoney_consensus::ghostdag::{order_ghostdag_parents, GhostdagParams};
use imoney_core::constants::{MAX_BLOCK_PARENTS, TARGET_TIME_PER_BLOCK_MS};
use imoney_core::{BlockHeader, Hash};
use imoney_emission::block_subsidy_im;
use imoney_pow::{compact_to_u256, is_valid_pow, MoneyPrinterPow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// In-memory BlockDAG Ledger for Internet Money Testnet.
pub struct DagLedger {
    pub blocks: HashMap<Hash, BlockHeader>,
    pub tips: HashSet<Hash>,
    pub blue_scores: HashMap<Hash, u64>,
    pub virtual_selected_parent: Hash,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    pub difficulty_bits: u32,
    ghostdag_params: GhostdagParams,
}

impl DagLedger {
    pub fn new() -> Self {
        let genesis = create_testnet_genesis();
        let genesis_pre_hash = genesis.pre_pow_hash().unwrap();
        let genesis_hash = genesis_pre_hash; // At genesis, hash identifies the block

        let mut blocks = HashMap::new();
        let mut tips = HashSet::new();
        let mut blue_scores = HashMap::new();

        blocks.insert(genesis_hash, genesis.clone());
        tips.insert(genesis_hash);
        blue_scores.insert(genesis_hash, 0);

        Self {
            blocks,
            tips,
            blue_scores,
            virtual_selected_parent: genesis_hash,
            virtual_blue_score: 0,
            virtual_daa_score: 0,
            difficulty_bits: genesis.bits,
            ghostdag_params: GhostdagParams::default(),
        }
    }

    /// Validates and inserts a newly mined block header into the BlockDAG.
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

        // Update DAG tips:
        // Any parent of this new block is no longer a tip (it now has a child)
        for parent in &header.parents {
            self.tips.remove(parent);
        }
        self.tips.insert(block_hash);

        // Save block header and scores
        self.blue_scores.insert(block_hash, ghostdag.blue_score);
        self.blocks.insert(block_hash, header.clone());

        // Update virtual tip if this block's blue score exceeds current virtual tip
        if ghostdag.blue_score > self.virtual_blue_score {
            self.virtual_selected_parent = block_hash;
            self.virtual_blue_score = ghostdag.blue_score;
            self.virtual_daa_score = header.daa_score;
            self.difficulty_bits = header.bits;
        }

        Ok(block_hash)
    }

    /// Generates a candidate block template for miners based on current tips.
    pub fn get_mining_template(&self) -> MiningTemplate {
        let mut parents: Vec<Hash> = self.tips.iter().copied().collect();
        parents.sort(); // Deterministic ordering
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

    /// Generates node diagnostic overview.
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
        }
    }
}

pub type SharedLedger = Arc<RwLock<DagLedger>>;
