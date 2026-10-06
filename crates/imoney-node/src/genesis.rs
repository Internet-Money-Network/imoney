use imoney_core::{BlockHeader, Hash};

/// Testnet-1 Network Magic identifier: "IMNT"
#[allow(dead_code)]
pub const TESTNET_MAGIC: [u8; 4] = [0x49, 0x4d, 0x4e, 0x54];

/// Easy initial difficulty target for devnet / testnet (bits 0x207fffff).
pub const TESTNET_GENESIS_BITS: u32 = 0x207fffff;

/// Creates the deterministic Genesis Block for Internet Money Testnet-1.
pub fn create_testnet_genesis() -> BlockHeader {
    // Deterministic timestamp: 2026-10-06 00:00:00 UTC = 1791244800000 ms
    let timestamp_ms = 1_791_244_800_000u64;

    // Genesis message: "Internet Money: 5-second blockDAG, Money Printer PoW, fair launch"
    let genesis_payload = b"Internet Money: 5-second blockDAG, Money Printer PoW, fair launch";
    let hash_root = Hash(*blake3::hash(genesis_payload).as_bytes());

    BlockHeader {
        version: 1,
        parents: Vec::new(), // Genesis has no parents
        hash_merkle_root: hash_root,
        accepted_id_merkle_root: Hash::ZERO,
        utxo_commitment: Hash::ZERO,
        timestamp_ms,
        bits: TESTNET_GENESIS_BITS,
        nonce: 42,
        daa_score: 0,
        blue_score: 0,
        blue_work: 0,
    }
}
