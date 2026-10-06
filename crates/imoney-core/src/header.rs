use crate::constants::MAX_BLOCK_PARENTS;
use crate::hash::Hash;
use byteorder::{BigEndian, WriteBytesExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HeaderError {
    #[error("Too many parents in block header: {0} > {1}")]
    TooManyParents(usize, usize),
    #[error("Block header has no parents")]
    NoParents,
}

/// Internet Money blockDAG header structure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Protocol version.
    pub version: u16,
    /// Hashes of direct parent blocks in the blockDAG (up to 16).
    pub parents: Vec<Hash>,
    /// Merkle root of transactions included in this block.
    pub hash_merkle_root: Hash,
    /// Merkle root of transactions accepted by this block's blue set.
    pub accepted_id_merkle_root: Hash,
    /// Commitment to the UTXO set state after applying this block.
    pub utxo_commitment: Hash,
    /// Timestamp in milliseconds since Unix epoch.
    pub timestamp_ms: u64,
    /// Compact difficulty target (bits representation).
    pub bits: u32,
    /// Proof of work nonce found by miner.
    pub nonce: u64,
    /// DAA (Difficulty Adjustment Algorithm) cumulative score.
    pub daa_score: u64,
    /// GHOSTDAG cumulative blue score.
    pub blue_score: u64,
    /// GHOSTDAG cumulative blue work.
    pub blue_work: u128,
}

impl BlockHeader {
    /// Serializes the header components into bytes used for computing PoW hash.
    /// Excludes the nonce, which is iterated during mining.
    pub fn pre_pow_bytes(&self) -> Result<Vec<u8>, HeaderError> {
        if self.parents.is_empty() && self.blue_score > 0 {
            return Err(HeaderError::NoParents);
        }
        if self.parents.len() > MAX_BLOCK_PARENTS {
            return Err(HeaderError::TooManyParents(self.parents.len(), MAX_BLOCK_PARENTS));
        }

        let mut buf = Vec::with_capacity(256);
        buf.write_u16::<BigEndian>(self.version).unwrap();
        buf.write_u8(self.parents.len() as u8).unwrap();
        for parent in &self.parents {
            buf.extend_from_slice(parent.as_bytes());
        }
        buf.extend_from_slice(self.hash_merkle_root.as_bytes());
        buf.extend_from_slice(self.accepted_id_merkle_root.as_bytes());
        buf.extend_from_slice(self.utxo_commitment.as_bytes());
        buf.write_u64::<BigEndian>(self.timestamp_ms).unwrap();
        buf.write_u32::<BigEndian>(self.bits).unwrap();
        buf.write_u64::<BigEndian>(self.daa_score).unwrap();
        buf.write_u64::<BigEndian>(self.blue_score).unwrap();
        buf.write_u128::<BigEndian>(self.blue_work).unwrap();

        Ok(buf)
    }

    /// Computes the unique Pre-PoW hash (header hash without nonce).
    pub fn pre_pow_hash(&self) -> Result<Hash, HeaderError> {
        let bytes = self.pre_pow_bytes()?;
        let hash = blake3::hash(&bytes);
        Ok(Hash(*hash.as_bytes()))
    }
}
