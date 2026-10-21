//! Escrow between a buyer and a seller, with an arbiter for disputes.
//!
//!     cargo run -p imoney-cookbook --example escrow -- <buyer-key>
//!
//! The buyer pays 1 IMN into an escrow, and then releases it to the seller once the goods
//! arrive. The seller and the arbiter are fresh keys; in real use each party holds their own.

use imoney_cookbook::{address_of, imn, load_key, new_key, node_url, Node, FEE_ATOMS, NETWORK};
use imoney_core::{EscrowRole, EscrowScript, Outpoint, Transaction, ATOMS_PER_IMN};
use std::time::Duration;

fn main() -> Result<(), String> {
    let buyer = load_key(&std::env::args().nth(1).ok_or("usage: escrow <buyer-key>")?)?;
    let (seller, arbiter) = (new_key(), new_key());
    let node = Node::new(&node_url());

    // The terms: three public keys, and a deadline after which the seller may collect alone
    // (about a day of blocks from now)
    let now = node.get("info")?["virtual_blue_score"].as_u64().ok_or("the node gave no blue score")?;
    let script = EscrowScript::new(
        buyer.verifying_key().to_bytes(),
        seller.verifying_key().to_bytes(),
        arbiter.verifying_key().to_bytes(),
        now + 17_280,
        true,
    )
    .map_err(|e| e.to_string())?;
    let escrow = script.address(NETWORK);
    println!("Escrow address: {}", escrow);
    // Like a shared wallet's script, every party should keep a copy of the terms
    println!("Terms to keep:  {}", hex::encode(script.to_bytes()));

    // The buyer pays into it, an ordinary payment
    let fund =
        Transaction::build_payment(&buyer, NETWORK, &escrow, ATOMS_PER_IMN, FEE_ATOMS, node.spendable_coins(&address_of(&buyer))?, None)?;
    let coin = (Outpoint { transaction_id: fund.id(), index: 0 }, fund.outputs[0].clone());
    println!("Paid into escrow in {}", node.broadcast(&fund)?);
    while node.spendable_coins(&escrow)?.is_empty() {
        std::thread::sleep(Duration::from_secs(1));
    }

    // The goods arrive: the buyer releases the money. The buyer's signature can only ever pay
    // the seller, so there is nothing else this could be turned into.
    let release = Transaction::spend_escrow(&script, NETWORK, coin, EscrowRole::Buyer, &buyer, FEE_ATOMS, None)?;
    println!("Released {} IMN to the seller in {}", imn(release.outputs[0].value_atoms), node.broadcast(&release)?);
    Ok(())
}
