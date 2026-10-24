use imoney_core::{Block, BlockHeader, Hash, Transaction};

/// Testnet-1 Network Magic identifier: "IMNT"
#[allow(dead_code)]
pub const TESTNET_MAGIC: [u8; 4] = [0x49, 0x4d, 0x4e, 0x54];

/// The public test network's easiest target, and its genesis block's: about a million hashes
/// a block (2^20). That is what a block costs while almost nobody is mining, so the chain
/// keeps moving on one CPU; a single graphics card pushes the difficulty about a hundred
/// times higher within a few blocks.
pub const TESTNET_GENESIS_BITS: u32 = 0x1e100000;

/// Private test networks (`--devnet`) and unit tests: every other hash is a block.
pub const DEVNET_GENESIS_BITS: u32 = 0x207fffff;

/// The genesis block of the public test network (testnet-6: quantum-safe addresses, Ed25519 with
/// SQIsign, and the maintenance fund). Each rule change gets a new genesis, so no node without
/// the rules can join: it would judge blocks differently and its ledger would part from the
/// network's.
pub fn create_testnet_genesis() -> Block {
    // 2026-10-24 00:00:00 UTC
    genesis(
        1_792_800_000_000,
        TESTNET_GENESIS_BITS,
        b"Internet Money testnet-6: quantum-safe addresses (Ed25519 + SQIsign), 5% maintenance fund for 3 years, 5-second blockDAG, Hallmark PoW",
    )
}

/// The genesis block shared by private test networks, which are told apart by their
/// network identifier instead.
pub fn create_devnet_genesis() -> Block {
    // 2026-10-06 00:00:00 UTC
    genesis(1_791_244_800_000, DEVNET_GENESIS_BITS, b"Internet Money: 5-second blockDAG, Hallmark PoW, fair launch")
}

fn genesis(timestamp_ms: u64, bits: u32, message: &[u8]) -> Block {
    // The genesis coinbase carries the launch message and creates no coins (zero premine)
    let transactions = vec![Transaction::coinbase(0, Vec::new(), message)];

    let header = BlockHeader {
        version: 1,
        parents: Vec::new(), // Genesis has no parents
        hash_merkle_root: Block::compute_merkle_root(&transactions),
        accepted_id_merkle_root: Hash::ZERO,
        utxo_commitment: Hash::ZERO,
        timestamp_ms,
        bits,
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

    /// A genesis dated in the future cannot be mined on until that time: blocks must be stamped
    /// after it and at most a little ahead of the clock. testnet-6 shipped dated six hours ahead
    /// and could not be mined until then; a release must not do that again.
    #[test]
    fn the_testnet_genesis_is_not_dated_in_the_future() {
        let genesis_ms = create_testnet_genesis().header.timestamp_ms;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        assert!(genesis_ms <= now_ms, "the testnet genesis is dated {} ms in the future", genesis_ms - now_ms);
    }

    #[test]
    fn genesis_is_deterministic_and_creates_no_coins() {
        for create in [create_testnet_genesis, create_devnet_genesis] {
            let genesis = create();
            assert_eq!(genesis.hash(), create().hash());
            assert_eq!(genesis.validate_structure(), Ok(()));
            assert!(genesis.transactions.iter().all(|tx| tx.outputs.is_empty()));
        }
        assert_ne!(create_testnet_genesis().hash(), create_devnet_genesis().hash());
        // The public network's floor is 2^20 expected hashes a block
        assert_eq!(imoney_consensus::work_from_bits(TESTNET_GENESIS_BITS), (1 << 20) - 1);
    }
}
