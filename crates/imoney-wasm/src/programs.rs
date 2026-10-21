//! Escrow, subscriptions and vaults for the browser wallet.
//!
//! Each program is described by its terms, passed around as hex (`terms_hex`) so the parties can
//! share them. A program's coins are found by asking the node for the coins at its address;
//! one entry of that listing (`coin_json`) is what the spending functions take.

use crate::{address_of, hex_array, network, signed, signing_key, TokenItem};
use imoney_core::{
    Address, AddressType, EscrowRole, EscrowScript, Hash, Outpoint, PendingWithdrawal, ScriptPublicKey, SubscriptionScript, Transaction,
    TxOutput, VaultScript,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// What a program step pays in fee: these transactions have one input and one or two outputs.
const PROGRAM_FEE_ATOMS: u64 = 600_000;

#[derive(Serialize)]
pub struct Program {
    pub terms_hex: String,
    /// Where the program's coins are, to pay into it and to look up its coins.
    pub address: String,
}

#[derive(Deserialize)]
struct CoinItem {
    transaction_id: String,
    index: u32,
    value_atoms: u64,
    #[serde(default)]
    token: Option<TokenItem>,
}

fn coin(coin_json: &str, script: ScriptPublicKey) -> Result<(Outpoint, TxOutput), String> {
    let item: CoinItem = serde_json::from_str(coin_json).map_err(|e| format!("Invalid coin: {}", e))?;
    let transaction_id = Hash::from_hex(&item.transaction_id).map_err(|_| "Invalid transaction ID".to_string())?;
    let token = item.token.as_ref().map(TokenItem::parse).transpose()?;
    Ok((Outpoint { transaction_id, index: item.index }, TxOutput { value_atoms: item.value_atoms, script_public_key: script, token }))
}

fn terms_bytes(terms_hex: &str) -> Result<Vec<u8>, String> {
    hex::decode(terms_hex.trim()).map_err(|_| "Those terms are not valid".to_string())
}

fn program(terms: Vec<u8>, address: Address) -> Program {
    Program { terms_hex: hex::encode(terms), address: address.to_string() }
}

fn to_js<T: Serialize>(result: Result<T, String>) -> Result<String, JsError> {
    crate::to_json(result)
}

// ---------------------------------------------------------------- escrow

/// Escrow terms from the three parties' wallet codes (public keys) and a deadline in blue score.
/// After the deadline the seller collects when `seller_collects`, else the buyer gets it back.
#[wasm_bindgen]
pub fn escrow_terms(
    buyer_code: &str,
    seller_code: &str,
    referee_code: &str,
    deadline: u64,
    seller_collects: bool,
    testnet: bool,
) -> Result<String, JsError> {
    to_js((|| {
        let script = EscrowScript::new(
            hex_array(buyer_code, "The buyer's code")?,
            hex_array(seller_code, "The seller's code")?,
            hex_array(referee_code, "The referee's code")?,
            deadline,
            seller_collects,
        )
        .map_err(|e| e.to_string())?;
        Ok(program(script.to_bytes(), script.address(network(testnet))))
    })())
}

/// Describes escrow terms: the parties' addresses, the deadline and who collects after it.
#[wasm_bindgen]
pub fn escrow_describe(terms_hex: &str, testnet: bool) -> Result<String, JsError> {
    to_js((|| {
        let script = EscrowScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        let net = network(testnet);
        let person = |key: &[u8; 32]| Address::from_public_key(net, AddressType::PubKeyHash, key).to_string();
        Ok(serde_json::json!({
            "address": script.address(net).to_string(),
            "buyer": person(&script.buyer),
            "seller": person(&script.seller),
            "referee": person(&script.arbiter),
            "buyer_code": hex::encode(script.buyer),
            "seller_code": hex::encode(script.seller),
            "referee_code": hex::encode(script.arbiter),
            "deadline": script.deadline,
            "seller_collects_after_deadline": script.seller_collects_after_deadline,
        }))
    })())
}

/// Moves an escrow coin. `action` is `release` (buyer pays the seller), `refund` (seller pays
/// the buyer back), `decide` (the referee pays `to_buyer_atoms` to the buyer and the rest to
/// the seller) or `collect` (after the deadline, the party the terms name).
#[wasm_bindgen]
pub fn escrow_act(
    private_key_hex: &str,
    testnet: bool,
    terms_hex: &str,
    coin_json: &str,
    action: &str,
    to_buyer_atoms: u64,
) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        let script = EscrowScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        let net = network(testnet);
        let held = coin(coin_json, ScriptPublicKey::pay_to_address(&script.address(net)))?;
        let (role, split) = match action {
            "release" => (EscrowRole::Buyer, None),
            "refund" => (EscrowRole::Seller, None),
            "decide" => (EscrowRole::Arbiter, Some(to_buyer_atoms)),
            "collect" => (EscrowRole::Deadline, None),
            other => return Err(format!("Unknown escrow action '{}'", other)),
        };
        let tx = Transaction::spend_escrow(&script, net, held, role, &key, PROGRAM_FEE_ATOMS, split)?;
        Ok(signed(tx, PROGRAM_FEE_ATOMS))
    })())
}

