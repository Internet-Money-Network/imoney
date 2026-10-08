use crate::daa::work_from_bits;
use crate::ghostdag::{GhostdagData, GhostdagParams};
use imoney_core::Hash;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Reads a block that is no longer held in memory.
pub type BlockLoader = Box<dyn Fn(&Hash) -> Option<DagBlock> + Send + Sync>;

/// Old blocks read back from disk that are kept at hand before starting over.
const RECALLED_BLOCKS: usize = 4_096;

/// What consensus needs to know about a block already in the DAG.
#[derive(Clone, Debug)]
pub struct DagBlock {
    pub parents: Vec<Hash>,
    /// Longest path to genesis. Strictly greater than the level of every ancestor.
    pub level: u64,
    pub timestamp_ms: u64,
    pub bits: u32,
    pub daa_score: u64,
    /// Expected number of hashes to mine this block.
    pub work: u128,
    pub ghostdag: GhostdagData,
}

/// Index of the block DAG used by the consensus rules.
///
/// Recent blocks are held in memory. A node may `forget` old ones and give the index a loader
/// that reads them back when a rule reaches that far, so the rules see the same DAG either
/// way and memory stays level as the chain grows.
pub struct Dag {
    pub params: GhostdagParams,
    blocks: HashMap<Hash, Arc<DagBlock>>,
    loader: Option<BlockLoader>,
    recalled: Mutex<HashMap<Hash, Arc<DagBlock>>>,
}

impl Dag {
    pub fn new(params: GhostdagParams) -> Self {
        Self {
            params,
            blocks: HashMap::new(),
            loader: None,
            recalled: Mutex::new(HashMap::new()),
        }
    }

    /// Sets where blocks that were forgotten are read from.
    pub fn set_loader(&mut self, loader: BlockLoader) {
        self.loader = Some(loader);
    }

    /// Number of blocks held in memory.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn contains(&self, hash: &Hash) -> bool {
        self.try_get(hash).is_some()
    }

    /// True when the block is held in memory.
    pub fn holds(&self, hash: &Hash) -> bool {
        self.blocks.contains_key(hash)
    }

    /// Returns a block that is known to be in the DAG.
    pub fn get(&self, hash: &Hash) -> Arc<DagBlock> {
        self.try_get(hash).expect("block must be in the DAG")
    }

    pub fn try_get(&self, hash: &Hash) -> Option<Arc<DagBlock>> {
        if let Some(block) = self.blocks.get(hash) {
            return Some(block.clone());
        }
        let loader = self.loader.as_ref()?;
        let mut recalled = self.recalled.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(block) = recalled.get(hash) {
            return Some(block.clone());
        }
        let block = Arc::new(loader(hash)?);
        if recalled.len() >= RECALLED_BLOCKS {
            recalled.clear();
        }
        recalled.insert(*hash, block.clone());
        Some(block)
    }

    /// Drops a block from memory. It must be readable through the loader from now on.
    pub fn forget(&mut self, hash: &Hash) {
        self.blocks.remove(hash);
    }

    pub fn hashes(&self) -> impl Iterator<Item = &Hash> {
        self.blocks.keys()
    }

    /// Inserts the genesis block.
    pub fn insert_genesis(&mut self, hash: Hash, timestamp_ms: u64, bits: u32) {
        self.blocks.insert(
            hash,
            Arc::new(DagBlock {
                parents: Vec::new(),
                level: 0,
                timestamp_ms,
                bits,
                daa_score: 0,
                work: work_from_bits(bits),
                ghostdag: GhostdagData::default(),
            }),
        );
    }

    /// Inserts a validated block whose parents are all present.
    pub fn insert(&mut self, hash: Hash, parents: Vec<Hash>, timestamp_ms: u64, bits: u32, daa_score: u64, ghostdag: GhostdagData) {
        let level = 1 + parents.iter().map(|p| self.get(p).level).max().unwrap_or(0);
        self.blocks.insert(
            hash,
            Arc::new(DagBlock {
                parents,
                level,
                timestamp_ms,
                bits,
                daa_score,
                work: work_from_bits(bits),
                ghostdag,
            }),
        );
    }

    /// Removes a block that was inserted but could not be committed.
    pub fn remove(&mut self, hash: &Hash) {
        self.blocks.remove(hash);
    }

    /// Restores a block loaded from storage.
    pub fn restore(&mut self, hash: Hash, block: DagBlock) {
        self.blocks.insert(hash, Arc::new(block));
    }

    /// Total order used to pick the selected parent and to order a mergeset:
    /// more blue work wins, ties broken by hash.
    pub fn sort_key(&self, hash: &Hash) -> (u128, Hash) {
        (self.get(hash).ghostdag.blue_work, *hash)
    }

    /// True when `ancestor` equals `descendant` or is in its past.
    ///
    /// Walks backwards from `descendant`, never descending below the ancestor's level.
    pub fn is_ancestor(&self, ancestor: &Hash, descendant: &Hash) -> bool {
        if ancestor == descendant {
            return true;
        }
        let floor = self.get(ancestor).level;
        let mut visited: HashSet<Hash> = HashSet::new();
        let mut stack = vec![*descendant];
        while let Some(current) = stack.pop() {
            for parent in &self.get(&current).parents {
                if parent == ancestor {
                    return true;
                }
                if self.get(parent).level > floor && visited.insert(*parent) {
                    stack.push(*parent);
                }
            }
        }
        false
    }

    /// The tip that the selected chain ends in: the one with the most blue work.
    pub fn best_of<'a>(&self, hashes: impl IntoIterator<Item = &'a Hash>) -> Option<Hash> {
        hashes.into_iter().max_by_key(|h| self.sort_key(h)).copied()
    }
}
