//! A shared address that needs two of three keys to spend.
//!
//!     cargo run -p imoney-cookbook --example multisig -- <funding-key>
//!
//! The funding key pays 1 IMN into a new 2-of-3 address, and two of the three holders then
//! pay 0.4 IMN back out of it.

use imoney_cookbook::{address_of, imn, load_key, new_key, node_url, Node, FEE_ATOMS, NETWORK};
use imoney_core::{MultisigScript, Transaction, ATOMS_PER_IMN};
use std::time::Duration;

fn main() -> Result<(), String> {
    let funder = load_key(&std::env::args().nth(1).ok_or("usage: multisig <funding-key>")?)?;
    let node = Node::new(&node_url());

    // Three holders, each with their own key. Only the public halves go into the script.
    let holders = [new_key(), new_key(), new_key()];
    let public_keys: Vec<[u8; 32]> = holders.iter().map(|key| key.verifying_key().to_bytes()).collect();
    let script = MultisigScript::new(2, &public_keys).map_err(|e| e.to_string())?;
    let shared = script.address(NETWORK);
    println!("2-of-3 address: {}", shared);
    // The script is needed to spend, and the address alone does not reveal it: back it up
    println!("Script to keep: {}", hex::encode(script.to_bytes()));

    // Paying into it is an ordinary payment
    let coins = node.spendable_coins(&address_of(&funder))?;
    let funding = Transaction::build_payment(&funder, NETWORK, &shared, ATOMS_PER_IMN, FEE_ATOMS, coins, None)?;
    println!("Funded in {}", node.broadcast(&funding)?);

    // Wait for the payment to be in a block, so the shared address has a coin to spend
    let shared_coins = loop {
        let coins = node.spendable_coins(&shared)?;
        if !coins.is_empty() {
            break coins;
        }
        std::thread::sleep(Duration::from_secs(1));
    };

    // Anyone can prepare the payment; it is worth nothing until enough holders sign it
    let amount = 40_000_000;
    let mut tx =
        Transaction::build_multisig_payment(&script, NETWORK, &address_of(&funder), amount, FEE_ATOMS, shared_coins, None, None)?;
    for input in 0..tx.inputs.len() {
        // Holders 0 and 2 sign, each on their own machine in real use. Order does not matter.
        let signatures = [tx.multisig_sign(NETWORK, input, &holders[2]), tx.multisig_sign(NETWORK, input, &holders[0])];
        tx.set_multisig_signatures(input, &script, &signatures).map_err(|e| e.to_string())?;
    }
    println!("Two holders paid {} IMN back out in {}", imn(amount), node.broadcast(&tx)?);
    Ok(())
}
