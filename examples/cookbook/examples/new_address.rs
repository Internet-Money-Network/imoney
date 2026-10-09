//! Makes a new key and shows its address.
//!
//!     cargo run -p imoney-cookbook --example new_address
//!
//! An address is derived from a public key, so nothing has to be registered anywhere: the
//! address exists as soon as you have the key, and it works on a machine that is offline.

use imoney_cookbook::{address_of, new_key};

fn main() {
    let key = new_key();
    println!("Address:     {}", address_of(&key));
    println!("Private key: {}", hex::encode(key.to_bytes()));
    println!("Whoever has the private key can spend what the address receives. Keep it secret.");
}
