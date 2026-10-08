#![no_main]
//! Everything a peer sends goes through the frame reader and the message decoder.

use imoney_core::Decode;
use imoney_node::genesis::TESTNET_MAGIC;
use imoney_node::p2p::codec::{FrameReader, Message};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = Message::from_bytes(data);

    // The same bytes as a stream behind a correct header, so the reader gets past the magic
    let mut stream = TESTNET_MAGIC.to_vec();
    stream.extend_from_slice(data);
    let runtime = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
    runtime.block_on(async {
        let mut reader = FrameReader::default();
        let mut input: &[u8] = &stream;
        while let Ok(Some(_)) = reader.read(&mut input, TESTNET_MAGIC).await {}
    });
});