// ---------------------------------------------------------------- subscriptions

/// Subscription terms. `first_due` is the blue score of the first collection; a period is in
/// blue score too (about 17,280 a day). `reference` is any text the merchant chooses.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn subscription_terms(
    customer_code: &str,
    merchant_code: &str,
    amount_per_period: u64,
    period: u64,
    first_due: u64,
    reference: &str,
    testnet: bool,
) -> Result<String, JsError> {
    to_js((|| {
        let script = SubscriptionScript::new(
            hex_array(customer_code, "The customer's code")?,
            hex_array(merchant_code, "The shop's code")?,
            amount_per_period,
            period,
            first_due,
            *blake3::hash(reference.as_bytes()).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        Ok(program(script.to_bytes(), script.address(network(testnet))))
    })())
}

/// The same subscription `periods` periods later: where its money sits after that many
/// collections. A wallet looks for the money at each in turn.
#[wasm_bindgen]
pub fn subscription_after(terms_hex: &str, periods: u32, testnet: bool) -> Result<String, JsError> {
    to_js((|| {
        let mut script = SubscriptionScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        for _ in 0..periods {
            script = script.next().map_err(|e| e.to_string())?;
        }
        Ok(serde_json::json!({
            "terms_hex": hex::encode(script.to_bytes()),
            "address": script.address(network(testnet)).to_string(),
            "next_due": script.next_due,
            "amount_per_period": script.amount_per_period,
            "period": script.period,
            "customer": Address::from_public_key(network(testnet), AddressType::PubKeyHash, &script.customer).to_string(),
            "merchant": Address::from_public_key(network(testnet), AddressType::PubKeyHash, &script.merchant).to_string(),
        }))
    })())
}

/// The shop collects one period's amount. `terms_hex` must be the terms the coin is at.
#[wasm_bindgen]
pub fn subscription_collect(private_key_hex: &str, testnet: bool, terms_hex: &str, coin_json: &str) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        let script = SubscriptionScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        let held = coin(coin_json, script.locking_script())?;
        Ok(signed(Transaction::collect_subscription(&script, network(testnet), held, &key, PROGRAM_FEE_ATOMS)?, PROGRAM_FEE_ATOMS))
    })())
}

/// The customer cancels and takes the money back to their own address.
#[wasm_bindgen]
pub fn subscription_cancel(private_key_hex: &str, testnet: bool, terms_hex: &str, coin_json: &str) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        let script = SubscriptionScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        let held = coin(coin_json, script.locking_script())?;
        let home = address_of(&key, testnet);
        Ok(signed(Transaction::cancel_subscription(&script, network(testnet), held, &key, &home, PROGRAM_FEE_ATOMS)?, PROGRAM_FEE_ATOMS))
    })())
}

// ---------------------------------------------------------------- vaults

/// Vault terms from the everyday wallet's code, a backup code and a delay in blue score.
#[wasm_bindgen]
pub fn vault_terms(everyday_code: &str, backup_code: &str, delay: u64, testnet: bool) -> Result<String, JsError> {
    to_js((|| {
        let script = VaultScript::new(hex_array(everyday_code, "The wallet's code")?, hex_array(backup_code, "The backup code")?, delay)
            .map_err(|e| e.to_string())?;
        Ok(program(script.to_bytes(), script.address(network(testnet))))
    })())
}

fn pending_address(pending: &PendingWithdrawal, testnet: bool) -> Address {
    Address::new(network(testnet), AddressType::VaultPending, Hash(pending.locking_script().script.try_into().expect("32 bytes")))
}

