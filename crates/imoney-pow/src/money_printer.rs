//! The Money Printer proof-of-work function.
//!
//! Each hash makes `NUM_MEMORY_ACCESSES` pseudo-random reads from a large dataset. Every dataset
//! item can be computed on its own from a small cache, so a miner holds the full dataset in
//! memory for speed while a node verifies a block from the cache alone. The dataset changes
//! every epoch, so work on one epoch's dataset cannot be reused for the next.

use crate::target::is_valid_pow;
use imoney_core::Hash;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Number of sequential memory access iterations per hash.
pub const NUM_MEMORY_ACCESSES: usize = 32;

/// Number of cache items combined into one dataset item.
pub const DATASET_PARENTS: u64 = 16;

/// Passes of data-dependent mixing over the cache when it is built.
const CACHE_ROUNDS: usize = 2;

/// Epochs whose cache or dataset are kept in memory at once.
const EPOCHS_KEPT: usize = 2;

type Item = [u64; 8];

/// Sizes of the proof-of-work memory structures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowParams {
    /// 64-byte items in the verification cache.
    pub cache_items: usize,
    /// 64-byte items in the full mining dataset.
    pub dataset_items: usize,
    /// Blocks (by DAA score) before the cache and dataset change.
    pub epoch_blocks: u64,
}

impl PowParams {
    /// Testnet and local development: 64 KB cache, 4 MB dataset, weekly epochs.
    pub const fn testnet() -> Self {
        Self { cache_items: 1 << 10, dataset_items: 1 << 16, epoch_blocks: 120_960 }
    }

    /// Mainnet target: 16 MB cache, 4.3 GB dataset, weekly epochs.
    pub const fn mainnet() -> Self {
        Self { cache_items: 1 << 18, dataset_items: 1 << 26, epoch_blocks: 120_960 }
    }

    pub fn epoch_of(&self, daa_score: u64) -> u64 {
        daa_score / self.epoch_blocks
    }
}

/// Whether to hold the full dataset in memory or compute items from the cache on demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowMode {
    /// Cache only. Enough to verify blocks; slow for mining.
    Light,
    /// Full dataset. Needed to mine at full speed.
    Full,
}

fn item_bytes(item: &Item) -> [u8; 64] {
    let mut bytes = [0u8; 64];
    for (chunk, word) in bytes.chunks_exact_mut(8).zip(item) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Blake3 in extendable-output mode: 64 bytes read as eight little-endian words.
fn expand(parts: &[&[u8]]) -> Item {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part);
    }
    let mut bytes = [0u8; 64];
    hasher.finalize_xof().fill(&mut bytes);
    let mut item = [0u64; 8];
    for (word, chunk) in item.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    item
}

/// FNV-style combine: cheap, and not invertible without knowing both inputs.
fn fnv(a: u64, b: u64) -> u64 {
    a.wrapping_mul(0x0000_0100_0000_01b3) ^ b
}

/// Builds the cache: a hash chain from the seed, then passes in which every item is rewritten
/// from its neighbour and an item chosen by its own contents. It has to be built in order.
fn build_cache(seed: &Hash, items: usize) -> Vec<Item> {
    let mut cache: Vec<Item> = Vec::with_capacity(items);
    cache.push(expand(&[b"IMN money printer cache", &seed.0]));
    for i in 1..items {
        cache.push(expand(&[&item_bytes(&cache[i - 1])]));
    }
    for _ in 0..CACHE_ROUNDS {
        for i in 0..items {
            let previous = cache[(i + items - 1) % items];
            let chosen = cache[(cache[i][0] % items as u64) as usize];
            let mut mixed = [0u64; 8];
            for j in 0..8 {
                mixed[j] = previous[j] ^ chosen[j];
            }
            cache[i] = expand(&[&item_bytes(&mixed)]);
        }
    }
    cache
}

/// Computes one dataset item from the cache alone.
fn dataset_item(cache: &[Item], index: u64) -> Item {
    let cache_len = cache.len() as u64;
    let mut mix = cache[(index % cache_len) as usize];
    mix[0] ^= index;
    mix = expand(&[&item_bytes(&mix)]);
    for parent in 0..DATASET_PARENTS {
        let chosen = &cache[(fnv(index ^ parent, mix[(parent % 8) as usize]) % cache_len) as usize];
        for j in 0..8 {
            mix[j] = fnv(mix[j], chosen[j]);
        }
    }
    expand(&[&item_bytes(&mix)])
}

/// The seed of an epoch's cache: derived from the network's base seed and the epoch number.
fn epoch_seed(base_seed: &Hash, epoch: u64) -> Hash {
    let mut hasher = blake3::Hasher::new_derive_key("IMN 2026 proof-of-work epoch seed");
    hasher.update(&base_seed.0);
    hasher.update(&epoch.to_be_bytes());
    Hash(*hasher.finalize().as_bytes())
}

