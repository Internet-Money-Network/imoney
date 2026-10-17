//! Sends a payment, optionally naming an invoice.
//!
//!     cargo run -p imoney-cookbook --example send -- <key> <to-address> <amount-imn> [invoice-id]
//!
//! `<key>` is a private key in hex, or a file holding one.
//!
//! The steps are the same in every language: ask a node which coins the sender has, build a
//! transaction that spends some of them, sign it locally, and hand it to any node. The node
//! never sees the private key.

use imoney_cookbook::{address_of, imn, load_key, node_url, Node, FEE_ATOMS, NETWORK};
use imoney_core::{Address, Encode, Transaction, ATOMS_PER_IMN};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [key, to, amount, rest @ ..] = args.as_slice() else {
        return Err("usage: send <key> <to-address> <amount-imn> [invoice-id]".to_string());
    };
    let key = load_key(key)?;
    let recipient = Address::decode(to).map_err(|e| format!("bad address: {}", e))?;
    let amount_atoms = (amount.parse::<f64>().map_err(|e| e.to_string())? * ATOMS_PER_IMN as f64).round() as u64;
    let invoice_id = rest.first().map(String::as_str);

    let node = Node::new(&node_url());
    let coins = node.spendable_coins(&address_of(&key))?;
    println!("{} has {} spendable coins", address_of(&key), coins.len());

    // Picks coins, pays the recipient, returns the change to the sender and signs every input.
    // A payment that spends many small coins is larger and needs a larger fee: nodes ask for
    // at least 10 atoms per byte, so build once, check the size, and rebuild if needed.
    let build = |fee: u64| Transaction::build_invoice_payment(&key, NETWORK, &recipient, amount_atoms, fee, coins.clone(), None, invoice_id);
    let mut fee = FEE_ATOMS;
    let mut tx = build(fee)?;
    while fee < 20 * tx.to_bytes().len() as u64 {
        fee = 20 * tx.to_bytes().len() as u64;
        tx = build(fee)?;
    }
    let tx_id = node.broadcast(&tx)?;
    println!("Sent {} IMN, fee {} IMN", imn(amount_atoms), imn(fee));
    println!("Transaction {}", tx_id);
    Ok(())
}
