//! The IMN half of an atomic swap: lock coins under a secret's hash, then claim them with the
//! secret, and separately take an unclaimed lock back after its deadline.
//!
//!     cargo run -p imoney-cookbook --example hashlock -- <funding-key>
//!
//! In a real swap Alice and Bob each lock coins for the other under the same hash, on this
//! chain and on another. Claiming one side reveals the secret that claims the other.

use imoney_cookbook::{address_of, imn, load_key, new_key, node_url, Node, FEE_ATOMS, NETWORK};
use imoney_core::{secret_hash, HashLockScript, Outpoint, ScriptPublicKey, Transaction, TxOutput};
use std::time::Duration;

const AMOUNT_ATOMS: u64 = 50_000_000;

fn main() -> Result<(), String> {
    let alice = load_key(&std::env::args().nth(1).ok_or("usage: hashlock <funding-key>")?)?;
    let bob = new_key();
    let node = Node::new(&node_url());
    let blue_score = || -> Result<u64, String> { node.get("info")?["virtual_blue_score"].as_u64().ok_or("no blue score".to_string()) };

    // Alice picks a secret and tells Bob only its hash
    let secret: [u8; 32] = new_key().to_bytes();
    let hash = secret_hash(&secret);
    println!("Secret hash: {}", hex::encode(hash));

    // Locks 0.5 IMN for Bob, and returns the lock's terms and the coin it created
    let lock = |refund_after: u64| -> Result<(HashLockScript, Outpoint), String> {
        let script = HashLockScript::new(bob.verifying_key().to_bytes(), alice.verifying_key().to_bytes(), hash, refund_after)
            .map_err(|e| e.to_string())?;
        let address = script.address(NETWORK);
        let coins = node.spendable_coins(&address_of(&alice))?;
        let funding = Transaction::build_payment(&alice, NETWORK, &address, AMOUNT_ATOMS, FEE_ATOMS, coins, None)?;
        let outpoint = Outpoint { transaction_id: funding.id(), index: 0 };
        node.broadcast(&funding)?;
        println!("Locked {} IMN at {} (refundable from blue score {})", imn(AMOUNT_ATOMS), address, refund_after);
        // Wait for the lock to be in a block before anything spends it
        while node.spendable_coins(&address)?.is_empty() {
            std::thread::sleep(Duration::from_secs(1));
        }
        Ok((script, outpoint))
    };

    // 1. A lock Bob claims. He must know the secret; in a real swap he learns it when Alice
    //    claims his coins on the other chain.
    let (script, outpoint) = lock(blue_score()? + 720)?;
    let claim = Transaction::spend_hashlock(&script, NETWORK, (outpoint, TxOutput::new(AMOUNT_ATOMS, ScriptPublicKey::pay_to_address(&script.address(NETWORK)))), &address_of(&bob), FEE_ATOMS, &bob, Some(secret))?;
    let claim_id = node.broadcast(&claim)?;
    println!("Bob claimed it in {}", claim_id);
    // Anyone can now read the secret from the claim
    let revealed = node.get(&format!("tx/{}", claim_id))?["revealed_secrets"][0].as_str().unwrap_or_default().to_string();
    println!("The claim reveals the secret: {}", revealed == hex::encode(secret));

    // 2. A lock nobody claims. Alice takes it back once the deadline has passed.
    let deadline = blue_score()? + 4;
    let (script, outpoint) = lock(deadline)?;
    let refund = Transaction::spend_hashlock(&script, NETWORK, (outpoint, TxOutput::new(AMOUNT_ATOMS, ScriptPublicKey::pay_to_address(&script.address(NETWORK)))), &address_of(&alice), FEE_ATOMS, &alice, None)?;
    if let Err(reason) = node.broadcast(&refund) {
        println!("Too early to refund, as it should be: {}", reason);
    }
    while blue_score()? < deadline {
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("Alice took it back in {}", node.broadcast(&refund)?);
    Ok(())
}