/// Money Printer memory for one epoch: the cache, and the full dataset when mining.
pub struct MoneyPrinterContext {
    cache: Vec<Item>,
    dataset: Option<Vec<Item>>,
    dataset_items: u64,
}

impl MoneyPrinterContext {
    /// Builds the memory for the epoch with the given seed.
    pub fn new(seed: &Hash, params: &PowParams, mode: PowMode) -> Self {
        let cache = build_cache(seed, params.cache_items);
        let dataset = match mode {
            PowMode::Light => None,
            PowMode::Full => Some(
                (0..params.dataset_items as u64)
                    .into_par_iter()
                    .map(|index| dataset_item(&cache, index))
                    .collect(),
            ),
        };
        Self { cache, dataset, dataset_items: params.dataset_items as u64 }
    }

    /// Number of items in the dataset.
    pub fn item_count(&self) -> usize {
        self.dataset_items as usize
    }

    fn item(&self, index: u64) -> Item {
        match &self.dataset {
            Some(dataset) => dataset[index as usize],
            None => dataset_item(&self.cache, index),
        }
    }

    /// Computes the 32-byte Money Printer hash for a given header pre-hash and nonce.
    pub fn hash(&self, pre_pow_hash: &Hash, nonce: u64) -> Hash {
        // Stage 1: expand the header hash and nonce into a 64-byte mix
        let seed = expand(&[b"IMN money printer hash", &pre_pow_hash.0, &nonce.to_le_bytes()]);
        let mut mix = seed;

        // Stage 2: memory-hard pseudo-random access into the dataset. Each read position depends
        // on everything read so far, so the reads cannot be prepared ahead or done in parallel.
        const ROTATIONS: [u32; 8] = [13, 29, 7, 41, 19, 53, 31, 11];
        for i in 0..NUM_MEMORY_ACCESSES as u64 {
            let index = (mix[(i % 8) as usize] ^ i.wrapping_mul(0x9e37_79b9_7f4a_7c15)) % self.dataset_items;
            let item = self.item(index);
            for j in 0..8 {
                mix[j] = (mix[j] ^ item[j]).rotate_left(ROTATIONS[j]).wrapping_mul(0xff51_afd7_ed55_8ccd);
            }
            // Spread each lane into its neighbour so every word depends on the whole item
            for j in 0..8 {
                mix[j] ^= mix[(j + 1) % 8] >> 29;
            }
        }

        // Stage 3: Final cryptographic compression
        let final_hash = blake3::hash(&[item_bytes(&seed), item_bytes(&mix)].concat());
        Hash(*final_hash.as_bytes())
    }
}

/// The Money Printer Proof-of-Work engine. Builds and keeps the memory for each epoch it is asked about.
pub struct MoneyPrinterPow {
    params: PowParams,
    base_seed: Hash,
    mode: PowMode,
    epochs: Mutex<HashMap<u64, Arc<MoneyPrinterContext>>>,
}

impl MoneyPrinterPow {
    /// `base_seed` ties the proof of work to one network; nodes use their genesis block hash.
    pub fn new(params: PowParams, base_seed: Hash, mode: PowMode) -> Self {
        Self { params, base_seed, mode, epochs: Mutex::new(HashMap::new()) }
    }

    pub fn params(&self) -> &PowParams {
        &self.params
    }

    /// The memory for the epoch containing `daa_score`, built on first use.
    pub fn context_for(&self, daa_score: u64) -> Arc<MoneyPrinterContext> {
        let epoch = self.params.epoch_of(daa_score);
        let mut epochs = self.epochs.lock().unwrap();
        if let Some(context) = epochs.get(&epoch) {
            return context.clone();
        }
        let context = Arc::new(MoneyPrinterContext::new(&epoch_seed(&self.base_seed, epoch), &self.params, self.mode));
        // Keep only the most recent epochs
        while epochs.len() >= EPOCHS_KEPT {
            let oldest = *epochs.keys().min().unwrap();
            epochs.remove(&oldest);
        }
        epochs.insert(epoch, context.clone());
        context
    }

    /// Computes the proof-of-work hash of a block with the given DAA score.
    pub fn calculate_hash(&self, pre_pow_hash: &Hash, nonce: u64, daa_score: u64) -> Hash {
        self.context_for(daa_score).hash(pre_pow_hash, nonce)
    }

    /// Validates whether a given block pre_pow_hash and nonce meet the target bits.
    pub fn verify(&self, pre_pow_hash: &Hash, nonce: u64, bits: u32, daa_score: u64) -> bool {
        is_valid_pow(&self.calculate_hash(pre_pow_hash, nonce, daa_score), bits)
    }