/// Starts a withdrawal from the vault to `to`, with the everyday key. Returns the signed
/// transaction, plus the withdrawal's terms and address to keep for completing or stopping it.
#[wasm_bindgen]
pub fn vault_start(private_key_hex: &str, testnet: bool, terms_hex: &str, coin_json: &str, to: &str, now: u64) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        let vault = VaultScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        let to = Address::decode(to.trim()).map_err(|e| format!("Invalid address: {}", e))?;
        let held = coin(coin_json, vault.locking_script())?;
        let (tx, pending) = Transaction::start_vault_withdrawal(&vault, network(testnet), held, &key, &to, now, PROGRAM_FEE_ATOMS)?;
        Ok(serde_json::json!({
            "signed": signed(tx, PROGRAM_FEE_ATOMS),
            "pending_hex": hex::encode(pending.to_bytes()),
            "pending_address": pending_address(&pending, testnet).to_string(),
            "unlock_at": pending.unlock_at,
        }))
    })())
}

/// Completes a withdrawal that has waited long enough. Needs no key.
#[wasm_bindgen]
pub fn vault_complete(pending_hex: &str, coin_json: &str) -> Result<String, JsError> {
    to_js((|| {
        let pending = PendingWithdrawal::from_bytes(&terms_bytes(pending_hex)?).map_err(|e| e.to_string())?;
        let held = coin(coin_json, pending.locking_script())?;
        Ok(signed(Transaction::complete_vault_withdrawal(&pending, held, PROGRAM_FEE_ATOMS)?, PROGRAM_FEE_ATOMS))
    })())
}

/// With the backup key: stops a pending withdrawal (give `pending_hex`) or moves vault coins
/// (leave it empty), sending everything to the backup key's own address.
#[wasm_bindgen]
pub fn vault_recover(
    backup_private_key_hex: &str,
    testnet: bool,
    terms_hex: &str,
    pending_hex: &str,
    coin_json: &str,
) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(backup_private_key_hex)?;
        let vault = VaultScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        let pending = if pending_hex.trim().is_empty() {
            None
        } else {
            Some(PendingWithdrawal::from_bytes(&terms_bytes(pending_hex)?).map_err(|e| e.to_string())?)
        };
        let script = pending.as_ref().map_or_else(|| vault.locking_script(), |p| p.locking_script());
        let held = coin(coin_json, script)?;
        let safe = address_of(&key, testnet);
        let tx = Transaction::recover_vault(&vault, pending.as_ref(), network(testnet), held, &key, &safe, PROGRAM_FEE_ATOMS)?;
        Ok(signed(tx, PROGRAM_FEE_ATOMS))
    })())
}

/// The vault address for these terms, and the address a pending withdrawal sits at.
#[wasm_bindgen]
pub fn vault_describe(terms_hex: &str, testnet: bool) -> Result<String, JsError> {
    to_js((|| {
        let vault = VaultScript::from_bytes(&terms_bytes(terms_hex)?).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "address": vault.address(network(testnet)).to_string(),
            "delay": vault.delay,
            "backup": Address::from_public_key(network(testnet), AddressType::PubKeyHash, &vault.backup).to_string(),
        }))
    })())
}

/// A pending withdrawal's address and unlock time, from its terms (as shared in a vault's deal code).
#[wasm_bindgen]
pub fn vault_pending_describe(pending_hex: &str, testnet: bool) -> Result<String, JsError> {
    to_js((|| {
        let pending = PendingWithdrawal::from_bytes(&terms_bytes(pending_hex)?).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "hex": hex::encode(pending.to_bytes()),
            "address": pending_address(&pending, testnet).to_string(),
            "unlock_at": pending.unlock_at,
        }))
    })())
}

// ---------------------------------------------------------------- swap offers

fn evm_address(text: &str, what: &str) -> Result<[u8; 20], String> {
    hex_array(text.trim().trim_start_matches("0x"), what)
}

/// Lists `atoms` of IMN for sale against a token on another chain: `chain_id` (8453 for Base),
/// the token's contract and the address to be paid at, as 0x addresses. `atoms` cost
/// `atoms * price_num / price_den` of the token's smallest units; `min_atoms` is the least sold
/// in one go.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn swap_offer_create(
    private_key_hex: &str,
    testnet: bool,
    chain_id: u64,
    token: &str,
    pay_to: &str,
    price_num: u64,
    price_den: u64,
    min_atoms: u64,
    atoms: u64,
    utxos_json: &str,
) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        let terms = imoney_core::swap_offer::SwapOfferTerms::new(
            key.verifying_key().to_bytes(),
            chain_id,
            evm_address(token, "The token's contract")?,
            evm_address(pay_to, "The address to be paid at")?,
            price_num,
            price_den,
            min_atoms,
        )
        .map_err(|e| e.to_string())?;
        let utxos = crate::spendable_utxos(utxos_json, &address_of(&key, testnet))?;
        crate::with_fee(0, |fee| Transaction::build_swap_offer(&key, network(testnet), &terms, atoms, fee, utxos.clone()))
    })())
}

