#![no_main]
//! Addresses are typed and pasted by people. Decoding must never panic, and an address that
//! decodes must print back as itself, ignoring letter case.

use imoney_core::Address;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(address) = Address::decode(text) {
        assert_eq!(address.to_string(), text.to_lowercase());
    }
});
