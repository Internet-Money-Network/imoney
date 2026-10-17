//! Issues a token, sends some of it for an invoice, and mints more.
//!
//!     cargo run -p imoney-cookbook --example token -- <key>
//!
//! `<key>` pays the IMN fees and receives the new token. The token goes to a fresh address.

use imoney_cookbook::{address_of, load_key, new_key, node_url, Node, FEE_ATOMS, NETWORK};
use imoney_core::{Hash, Outpoint, Transaction, TokenInfo, TxOutput};
use std::time::Duration;

/// Waits until the node has accepted `tx_id` into a block.
fn wait_for_block(node: &Node, tx_id: &str) -> Result<(), String> {
    for _ in 0..60 {
        if node.get(&format!("tx/{}", tx_id))?["status"] == "confirmed" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(format!("{} did not reach a block", tx_id))
}

/// Every coin the address holds, with or without a token, as the builders take them.
fn all_coins(node: &Node, key: &ed25519_dalek::SigningKey) -> Result<Vec<(Outpoint, TxOutput)>, String> {
    let address = address_of(key);
    let listed: Vec<serde_json::Value> = serde_json::from_value(node.get(&format!("address/{}/utxos", address))?).map_err(|e| e.to_string())?;
    listed
        .iter()
        .filter(|coin| coin["spendable"] != false)
        .map(|coin| {
            let mut output = TxOutput::new(coin["value_atoms"].as_u64().unwrap_or(0), imoney_core::ScriptPublicKey::pay_to_address(&address));
            if let Some(token) = coin.get("token").filter(|t| !t.is_null()) {
                let id = Hash::from_hex(token["token_id"].as_str().unwrap_or_default()).map_err(|e| e.to_string())?;
                output.token = Some(imoney_core::TokenAmount {
                    id,
                    amount: token["amount"].as_u64().unwrap_or(0),
                    mint_authority: token["mint_authority"].as_bool().unwrap_or(false),
                });
            }
            let transaction_id = Hash::from_hex(coin["transaction_id"].as_str().unwrap_or_default()).map_err(|e| e.to_string())?;
            Ok((Outpoint { transaction_id, index: coin["index"].as_u64().unwrap_or(0) as u32 }, output))
        })
        .collect()
}

fn main() -> Result<(), String> {
    let issuer = load_key(&std::env::args().nth(1).ok_or("usage: token <key>")?)?;
    let shop = new_key();
    let node = Node::new(&node_url());

    // 1. Issue 1,000.00 "BEAN" (two decimals, so 100,000 units), keeping the right to mint more
    let info = TokenInfo { name: "Coffee points".into(), ticker: "BEAN".into(), decimals: 2, url: String::new() };
    let issue = Transaction::build_token_issue(&issuer, NETWORK, info, 100_000, true, FEE_ATOMS, node.spendable_coins(&address_of(&issuer))?)?;
    let id = issue.issued_token().expect("an issuing transaction names its token");
    wait_for_block(&node, &node.broadcast(&issue)?)?;
    println!("Issued token {}", id.to_hex());
    println!("  {}", node.get(&format!("token/{}", id.to_hex()))?);

    // 2. Pay a shop 2.50 BEAN for an order. The fee is paid in IMN.
    let pay = Transaction::build_token_transfer(&issuer, NETWORK, &address_of(&shop), id, 250, FEE_ATOMS, all_coins(&node, &issuer)?, Some("order-1"))?;
    wait_for_block(&node, &node.broadcast(&pay)?)?;
    println!("The shop holds: {}", node.get(&format!("address/{}/tokens", address_of(&shop)))?);

    // 3. Mint 500.00 more
    let mint = Transaction::build_token_mint(&issuer, NETWORK, id, 50_000, false, FEE_ATOMS, all_coins(&node, &issuer)?)?;
    wait_for_block(&node, &node.broadcast(&mint)?)?;
    println!("The issuer holds: {}", node.get(&format!("address/{}/tokens", address_of(&issuer)))?);
    Ok(())
}
