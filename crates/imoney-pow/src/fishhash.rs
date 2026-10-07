// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// This file is a port of the FishHash proof-of-work algorithm from the
// `fish_hash` crate (version 0.3.0) by Iron Fish, https://github.com/iron-fish/ironfish,
// which is licensed under the MPL-2.0. Changes from the original: the cache and dataset
// sizes are parameters instead of constants, hashing takes `&self` so it can run on many
// threads, and the dataset is built up front in parallel instead of lazily.
// With the original sizes and seed it produces the same hashes as the original.

//! FishHash: an Ethash-family, memory-hard proof-of-work function.
//!
//! A small *light cache* is built from a seed. Every item of the large *dataset* can be
//! computed from that cache alone. A hash reads three dataset items in each of 32 rounds.
//! Miners keep the whole dataset in memory; verifiers compute just the items one hash reads.

use rayon::prelude::*;
use sha3::{Digest, Keccak512};

const FNV_PRIME: u32 = 0x0100_0193;
const FULL_DATASET_ITEM_PARENTS: u32 = 512;
const NUM_DATASET_ACCESSES: usize = 32;
const LIGHT_CACHE_ROUNDS: usize = 3;

/// The seed fixed by the FishHash specification, as used by Iron Fish.
pub const FISHHASH_SEED: [u8; 32] = [
    0xeb, 0x01, 0x63, 0xae, 0xf2, 0xab, 0x1c, 0x5a, 0x66, 0x31, 0x0c, 0x1c, 0x14, 0xd6, 0x0f, 0x42,
    0x55, 0xa9, 0xb3, 0x9b, 0x0e, 0xdf, 0x26, 0x53, 0x98, 0x44, 0xf1, 0x17, 0xad, 0x67, 0x21, 0x19,
];

/// Light cache size fixed by the FishHash specification (about 75 MB).
pub const FISHHASH_LIGHT_CACHE_ITEMS: u32 = 1_179_641;
/// Dataset size fixed by the FishHash specification (about 4.6 GB).
pub const FISHHASH_DATASET_ITEMS: u32 = 37_748_717;

/// A 64-byte cache item as sixteen little-endian words.
type Hash512 = [u32; 16];
/// A 128-byte dataset item as thirty-two little-endian words.
type Hash1024 = [u32; 32];

fn keccak512(words: &Hash512) -> Hash512 {
    let mut bytes = [0u8; 64];
    for (chunk, word) in bytes.chunks_exact_mut(4).zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    words_from_bytes(&Keccak512::digest(bytes))
}

fn words_from_bytes(bytes: &[u8]) -> Hash512 {
    let mut words = [0u32; 16];
    for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(4)) {
        *word = u32::from_le_bytes(chunk.try_into().unwrap());
    }
    words
}

fn fnv1(u: u32, v: u32) -> u32 {
    u.wrapping_mul(FNV_PRIME) ^ v
}

fn fnv1_512(u: &Hash512, v: &Hash512) -> Hash512 {
    let mut r = [0u32; 16];
    for i in 0..16 {
        r[i] = fnv1(u[i], v[i]);
    }
    r
}

fn build_light_cache(seed: &[u8; 32], items: u32) -> Vec<Hash512> {
    let mut cache = Vec::with_capacity(items as usize);
    let mut item = words_from_bytes(&Keccak512::digest(seed));
    cache.push(item);
    for _ in 1..items {
        item = keccak512(&item);
        cache.push(item);
    }

    for _ in 0..LIGHT_CACHE_ROUNDS {
        for i in 0..items {
            // First index: the item's first word
            let v = cache[i as usize][0] % items;
            // Second index: the previous item, wrapping around
            let w = items.wrapping_add(i.wrapping_sub(1)) % items;

            let mut x = [0u32; 16];
            for j in 0..16 {
                x[j] = cache[v as usize][j] ^ cache[w as usize][j];
            }
            cache[i as usize] = keccak512(&x);
        }
    }
    cache
}

fn calculate_dataset_item_1024(light_cache: &[Hash512], index: usize) -> Hash1024 {
    let cache_items = light_cache.len() as u32;
    let seed0 = (index * 2) as u32;
    let seed1 = seed0 + 1;

    let mut mix0 = light_cache[(seed0 % cache_items) as usize];
    let mut mix1 = light_cache[(seed1 % cache_items) as usize];
    mix0[0] ^= seed0;
    mix1[0] ^= seed1;

    mix0 = keccak512(&mix0);
    mix1 = keccak512(&mix1);

    for j in 0..FULL_DATASET_ITEM_PARENTS {
        let t0 = fnv1(seed0 ^ j, mix0[(j % 16) as usize]);
        let t1 = fnv1(seed1 ^ j, mix1[(j % 16) as usize]);
        mix0 = fnv1_512(&mix0, &light_cache[(t0 % cache_items) as usize]);
        mix1 = fnv1_512(&mix1, &light_cache[(t1 % cache_items) as usize]);
    }

    mix0 = keccak512(&mix0);
    mix1 = keccak512(&mix1);

    let mut item = [0u32; 32];
    item[..16].copy_from_slice(&mix0);
    item[16..].copy_from_slice(&mix1);
    item
}

