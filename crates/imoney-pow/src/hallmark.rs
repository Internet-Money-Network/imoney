//! The Hallmark proof of work: FishHash with a seed specific to each network.
//!
//! The algorithm itself lives in `fishhash.rs`. This module chooses the sizes, derives the
//! network's seed, and wraps hashing, verification and mining for block headers.

use crate::fishhash::{FishHashContext, FISHHASH_DATASET_ITEMS, FISHHASH_LIGHT_CACHE_ITEMS};
use crate::target::is_valid_pow;
use imoney_core::Hash;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Sizes of the proof-of-work memory structures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowParams {
    /// 64-byte items in the light cache used for verification.
    pub light_cache_items: u32,
    /// 128-byte items in the full mining dataset.
    pub dataset_items: u32,
}

impl PowParams {
    /// The public networks: the sizes fixed by the FishHash specification (75 MB cache,
    /// 4.6 GB dataset).
    pub const fn mainnet() -> Self {
        Self { light_cache_items: FISHHASH_LIGHT_CACHE_ITEMS, dataset_items: FISHHASH_DATASET_ITEMS }
    }

    /// Private test networks and local development: 1 MB cache, 32 MB dataset, so a CPU can mine.
    pub const fn dev() -> Self {
        Self { light_cache_items: 16_411, dataset_items: 262_147 }
    }

    /// Unit tests: small enough to build instantly in a debug build.
    pub const fn tiny() -> Self {
        Self { light_cache_items: 257, dataset_items: 1_031 }
    }
}

/// Whether to hold the full dataset in memory or compute items from the cache on demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowMode {
    /// Light cache only. Enough to verify blocks; slow for mining.
    Light,
    /// Full dataset. Needed to mine at full speed.
    Full,
}

/// The FishHash seed of a network, derived from its genesis block hash. A different seed gives
/// a different dataset, so hashpower pointed at another FishHash network does not apply here.
pub fn network_seed(genesis_hash: &Hash) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("IMN 2026 Hallmark seed");
    hasher.update(&genesis_hash.0);
    *hasher.finalize().as_bytes()
}

/// The bytes hashed for a block: its pre-proof-of-work header hash followed by the nonce.
fn pow_input(pre_pow_hash: &Hash, nonce: u64) -> [u8; 40] {
    let mut input = [0u8; 40];
    input[..32].copy_from_slice(&pre_pow_hash.0);
    input[32..].copy_from_slice(&nonce.to_le_bytes());
    input
}

/// The Hallmark Proof-of-Work engine. The memory is built the first time it is needed.
pub struct HallmarkPow {
    params: PowParams,
    seed: [u8; 32],
    mode: PowMode,
    context: OnceLock<Arc<FishHashContext>>,
}

impl HallmarkPow {
    /// Creates the engine for the network with the given genesis block hash.
    pub fn new(params: PowParams, genesis_hash: Hash, mode: PowMode) -> Self {
        Self { params, seed: network_seed(&genesis_hash), mode, context: OnceLock::new() }
    }

    pub fn params(&self) -> &PowParams {
        &self.params
    }

    /// The cache (and dataset, in full mode), built on first use.
    pub fn context(&self) -> Arc<FishHashContext> {
        self.context
            .get_or_init(|| {
                Arc::new(FishHashContext::new(
                    &self.seed,
                    self.params.light_cache_items,
                    self.params.dataset_items,
                    self.mode == PowMode::Full,
                ))
            })
            .clone()
    }

    /// Computes the 32-byte proof-of-work hash for a given header pre-hash and nonce.
    pub fn calculate_hash(&self, pre_pow_hash: &Hash, nonce: u64) -> Hash {
        Hash(self.context().hash(&pow_input(pre_pow_hash, nonce)))
    }

    /// Validates whether a given block pre_pow_hash and nonce meet the target bits.
    pub fn verify(&self, pre_pow_hash: &Hash, nonce: u64, bits: u32) -> bool {
        is_valid_pow(&self.calculate_hash(pre_pow_hash, nonce), bits)
    }

    /// Parallel multi-threaded CPU miner loop.
    pub fn mine(
        &self,
        pre_pow_hash: &Hash,
        bits: u32,
        start_nonce: u64,
        max_iters: u64,
        stop_signal: Arc<AtomicBool>,
    ) -> Option<(u64, Hash)> {
        let context = self.context();
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
                let hash = Hash(context.hash(&pow_input(pre_pow_hash, n)));
                if is_valid_pow(&hash, bits) {
                    found_nonce.store(n, Ordering::SeqCst);
                    stop_signal.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });

        let n = found_nonce.load(Ordering::SeqCst);
        if n != u64::MAX {
            Some((n, self.calculate_hash(pre_pow_hash, n)))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(mode: PowMode) -> HallmarkPow {
        HallmarkPow::new(PowParams::tiny(), Hash([7u8; 32]), mode)
    }

    #[test]
    fn light_verification_matches_full_dataset_mining() {
        let (light, full) = (engine(PowMode::Light), engine(PowMode::Full));
        let header = Hash([1u8; 32]);
        for nonce in 0..50 {
            assert_eq!(light.calculate_hash(&header, nonce), full.calculate_hash(&header, nonce));
        }

        // A nonce found with the full dataset verifies from the cache alone
        let bits = 0x2000ffff;
        let (nonce, hash) = full.mine(&header, bits, 0, 1_000_000, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(is_valid_pow(&hash, bits));
        assert!(light.verify(&header, nonce, bits));
        assert!(!light.verify(&header, nonce, 0x1900ffff));
    }

    #[test]
    fn hash_depends_on_header_nonce_and_network() {
        let pow = engine(PowMode::Light);
        let base = pow.calculate_hash(&Hash([1u8; 32]), 9);

        assert_eq!(base, pow.calculate_hash(&Hash([1u8; 32]), 9));
        assert_ne!(base, pow.calculate_hash(&Hash([2u8; 32]), 9));
        assert_ne!(base, pow.calculate_hash(&Hash([1u8; 32]), 10));
        // Another network's genesis: different seed, different dataset
        let other_network = HallmarkPow::new(PowParams::tiny(), Hash([8u8; 32]), PowMode::Light);
        assert_ne!(base, other_network.calculate_hash(&Hash([1u8; 32]), 9));
        // And never the stock FishHash seed
        assert_ne!(network_seed(&Hash([7u8; 32])), crate::fishhash::FISHHASH_SEED);
    }

    #[test]
    fn output_bits_are_balanced() {
        let pow = engine(PowMode::Full);
        let ones: u32 = (0..500u64)
            .map(|nonce| pow.calculate_hash(&Hash([3u8; 32]), nonce).0.iter().map(|b| b.count_ones()).sum::<u32>())
            .sum();
        // 128,000 output bits: expect about half set
        assert!((62_000..66_000).contains(&ones), "{}", ones);
    }
}
