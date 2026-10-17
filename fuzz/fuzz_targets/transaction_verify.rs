#![no_main]
//! A transaction's signature scripts are attacker-controlled. Checking them against either
//! kind of address must never panic.

use imoney_core::escrow::SCRIPT_VERSION_ESCROW;
use imoney_core::hashlock::SCRIPT_VERSION_HASHLOCK;
use imoney_core::multisig::SCRIPT_VERSION_MULTISIG;
use imoney_core::subscription::SCRIPT_VERSION_SUBSCRIPTION;
use imoney_core::{Decode, Encode, Network, ScriptPublicKey, Transaction};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((script_hash, rest)) = data.split_at_checked(32) else { return };
    if let Ok(tx) = Transaction::from_bytes(rest) {
        assert_eq!(tx.to_bytes(), rest);
        let _ = tx.id();
        let _ = tx.invoice_id();
        for version in [0, SCRIPT_VERSION_MULTISIG, SCRIPT_VERSION_HASHLOCK, SCRIPT_VERSION_ESCROW, SCRIPT_VERSION_SUBSCRIPTION] {
            // An offer take needs no signature by design; its rules are checked against the outputs
            let locked = ScriptPublicKey { version, script: script_hash.to_vec() };
            for index in 0..tx.inputs.len().min(4) {
                // Random bytes forging a signature would be a finding in itself
                assert!(tx.verify_input_at(Network::Testnet, index, &locked, u64::MAX).is_err());
            }
        }
    }
});
