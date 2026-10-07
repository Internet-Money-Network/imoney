use crate::constants::MAX_BLOCK_PARENTS;
use crate::hash::Hash;
use crate::serialize::{put_list, tagged_hash, Decode, DecodeError, Encode, Reader};
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

    /// The block ID: a hash of the full header, including the nonce.
    /// Cheap to compute, unlike the proof-of-work hash that is compared against the target.
    pub fn hash(&self) -> Hash {
        tagged_hash("IMN 2026 block hash", &[&self.to_bytes()])
    }
}

impl Encode for BlockHeader {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.to_be_bytes());
        put_list(out, &self.parents);
        self.hash_merkle_root.encode(out);
        self.accepted_id_merkle_root.encode(out);
        self.utxo_commitment.encode(out);
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        out.extend_from_slice(&self.bits.to_be_bytes());
        out.extend_from_slice(&self.nonce.to_be_bytes());
        out.extend_from_slice(&self.daa_score.to_be_bytes());
        out.extend_from_slice(&self.blue_score.to_be_bytes());
        out.extend_from_slice(&self.blue_work.to_be_bytes());
    }
}

impl Decode for BlockHeader {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            version: reader.u16()?,
            parents: reader.list(MAX_BLOCK_PARENTS)?,
            hash_merkle_root: reader.hash()?,
            accepted_id_merkle_root: reader.hash()?,
            utxo_commitment: reader.hash()?,
            timestamp_ms: reader.u64()?,
            bits: reader.u32()?,
            nonce: reader.u64()?,
            daa_score: reader.u64()?,
            blue_score: reader.u64()?,
            blue_work: reader.u128()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BlockHeader {
        BlockHeader {
            version: 1,
            parents: vec![Hash([1u8; 32]), Hash([2u8; 32])],
            hash_merkle_root: Hash([3u8; 32]),
            accepted_id_merkle_root: Hash::ZERO,
            utxo_commitment: Hash::ZERO,
            timestamp_ms: 1_791_244_800_000,
            bits: 0x207fffff,
            nonce: 99,
            daa_score: 10,
            blue_score: 9,
            blue_work: 9000,
        }
    }

    #[test]
    fn header_round_trips_and_has_fixed_size() {
        let header = sample();
        let bytes = header.to_bytes();
        // 2 + 4 + 2*32 + 3*32 + 8 + 4 + 8 + 8 + 8 + 16
        assert_eq!(bytes.len(), 218);
        assert_eq!(BlockHeader::from_bytes(&bytes).unwrap(), header);
    }

    #[test]
    fn block_hash_commits_to_nonce_but_pre_pow_hash_does_not() {
        let header = sample();
        let mut other = header.clone();
        other.nonce += 1;

        assert_ne!(header.hash(), other.hash());
        assert_eq!(header.pre_pow_hash().unwrap(), other.pre_pow_hash().unwrap());
    }

    #[test]
    fn decode_rejects_too_many_parents() {
        let mut header = sample();
        header.parents = vec![Hash([7u8; 32]); MAX_BLOCK_PARENTS + 1];
        assert!(BlockHeader::from_bytes(&header.to_bytes()).is_err());
    }
}