/// The swap offer coin a node lists (one entry of `/api/v1/swap-offers`).
fn swap_offer_coin(offer_json: &str) -> Result<(Outpoint, TxOutput), String> {
    #[derive(Deserialize)]
    struct Listed {
        transaction_id: String,
        index: u32,
        atoms: u64,
        terms_hex: String,
    }
    let listed: Listed = serde_json::from_str(offer_json).map_err(|e| format!("Invalid offer: {}", e))?;
    let terms = imoney_core::swap_offer::SwapOfferTerms::from_bytes(&terms_bytes(&listed.terms_hex)?).map_err(|e| e.to_string())?;
    let transaction_id = Hash::from_hex(&listed.transaction_id).map_err(|_| "Invalid transaction ID".to_string())?;
    Ok((Outpoint { transaction_id, index: listed.index }, TxOutput::new(listed.atoms, terms.script())))
}

/// The hash lock a swap fills into: from the buyer's and maker's codes, the hash of the buyer's
/// secret, and the blue score from which the maker may take it back. Both sides compute it the
/// same way, so the buyer knows where to look for their IMN.
#[wasm_bindgen]
pub fn swap_lock(buyer_code: &str, maker_code: &str, hash_hex: &str, refund_after: u64, testnet: bool) -> Result<String, JsError> {
    to_js((|| {
        let lock = imoney_core::HashLockScript::new(
            hex_array(buyer_code, "The buyer's code")?,
            hex_array(maker_code, "The maker's code")?,
            hex_array(hash_hex, "The hash")?,
            refund_after,
        )
        .map_err(|e| e.to_string())?;
        Ok(program(lock.to_bytes(), lock.address(network(testnet))))
    })())
}

/// With the maker's key: fills `atoms` of a swap offer for a buyer whose payment is locked on
/// the other chain, by locking them for the buyer under the same hash.
#[wasm_bindgen]
pub fn swap_offer_fill(
    private_key_hex: &str,
    testnet: bool,
    offer_json: &str,
    atoms: u64,
    buyer_code: &str,
    hash_hex: &str,
    refund_after: u64,
) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        let (tx, _) = Transaction::fill_swap_offer(
            &key,
            network(testnet),
            swap_offer_coin(offer_json)?,
            atoms,
            hex_array(buyer_code, "The buyer's code")?,
            hex_array(hash_hex, "The hash")?,
            refund_after,
            PROGRAM_FEE_ATOMS,
        )?;
        Ok(signed(tx, PROGRAM_FEE_ATOMS))
    })())
}

/// With the maker's key: takes a swap offer off the order book and back to the wallet.
#[wasm_bindgen]
pub fn swap_offer_cancel(private_key_hex: &str, testnet: bool, offer_json: &str) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        Ok(signed(
            Transaction::cancel_swap_offer(&key, network(testnet), swap_offer_coin(offer_json)?, PROGRAM_FEE_ATOMS)?,
            PROGRAM_FEE_ATOMS,
        ))
    })())
}

/// Claims a hash lock with its secret (the buyer), or takes it back after its refund time
/// (the maker: leave `secret_hex` empty), to the key's own address.
#[wasm_bindgen]
pub fn swap_lock_spend(private_key_hex: &str, testnet: bool, lock_hex: &str, coin_json: &str, secret_hex: &str) -> Result<String, JsError> {
    to_js((|| {
        let key = signing_key(private_key_hex)?;
        let lock = imoney_core::HashLockScript::from_bytes(&terms_bytes(lock_hex)?).map_err(|e| e.to_string())?;
        let held = coin(coin_json, ScriptPublicKey::pay_to_address(&lock.address(network(testnet))))?;
        let secret = if secret_hex.trim().is_empty() { None } else { Some(hex_array(secret_hex, "The secret")?) };
        let tx = Transaction::spend_hashlock(&lock, network(testnet), held, &address_of(&key, testnet), PROGRAM_FEE_ATOMS, &key, secret)?;
        Ok(signed(tx, PROGRAM_FEE_ATOMS))
    })())
}
