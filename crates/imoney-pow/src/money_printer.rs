use crate::target::is_valid_pow;
use byteorder::{ByteOrder, LittleEndian};
use imoney_core::Hash;
use rayon::prelude::*;
use sha3::{Digest, Keccak256};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Default dataset item count: 65,536 items * 64 bytes = 4 MB (for fast tests/devnet).
/// Production mainnet default: 67,108,864 items * 64 bytes = 4.29 GB.
pub const DEVNET_DATASET_ITEMS: usize = 65_536;
pub const MAINNET_DATASET_ITEMS: usize = 67_108_864;

/// Number of sequential memory access iterations per hash.
pub const NUM_MEMORY_ACCESSES: usize = 32;

/// Money Printer Proof-of-Work Context.
/// Holds the memory-hard dataset in RAM for mining and node validation.
pub struct MoneyPrinterContext {
    /// 64-byte memory dataset items.
    dataset: Vec<[u8; 64]>,
}

impl MoneyPrinterContext {
    /// Generates a new Money Printer memory dataset from a seed hash.
    pub fn new(seed: &Hash, num_items: usize) -> Self {
        let mut dataset = Vec::with_capacity(num_items);
        let mut hasher = blake3::Hasher::new_keyed(&seed.0);

        for i in 0..num_items {
            let mut item = [0u8; 64];
            let mut buf = [0u8; 8];
            LittleEndian::write_u64(&mut buf, i as u64);

            hasher.update(&buf);
            let h1 = hasher.finalize();

            let mut k_hasher = Keccak256::new();
            k_hasher.update(h1.as_bytes());
            k_hasher.update(&buf);
            let h2 = k_hasher.finalize();

            item[..32].copy_from_slice(h1.as_bytes());
            item[32..].copy_from_slice(&h2);
            dataset.push(item);
        }

        Self { dataset }
    }

    /// Number of items in the dataset.
    pub fn item_count(&self) -> usize {
        self.dataset.len()
    }
}

/// The Money Printer Proof-of-Work engine.
pub struct MoneyPrinterPow {
    context: Arc<MoneyPrinterContext>,
}

impl MoneyPrinterPow {
    pub fn new(context: Arc<MoneyPrinterContext>) -> Self {
        Self { context }
    }

    /// Computes the 32-byte Money Printer hash for a given header pre-hash and nonce.
    pub fn calculate_hash(&self, pre_pow_hash: &Hash, nonce: u64) -> Hash {
        let mut initial_input = [0u8; 40];
        initial_input[..32].copy_from_slice(pre_pow_hash.as_bytes());
        LittleEndian::write_u64(&mut initial_input[32..], nonce);

        // Stage 1: Initial Blake3 seed mix (64 bytes)
        let mut mix = [0u8; 64];
        let h1 = blake3::hash(&initial_input);
        mix[..32].copy_from_slice(h1.as_bytes());

        let mut h2_hasher = Keccak256::new();
        h2_hasher.update(h1.as_bytes());
        h2_hasher.update(&nonce.to_le_bytes());
        mix[32..].copy_from_slice(&h2_hasher.finalize());

        // Stage 2: Memory-hard pseudo-random access into the dataset
        let num_items = self.context.item_count() as u64;
        if num_items > 0 {
            for _ in 0..NUM_MEMORY_ACCESSES {
                let index = LittleEndian::read_u64(&mix[0..8]) % num_items;
                let item = &self.context.dataset[index as usize];

                for j in 0..8 {
                    let mix_word = LittleEndian::read_u64(&mix[j * 8..(j + 1) * 8]);
                    let item_word = LittleEndian::read_u64(&item[j * 8..(j + 1) * 8]);
                    let new_word = mix_word.wrapping_mul(31).wrapping_add(item_word) ^ mix_word.rotate_left(13);
                    LittleEndian::write_u64(&mut mix[j * 8..(j + 1) * 8], new_word);
                }
            }
        }

        // Stage 3: Final cryptographic compression
        let final_hash = blake3::hash(&mix);
        Hash(*final_hash.as_bytes())
    }

    /// Validates whether a given block pre_pow_hash and nonce meet the target bits.
    pub fn verify(&self, pre_pow_hash: &Hash, nonce: u64, bits: u32) -> bool {
        let hash = self.calculate_hash(pre_pow_hash, nonce);
        is_valid_pow(&hash, bits)
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
        let threads = rayon::current_num_threads();
        let chunk_size = max_iters / (threads as u64).max(1);
        let found_nonce = Arc::new(AtomicU64::new(u64::MAX));

        (0..threads).into_par_iter().for_each(|t| {
            let thread_start = start_nonce + (t as u64 * chunk_size);
            let thread_end = thread_start + chunk_size;

            for n in thread_start..thread_end {
                if stop_signal.load(Ordering::Relaxed) || found_nonce.load(Ordering::Relaxed) != u64::MAX {
                    break;
                }
                let hash = self.calculate_hash(pre_pow_hash, n);
                if is_valid_pow(&hash, bits) {
                    found_nonce.store(n, Ordering::SeqCst);
                    stop_signal.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });

        let n = found_nonce.load(Ordering::SeqCst);
        if n != u64::MAX {
            let hash = self.calculate_hash(pre_pow_hash, n);
            Some((n, hash))
        } else {
            None
        }
    }
}