    /// Parallel multi-threaded CPU miner loop.
    pub fn mine(
        &self,
        pre_pow_hash: &Hash,
        bits: u32,
        start_nonce: u64,
        max_iters: u64,
        stop_signal: Arc<AtomicBool>,
        daa_score: u64,
    ) -> Option<(u64, Hash)> {
        let context = self.context_for(daa_score);
        let threads = rayon::current_num_threads();
        let chunk_size = (max_iters / (threads as u64).max(1)).max(1);
        let found_nonce = Arc::new(AtomicU64::new(u64::MAX));

        (0..threads).into_par_iter().for_each(|t| {
            let thread_start = start_nonce.wrapping_add(t as u64 * chunk_size);

            for offset in 0..chunk_size {
                if stop_signal.load(Ordering::Relaxed) || found_nonce.load(Ordering::Relaxed) != u64::MAX {
                    break;
                }
                let n = thread_start.wrapping_add(offset);
                let hash = context.hash(pre_pow_hash, n);
                if is_valid_pow(&hash, bits) {
                    found_nonce.store(n, Ordering::SeqCst);
                    stop_signal.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });

        let n = found_nonce.load(Ordering::SeqCst);
        if n != u64::MAX {
            let hash = context.hash(pre_pow_hash, n);
            Some((n, hash))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: PowParams = PowParams { cache_items: 64, dataset_items: 1024, epoch_blocks: 100 };

    fn engine(mode: PowMode) -> MoneyPrinterPow {
        MoneyPrinterPow::new(SMALL, Hash([7u8; 32]), mode)
    }

    #[test]
    fn light_verification_matches_full_dataset_mining() {
        let (light, full) = (engine(PowMode::Light), engine(PowMode::Full));
        let header = Hash([1u8; 32]);
        for nonce in 0..200 {
            assert_eq!(light.calculate_hash(&header, nonce, 5), full.calculate_hash(&header, nonce, 5));
        }

        // A nonce found with the full dataset verifies from the cache alone
        let bits = 0x2000ffff;
        let (nonce, hash) = full.mine(&header, bits, 0, 1_000_000, Arc::new(AtomicBool::new(false)), 5).unwrap();
        assert!(is_valid_pow(&hash, bits));
        assert!(light.verify(&header, nonce, bits, 5));
        assert!(!light.verify(&header, nonce.wrapping_add(1), 0x1d00ffff, 5));
    }

    #[test]
    fn hash_depends_on_header_nonce_epoch_and_network() {
        let pow = engine(PowMode::Light);
        let base = pow.calculate_hash(&Hash([1u8; 32]), 9, 5);

        assert_eq!(base, pow.calculate_hash(&Hash([1u8; 32]), 9, 5));
        assert_ne!(base, pow.calculate_hash(&Hash([2u8; 32]), 9, 5));
        assert_ne!(base, pow.calculate_hash(&Hash([1u8; 32]), 10, 5));
        // Same epoch: the DAA score itself does not enter the hash
        assert_eq!(base, pow.calculate_hash(&Hash([1u8; 32]), 9, 99));
        // Next epoch: different dataset
        assert_ne!(base, pow.calculate_hash(&Hash([1u8; 32]), 9, 100));
        // Another network's base seed: different dataset
        let other_network = MoneyPrinterPow::new(SMALL, Hash([8u8; 32]), PowMode::Light);
        assert_ne!(base, other_network.calculate_hash(&Hash([1u8; 32]), 9, 5));
    }

    #[test]
    fn hash_matches_fixed_vector() {
        // Pins the algorithm: any change to it must change this value deliberately
        let hash = engine(PowMode::Light).calculate_hash(&Hash([1u8; 32]), 42, 0);
        assert_eq!(hash.to_hex(), FIXED_VECTOR);
    }

    #[test]
    fn output_bits_are_balanced_and_reads_spread_across_the_dataset() {
        let pow = engine(PowMode::Light);
        let mut ones = 0u32;
        for nonce in 0..500u64 {
            let hash = pow.calculate_hash(&Hash([3u8; 32]), nonce, 0);
            ones += hash.0.iter().map(|b| b.count_ones()).sum::<u32>();
        }
        // 128,000 output bits: expect about half set
        assert!((62_000..66_000).contains(&ones), "{}", ones);

        // Different dataset items are different
        let context = pow.context_for(0);
        let distinct: std::collections::HashSet<Item> = (0..1024).map(|i| context.item(i)).collect();
        assert_eq!(distinct.len(), 1024);
    }

    #[test]
    fn only_recent_epochs_are_kept_in_memory() {
        let pow = engine(PowMode::Light);
        for epoch in 0..5u64 {
            pow.calculate_hash(&Hash([1u8; 32]), 0, epoch * 100);
        }
        assert_eq!(pow.epochs.lock().unwrap().len(), EPOCHS_KEPT);
    }

    const FIXED_VECTOR: &str = "8be27a22590e66a67e6bda23bde183c74df1a116d7707659407df02d8959a242";
}
