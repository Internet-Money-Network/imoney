#![no_main]
//! A block from the network is arbitrary bytes. Decoding must never panic, and whatever
//! decodes must encode back to the same bytes (one encoding per block).

use imoney_core::{Block, Decode, Encode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(block) = Block::from_bytes(data) {
        assert_eq!(block.to_bytes(), data, "a block decoded from bytes that are not its encoding");
        let _ = block.validate_structure();
        let _ = block.header.hash();
    }
});