/// Reads word pair `j` of an item as one little-endian 64-bit value.
fn get_u64(item: &Hash1024, j: usize) -> u64 {
    item[2 * j] as u64 | (item[2 * j + 1] as u64) << 32
}

/// FishHash memory for one seed: the light cache, and the full dataset when mining.
pub struct FishHashContext {
    light_cache: Vec<Hash512>,
    full_dataset: Option<Vec<Hash1024>>,
    dataset_items: u32,
}

impl FishHashContext {
    /// Builds the light cache, and the full dataset too when `full` is set.
    pub fn new(seed: &[u8; 32], light_cache_items: u32, dataset_items: u32, full: bool) -> Self {
        let light_cache = build_light_cache(seed, light_cache_items);
        let full_dataset = full.then(|| {
            (0..dataset_items as usize)
                .into_par_iter()
                .map(|index| calculate_dataset_item_1024(&light_cache, index))
                .collect()
        });
        Self { light_cache, full_dataset, dataset_items }
    }

    pub fn dataset_items(&self) -> u32 {
        self.dataset_items
    }

    fn lookup(&self, index: usize) -> Hash1024 {
        match &self.full_dataset {
            Some(dataset) => dataset[index],
            None => calculate_dataset_item_1024(&self.light_cache, index),
        }
    }

    fn kernel(&self, seed: &Hash512) -> [u32; 8] {
        let mut mix = [0u32; 32];
        mix[..16].copy_from_slice(seed);
        mix[16..].copy_from_slice(seed);

        for _ in 0..NUM_DATASET_ACCESSES {
            // Calculate new fetching indexes
            let p0 = mix[0] % self.dataset_items;
            let p1 = mix[4] % self.dataset_items;
            let p2 = mix[8] % self.dataset_items;

            let fetch0 = self.lookup(p0 as usize);
            let mut fetch1 = self.lookup(p1 as usize);
            let mut fetch2 = self.lookup(p2 as usize);

            // Modify fetch1 and fetch2
            for j in 0..32 {
                fetch1[j] = fnv1(mix[j], fetch1[j]);
                fetch2[j] ^= mix[j];
            }

            // Final computation of new mix
            for j in 0..16 {
                let value = get_u64(&fetch0, j).wrapping_mul(get_u64(&fetch1, j)).wrapping_add(get_u64(&fetch2, j));
                mix[2 * j] = value as u32;
                mix[2 * j + 1] = (value >> 32) as u32;
            }
        }

        // Collapse the result into 32 bytes
        let mut mix_hash = [0u32; 8];
        for i in (0..32).step_by(4) {
            let h1 = fnv1(mix[i], mix[i + 1]);
            let h2 = fnv1(h1, mix[i + 2]);
            mix_hash[i / 4] = fnv1(h2, mix[i + 3]);
        }
        mix_hash
    }

    /// The FishHash of `header`.
    pub fn hash(&self, header: &[u8]) -> [u8; 32] {
        let mut seed_bytes = [0u8; 64];
        let mut hasher = blake3::Hasher::new();
        hasher.update(header);
        hasher.finalize_xof().fill(&mut seed_bytes);

        let mix_hash = self.kernel(&words_from_bytes(&seed_bytes));

        let mut final_data = [0u8; 96];
        final_data[..64].copy_from_slice(&seed_bytes);
        for (chunk, word) in final_data[64..].chunks_exact_mut(4).zip(&mix_hash) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        *blake3::hash(&final_data).as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_full_contexts_agree() {
        let light = FishHashContext::new(&[7u8; 32], 257, 2053, false);
        let full = FishHashContext::new(&[7u8; 32], 257, 2053, true);
        for i in 0..50u32 {
            let header = i.to_le_bytes();
            assert_eq!(light.hash(&header), full.hash(&header));
        }
        assert_ne!(light.hash(b"a"), light.hash(b"b"));
        assert_ne!(light.hash(b"a"), FishHashContext::new(&[8u8; 32], 257, 2053, false).hash(b"a"));
    }

    /// Byte-for-byte comparison with the Iron Fish implementation at the real sizes.
    /// Builds two 75 MB caches, so it is skipped by default. Run it with:
    /// `cargo test --release -p imoney-pow -- --ignored`
    #[test]
    #[ignore]
    fn matches_the_reference_implementation() {
        let ours = FishHashContext::new(&FISHHASH_SEED, FISHHASH_LIGHT_CACHE_ITEMS, FISHHASH_DATASET_ITEMS, false);
        let mut reference = fish_hash::Context::new(false, None);

        let headers: [&[u8]; 4] = [b"", b"hello fishhash", &[0u8; 180], &[0xa5u8; 40]];
        for header in headers {
            let mut expected = [0u8; 32];
            fish_hash::hash(&mut expected, &mut reference, header);
            assert_eq!(ours.hash(header), expected);
        }
    }
}
