//! The Internet Money steps of an atomic swap, one per command, printing JSON. Another
//! program drives these alongside the other chain; see `contracts/e2e-swap.mjs`.
//!
//!     swap_cli pubkey <key>
//!     swap_cli lock   <funding-key> <recipient-pubkey-hex> <secret-hash-hex> <amount-atoms> <refund-after-blocks>
//!     swap_cli claim  <recipient-key> <script-hex> <lock-txid> <amount-atoms> <secret-hex>
//!     swap_cli refund <refunder-key> <script-hex> <lock-txid> <amount-atoms>
//!
//! A key is 64 hex characters or a file holding them.

use imoney_cookbook::{address_of, load_key, node_url, Node, FEE_ATOMS, NETWORK};
use imoney_core::{Hash, HashLockScript, Outpoint, ScriptPublicKey, Transaction, TxOutput};
use serde_json::json;

fn bytes32(hex_text: &str) -> Result<[u8; 32], String> {
    hex::decode(hex_text.trim_start_matches("0x"))
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "expected 32 bytes of hex".to_string())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).cloned().ok_or_else(|| "missing argument; see the top of swap_cli.rs".to_string());
    let node = Node::new(&node_url());
    let atoms = |i: usize| -> Result<u64, String> { arg(i)?.parse().map_err(|_| "amount must be a whole number of atoms".to_string()) };

    let output = match arg(0)?.as_str() {
        "pubkey" => {
            let key = load_key(&arg(1)?)?;
            json!({ "public_key": hex::encode(key.verifying_key().to_bytes()), "address": address_of(&key).to_string() })
        }
        "lock" => {
            let funder = load_key(&arg(1)?)?;
            let blue_score = node.get("info")?["virtual_blue_score"].as_u64().ok_or("the node gave no blue score")?;
            let refund_after = blue_score + arg(5)?.parse::<u64>().map_err(|e| e.to_string())?;
            let script = HashLockScript::new(bytes32(&arg(2)?)?, funder.verifying_key().to_bytes(), bytes32(&arg(3)?)?, refund_after)
                .map_err(|e| e.to_string())?;
            let address = script.address(NETWORK);
            let coins = node.spendable_coins(&address_of(&funder))?;
            let funding = Transaction::build_payment(&funder, NETWORK, &address, atoms(4)?, FEE_ATOMS, coins, None)?;
            let tx_id = node.broadcast(&funding)?;
            json!({ "script": hex::encode(script.to_bytes()), "address": address.to_string(), "tx_id": tx_id, "refund_after": refund_after })
        }
        command @ ("claim" | "refund") => {
            let key = load_key(&arg(1)?)?;
            let script = HashLockScript::from_bytes(&hex::decode(arg(2)?).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let locked = TxOutput::new(atoms(4)?, ScriptPublicKey::pay_to_address(&script.address(NETWORK)));
            let coin = (Outpoint { transaction_id: Hash::from_hex(&arg(3)?).map_err(|e| e.to_string())?, index: 0 }, locked);
            let secret = if command == "claim" { Some(bytes32(&arg(5)?)?) } else { None };
            let tx = Transaction::spend_hashlock(&script, NETWORK, coin, &address_of(&key), FEE_ATOMS, &key, secret)?;
            json!({ "tx_id": node.broadcast(&tx)?, "paid_to": address_of(&key).to_string() })
        }
        other => return Err(format!("unknown command '{}'", other)),
    };
    println!("{}", output);
    Ok(())
}
