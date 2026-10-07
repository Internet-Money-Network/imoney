use crate::constants::{MAX_BLOCK_BYTES, MAX_TX_BYTES};
use crate::hash::Hash;
use crate::header::BlockHeader;
use crate::merkle::merkle_root;
use crate::serialize::{put_list, Decode, DecodeError, Encode, Reader};
use crate::transaction::Transaction;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum BlockError {
    #[error("Block has no transactions")]
    NoTransactions,
    #[error("First transaction is not a coinbase")]
    FirstNotCoinbase,
    #[error("Transaction {0} is a second coinbase")]
    ExtraCoinbase(usize),
    #[error("Transaction {0} has no outputs")]
    NoOutputs(usize),
    #[error("Transaction {0} is {1} bytes, above the {2}-byte limit")]
    TransactionTooLarge(usize, usize, usize),
    #[error("Block is {0} bytes, above the {1}-byte limit")]
    BlockTooLarge(usize, usize),
    #[error("Duplicate transaction in block: {0}")]
    DuplicateTransaction(Hash),
    #[error("Header merkle root does not match the transactions")]
    MerkleRootMismatch,
}

/// A block: header plus the transactions it carries. The first transaction is the coinbase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

impl Block {
    /// The block ID.
    pub fn hash(&self) -> Hash {
        self.header.hash()
    }

    /// Merkle root over the full transaction hashes, so the header commits to signatures too.
    pub fn compute_merkle_root(transactions: &[Transaction]) -> Hash {
        let hashes: Vec<Hash> = transactions.iter().map(Transaction::hash).collect();
        merkle_root(&hashes)
    }

    /// Checks everything that can be verified without chain state.
    pub fn validate_structure(&self) -> Result<(), BlockError> {
        let (coinbase, rest) = self.transactions.split_first().ok_or(BlockError::NoTransactions)?;
        if !coinbase.is_coinbase() {
            return Err(BlockError::FirstNotCoinbase);
        }

        let mut seen = HashSet::with_capacity(self.transactions.len());
        for (i, tx) in self.transactions.iter().enumerate() {
            let size = tx.to_bytes().len();
            if size > MAX_TX_BYTES {
                return Err(BlockError::TransactionTooLarge(i, size, MAX_TX_BYTES));
            }
            let id = tx.id();
            if !seen.insert(id) {
                return Err(BlockError::DuplicateTransaction(id));
            }
        }
        for (i, tx) in rest.iter().enumerate() {
            if tx.is_coinbase() {
                return Err(BlockError::ExtraCoinbase(i + 1));
            }
            if tx.outputs.is_empty() {
                return Err(BlockError::NoOutputs(i + 1));
            }
        }

        let size = self.to_bytes().len();
        if size > MAX_BLOCK_BYTES {
            return Err(BlockError::BlockTooLarge(size, MAX_BLOCK_BYTES));
        }
        if Self::compute_merkle_root(&self.transactions) != self.header.hash_merkle_root {
            return Err(BlockError::MerkleRootMismatch);
        }
        Ok(())
    }
}

impl Encode for Block {
    fn encode(&self, out: &mut Vec<u8>) {
        self.header.encode(out);
        put_list(out, &self.transactions);
    }
}

impl Decode for Block {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            header: BlockHeader::decode(reader)?,
            transactions: reader.list(MAX_BLOCK_BYTES)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::{Address, AddressType, Network};
    use crate::transaction::{Outpoint, ScriptPublicKey, TxOutput};
    use ed25519_dalek::SigningKey;

    fn payment(seed: u8) -> Transaction {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let recipient = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &[seed; 32]);
        let utxo = TxOutput {
            value_atoms: 10_000,
            script_public_key: ScriptPublicKey::pay_to_address(&recipient),
        };
        let outpoint = Outpoint { transaction_id: Hash([seed; 32]), index: 0 };
        Transaction::build_payment(&key, Network::Testnet, &recipient, 5_000, 100, vec![(outpoint, utxo)], None).unwrap()
    }

    fn block_with(transactions: Vec<Transaction>) -> Block {
        let header = BlockHeader {
            version: 1,
            parents: vec![Hash([9u8; 32])],
            hash_merkle_root: Block::compute_merkle_root(&transactions),
            accepted_id_merkle_root: Hash::ZERO,
            utxo_commitment: Hash::ZERO,
            timestamp_ms: 1,
            bits: 0x207fffff,
            nonce: 7,
            daa_score: 1,
            blue_score: 1,
            blue_work: 1000,
        };
        Block { header, transactions }
    }

    #[test]
    fn block_round_trips_through_canonical_encoding() {
        let block = block_with(vec![Transaction::coinbase(1, Vec::new(), b"x"), payment(1), payment(2)]);
        let bytes = block.to_bytes();
        assert_eq!(Block::from_bytes(&bytes).unwrap(), block);

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(Block::from_bytes(&trailing), Err(DecodeError::TrailingBytes(1)));
        assert!(Block::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn structure_rules_are_enforced() {
        let coinbase = Transaction::coinbase(1, Vec::new(), b"x");
        assert_eq!(block_with(vec![coinbase.clone(), payment(1)]).validate_structure(), Ok(()));

        assert_eq!(block_with(Vec::new()).validate_structure(), Err(BlockError::NoTransactions));
        assert_eq!(block_with(vec![payment(1)]).validate_structure(), Err(BlockError::FirstNotCoinbase));
        assert_eq!(
            block_with(vec![coinbase.clone(), Transaction::coinbase(2, Vec::new(), b"y")]).validate_structure(),
            Err(BlockError::ExtraCoinbase(1))
        );
        assert!(matches!(
            block_with(vec![coinbase.clone(), payment(1), payment(1)]).validate_structure(),
            Err(BlockError::DuplicateTransaction(_))
        ));

        let mut tampered = block_with(vec![coinbase, payment(1)]);
        tampered.transactions[1].inputs[0].signature_script[40] ^= 1;
        assert_eq!(tampered.validate_structure(), Err(BlockError::MerkleRootMismatch));
    }

    /// Feeds the decoders random and corrupted input. They must never panic, and anything they
    /// accept must re-encode to exactly the bytes it came from (one encoding per value).
    #[test]
    fn decoders_survive_arbitrary_and_corrupted_input() {
        use rand::{Rng, RngCore, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x1337);

        fn check<T: Decode + Encode>(bytes: &[u8]) {
            if let Ok(value) = T::from_bytes(bytes) {
                assert_eq!(value.to_bytes(), bytes, "accepted a non-canonical encoding");
            }
        }

        for _ in 0..20_000 {
            let mut noise = vec![0u8; rng.gen_range(0..400)];
            rng.fill_bytes(&mut noise);
            check::<Transaction>(&noise);
            check::<BlockHeader>(&noise);
            check::<Block>(&noise);
        }

        let block = block_with(vec![Transaction::coinbase(1, Vec::new(), b"x"), payment(1), payment(2)]);
        let valid = block.to_bytes();
        for _ in 0..20_000 {
            let mut corrupted = valid.clone();
            match rng.gen_range(0..3) {
                0 => {
                    let at = rng.gen_range(0..corrupted.len());
                    corrupted[at] ^= 1 << rng.gen_range(0..8);
                }
                1 => corrupted.truncate(rng.gen_range(0..corrupted.len())),
                _ => {
                    let at = rng.gen_range(0..corrupted.len());
                    corrupted.insert(at, rng.gen());
                }
            }
            check::<Block>(&corrupted);
            check::<Transaction>(&corrupted[corrupted.len().min(222)..]);
        }
    }
}
