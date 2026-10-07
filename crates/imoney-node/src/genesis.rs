use imoney_core::{Block, BlockHeader, Hash, Transaction};

/// Testnet-1 Network Magic identifier: "IMNT"
#[allow(dead_code)]
pub const TESTNET_MAGIC: [u8; 4] = [0x49, 0x4d, 0x4e, 0x54];

/// Easy initial difficulty target for devnet / testnet (bits 0x207fffff).
pub const TESTNET_GENESIS_BITS: u32 = 0x207fffff;

/// Creates the deterministic Genesis Block for Internet Money Testnet-1.
pub fn create_testnet_genesis() -> Block {
    // Deterministic timestamp: 2026-10-06 00:00:00 UTC = 1791244800000 ms
    let timestamp_ms = 1_791_244_800_000u64;

    // The genesis coinbase carries the launch message and creates no coins (zero premine)
    let genesis_payload = b"Internet Money: 5-second blockDAG, Money Printer PoW, fair launch";
    let transactions = vec![Transaction::coinbase(0, Vec::new(), genesis_payload)];

    let header = BlockHeader {
        version: 1,
        parents: Vec::new(), // Genesis has no parents
        hash_merkle_root: Block::compute_merkle_root(&transactions),
        accepted_id_merkle_root: Hash::ZERO,
        utxo_commitment: Hash::ZERO,
        timestamp_ms,
        bits: TESTNET_GENESIS_BITS,
        nonce: 42,
        daa_score: 0,
        blue_score: 0,
        blue_work: 0,
    };

    Block { header, transactions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_is_deterministic_and_creates_no_coins() {
        let genesis = create_testnet_genesis();
        assert_eq!(genesis.hash(), create_testnet_genesis().hash());
        assert_eq!(genesis.validate_structure(), Ok(()));
        assert!(genesis.transactions.iter().all(|tx| tx.outputs.is_empty()));
    }
}
