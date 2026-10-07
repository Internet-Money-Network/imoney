//! Key handling and transaction signing for wallets that run in the browser.
//!
//! Everything here runs on the user's device. A wallet built on it sends a node only signed
//! transactions and public addresses; private keys and recovery phrases never leave the page.

use bip39::Mnemonic;
use ed25519_dalek::SigningKey;
use imoney_core::constants::MIN_RELAY_FEE_PER_BYTE;
use imoney_core::{Address, AddressType, Encode, Hash, Network, Outpoint, ScriptPublicKey, Transaction, TxOutput};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// Fee rate wallets pay when none is given: twice the default relay minimum, in atoms per byte.
pub const DEFAULT_FEE_PER_BYTE: u64 = 2 * MIN_RELAY_FEE_PER_BYTE;

/// A wallet's keys, as handed to the page.
#[derive(Debug, Serialize)]
pub struct WalletKeys {
    pub private_key_hex: String,
    pub address: String,
}

/// An unspent output as the node's `/address/:addr/utxos` endpoint reports it.
#[derive(Deserialize)]
struct UtxoItem {
    transaction_id: String,
    index: u32,
    value_atoms: u64,
    /// Absent on older nodes; a missing flag is treated as spendable.
    spendable: Option<bool>,
}

/// A signed transaction ready for the node's `/tx/broadcast` endpoint.
#[derive(Debug, Serialize)]
pub struct SignedTransaction {
    pub tx_id: String,
    pub transaction: Transaction,
    pub inputs_used: usize,
    /// The fee the transaction pays, in atoms.
    pub fee_atoms: u64,
}

fn network(testnet: bool) -> Network {
    if testnet {
        Network::Testnet
    } else {
        Network::Mainnet
    }
}

fn signing_key(private_key_hex: &str) -> Result<SigningKey, String> {
    let bytes: [u8; 32] = hex::decode(private_key_hex.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| "Private key must be 64 hex characters".to_string())?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn address_of(key: &SigningKey, testnet: bool) -> Address {
    Address::from_public_key(network(testnet), AddressType::PubKeyHash, key.verifying_key().as_bytes())
}

fn keys(key: &SigningKey, testnet: bool) -> WalletKeys {
    WalletKeys { private_key_hex: hex::encode(key.to_bytes()), address: address_of(key, testnet).to_string() }
}

fn parse_address(address: &str, testnet: bool, what: &str) -> Result<Address, String> {
    let parsed = Address::decode(address.trim()).map_err(|e| format!("Invalid {} address: {}", what, e))?;
    if parsed.network != network(testnet) {
        return Err(format!("The {} address belongs to a different network", what));
    }
    Ok(parsed)
}

/// The owner's spendable outputs, parsed from the node's JSON.
fn spendable_utxos(utxos_json: &str, owner: &Address) -> Result<Vec<(Outpoint, TxOutput)>, String> {
    let items: Vec<UtxoItem> = serde_json::from_str(utxos_json).map_err(|e| format!("Invalid UTXO list: {}", e))?;
    let script = ScriptPublicKey::pay_to_address(owner);
    items
        .into_iter()
        .filter(|item| item.spendable != Some(false))
        .map(|item| {
            let transaction_id =
                Hash::from_hex(&item.transaction_id).map_err(|_| "Invalid transaction ID in UTXO list".to_string())?;
            Ok((
                Outpoint { transaction_id, index: item.index },
                TxOutput { value_atoms: item.value_atoms, script_public_key: script.clone() },
            ))
        })
        .collect()
}

fn signed(tx: Transaction, fee_atoms: u64) -> SignedTransaction {
    SignedTransaction { tx_id: tx.id().to_hex(), inputs_used: tx.inputs.len(), transaction: tx, fee_atoms }
}

/// The wallet logic, kept free of JavaScript types so it can be tested natively.
pub mod wallet {
    use super::*;

    /// A fresh 12-word recovery phrase from the platform's secure random source.
    pub fn new_mnemonic() -> Result<String, String> {
        let mut entropy = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut entropy);
        Ok(Mnemonic::from_entropy(&entropy).map_err(|e| e.to_string())?.to_string())
    }

    /// Derives the wallet's key from a recovery phrase. The same phrase always gives the same key.
    pub fn keys_from_mnemonic(phrase: &str, testnet: bool) -> Result<WalletKeys, String> {
        let mnemonic = Mnemonic::parse(phrase.trim()).map_err(|e| format!("Invalid recovery phrase: {}", e))?;
        let seed = mnemonic.to_seed("");
        let key_bytes = blake3::derive_key("IMN 2026 wallet key 0", &seed);
        Ok(keys(&SigningKey::from_bytes(&key_bytes), testnet))
    }

    pub fn keys_from_private_key(private_key_hex: &str, testnet: bool) -> Result<WalletKeys, String> {
        Ok(keys(&signing_key(private_key_hex)?, testnet))
    }

    pub fn is_valid_address(address: &str, testnet: bool) -> bool {
        parse_address(address, testnet, "given").is_ok()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_payment(
        private_key_hex: &str,
        testnet: bool,
        recipient: &str,
        amount_atoms: u64,
        fee_atoms: u64,
        utxos_json: &str,
        service_address: Option<&str>,
        invoice_id: Option<&str>,
    ) -> Result<SignedTransaction, String> {
        let key = signing_key(private_key_hex)?;
        let recipient = parse_address(recipient, testnet, "recipient")?;
        let service = service_address.map(|a| parse_address(a, testnet, "service")).transpose()?;
        let utxos = spendable_utxos(utxos_json, &address_of(&key, testnet))?;
        let build = |fee: u64| {
            Transaction::build_invoice_payment(
                &key,
                network(testnet),
                &recipient,
                amount_atoms,
                fee,
                utxos.clone(),
                service.as_ref(),
                invoice_id.filter(|id| !id.is_empty()),
            )
        };
        if fee_atoms > 0 {
            return Ok(signed(build(fee_atoms)?, fee_atoms));
        }

        // No fee given: pay for the transaction's actual size. A higher fee can pull in another
        // input and make the transaction larger, so repeat until the fee covers the size.
        let mut fee = DEFAULT_FEE_PER_BYTE * 300;
        for _ in 0..8 {
            let tx = build(fee)?;
            let needed = DEFAULT_FEE_PER_BYTE * tx.to_bytes().len() as u64;
            if fee >= needed {
                return Ok(signed(tx, fee));
            }
            fee = needed;
        }
        Err("Could not settle on a fee for this payment".to_string())
    }

    /// Merges the wallet's smallest spendable outputs (up to the per-transaction limit) into one.
    pub fn build_consolidation(
        private_key_hex: &str,
        testnet: bool,
        fee_atoms: u64,
        utxos_json: &str,
        service_address: Option<&str>,
    ) -> Result<SignedTransaction, String> {
        let key = signing_key(private_key_hex)?;
        let service = service_address.map(|a| parse_address(a, testnet, "service")).transpose()?;
        let mut utxos = spendable_utxos(utxos_json, &address_of(&key, testnet))?;
        utxos.sort_by_key(|(_, output)| output.value_atoms);
        utxos.truncate(imoney_core::transaction::MAX_CONSOLIDATION_INPUTS);
        // No fee given: about 144 bytes per merged output plus a fixed part
        let fee = if fee_atoms > 0 { fee_atoms } else { DEFAULT_FEE_PER_BYTE * (160 + 144 * utxos.len() as u64) };
        let tx = Transaction::build_consolidation(&key, network(testnet), fee, utxos, service.as_ref())?;
        Ok(signed(tx, fee))
    }
}

fn to_json<T: Serialize>(result: Result<T, String>) -> Result<String, JsError> {
    let value = result.map_err(|e| JsError::new(&e))?;
    serde_json::to_string(&value).map_err(|e| JsError::new(&e.to_string()))
}

/// Returns a new 12-word recovery phrase.
#[wasm_bindgen]
pub fn new_mnemonic() -> Result<String, JsError> {
    wallet::new_mnemonic().map_err(|e| JsError::new(&e))
}

/// Returns `{ private_key_hex, address }` as JSON for a recovery phrase.
#[wasm_bindgen]
pub fn keys_from_mnemonic(phrase: &str, testnet: bool) -> Result<String, JsError> {
    to_json(wallet::keys_from_mnemonic(phrase, testnet))
}

/// Returns `{ private_key_hex, address }` as JSON for a hex private key.
#[wasm_bindgen]
pub fn keys_from_private_key(private_key_hex: &str, testnet: bool) -> Result<String, JsError> {
    to_json(wallet::keys_from_private_key(private_key_hex, testnet))
}

#[wasm_bindgen]
pub fn is_valid_address(address: &str, testnet: bool) -> bool {
    wallet::is_valid_address(address, testnet)
}

/// Builds and signs a payment. Returns `{ tx_id, transaction, inputs_used, fee_atoms }` as JSON;
/// post `{ transaction }` to the node's `/api/v1/tx/broadcast`. Pass a fee of 0 to have the fee
/// set from the transaction's size.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn build_payment(
    private_key_hex: &str,
    testnet: bool,
    recipient: &str,
    amount_atoms: u64,
    fee_atoms: u64,
    utxos_json: &str,
    service_address: Option<String>,
    invoice_id: Option<String>,
) -> Result<String, JsError> {
    to_json(wallet::build_payment(
        private_key_hex,
        testnet,
        recipient,
        amount_atoms,
        fee_atoms,
        utxos_json,
        service_address.as_deref(),
        invoice_id.as_deref(),
    ))
}

/// Builds and signs a transaction merging the wallet's small outputs into one.
#[wasm_bindgen]
pub fn build_consolidation(
    private_key_hex: &str,
    testnet: bool,
    fee_atoms: u64,
    utxos_json: &str,
    service_address: Option<String>,
) -> Result<String, JsError> {
    to_json(wallet::build_consolidation(private_key_hex, testnet, fee_atoms, utxos_json, service_address.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::wallet::*;
    use crate::DEFAULT_FEE_PER_BYTE;
    use imoney_core::{Address, Encode, Network, ScriptPublicKey, Transaction};

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn utxos(values: &[u64]) -> String {
        let items: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, v)| format!(r#"{{"transaction_id":"{}","index":{},"value_atoms":{},"value_imn":0,"spendable":true}}"#, "ab".repeat(32), i, v))
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn recovery_phrase_always_restores_the_same_wallet() {
        let phrase = new_mnemonic().unwrap();
        assert_eq!(phrase.split_whitespace().count(), 12);
        assert_ne!(phrase, new_mnemonic().unwrap());

        let first = keys_from_mnemonic(PHRASE, true).unwrap();
        let again = keys_from_mnemonic(&format!("  {}  ", PHRASE), true).unwrap();
        assert_eq!(first.private_key_hex, again.private_key_hex);
        assert!(first.address.starts_with("imntest:q"));
        assert_eq!(keys_from_private_key(&first.private_key_hex, true).unwrap().address, first.address);
        // Same key, other network: same key bytes, different address prefix
        assert!(keys_from_mnemonic(PHRASE, false).unwrap().address.starts_with("imn:q"));

        assert!(keys_from_mnemonic("not a real phrase", true).is_err());
        assert!(keys_from_private_key("abcd", true).is_err());
    }

    #[test]
    fn payment_built_in_the_wallet_verifies_like_any_other() {
        let me = keys_from_mnemonic(PHRASE, true).unwrap();
        let shop = keys_from_private_key(&"11".repeat(32), true).unwrap();
        let node = keys_from_private_key(&"22".repeat(32), true).unwrap();

        let signed = build_payment(
            &me.private_key_hex,
            true,
            &shop.address,
            60_000,
            1_000,
            &utxos(&[50_000, 50_000]),
            Some(&node.address),
            Some("INV-7"),
        )
        .unwrap();
        let tx = &signed.transaction;
        assert_eq!(signed.inputs_used, 2);
        assert_eq!(signed.tx_id, tx.id().to_hex());
        assert_eq!(tx.invoice_id(), Some("INV-7"));
        assert_eq!(tx.outputs[0].value_atoms, 60_000);
        assert_eq!(tx.outputs[1].value_atoms, 39_000); // change
        assert!(tx.service.is_some());

        let my_script = ScriptPublicKey::pay_to_address(&Address::decode(&me.address).unwrap());
        for i in 0..tx.inputs.len() {
            assert!(tx.verify_input(Network::Testnet, i, &my_script).is_ok());
        }
        // The JSON a page posts to the node parses back into the same transaction
        let json = serde_json::to_string(&signed).unwrap();
        let posted: serde_json::Value = serde_json::from_str(&json).unwrap();
        let round_trip: Transaction = serde_json::from_value(posted["transaction"].clone()).unwrap();
        assert_eq!(&round_trip, tx);
    }

    #[test]
    fn automatic_fee_covers_the_size_even_when_it_pulls_in_more_inputs() {
        let me = keys_from_mnemonic(PHRASE, true).unwrap();
        let shop = keys_from_private_key(&"11".repeat(32), true).unwrap();
        let fee_of = |amount: u64, coins: &[u64]| {
            let signed = build_payment(&me.private_key_hex, true, &shop.address, amount, 0, &utxos(coins), None, None).unwrap();
            let size = signed.transaction.to_bytes().len() as u64;
            assert!(signed.fee_atoms >= DEFAULT_FEE_PER_BYTE * size, "fee {} for {} bytes", signed.fee_atoms, size);
            assert!(signed.fee_atoms <= DEFAULT_FEE_PER_BYTE * (size + 300), "fee {} for {} bytes", signed.fee_atoms, size);
            let paid_in: u64 = signed.inputs_used as u64 * coins[0];
            let paid_out: u64 = signed.transaction.outputs.iter().map(|o| o.value_atoms).sum();
            assert_eq!(paid_in - paid_out, signed.fee_atoms);
            signed
        };

        assert_eq!(fee_of(10_000, &[1_000_000]).inputs_used, 1);
        // 20 small coins: the fee grows with every input needed to pay it
        let many = fee_of(100_000, &[10_000; 20]);
        assert!(many.inputs_used > 10 && many.inputs_used <= 18, "{}", many.inputs_used);
        // When the coins cannot cover the amount plus the fee they imply, say so
        let short = build_payment(&me.private_key_hex, true, &shop.address, 150_000, 0, &utxos(&[10_000; 20]), None, None);
        assert!(short.unwrap_err().contains("Insufficient"));

        let merged = build_consolidation(&me.private_key_hex, true, 0, &utxos(&[90_000, 90_000, 90_000]), None).unwrap();
        assert!(merged.fee_atoms >= DEFAULT_FEE_PER_BYTE * merged.transaction.to_bytes().len() as u64);
    }

    #[test]
    fn wallet_refuses_bad_input() {
        let me = keys_from_mnemonic(PHRASE, true).unwrap();
        let shop = keys_from_private_key(&"11".repeat(32), true).unwrap();
        let mainnet_shop = keys_from_private_key(&"11".repeat(32), false).unwrap();
        let pay = |to: &str, amount: u64, list: &str| build_payment(&me.private_key_hex, true, to, amount, 1_000, list, None, None);

        assert!(pay(&shop.address, 60_000, &utxos(&[50_000])).unwrap_err().contains("Insufficient"));
        assert!(pay(&mainnet_shop.address, 1_000, &utxos(&[50_000])).unwrap_err().contains("different network"));
        assert!(pay("imntest:nonsense", 1_000, &utxos(&[50_000])).is_err());
        assert!(pay(&shop.address, 1_000, "not json").is_err());
        assert!(is_valid_address(&shop.address, true));
        assert!(!is_valid_address(&shop.address, false));

        // Immature rewards are left alone
        let immature = r#"[{"transaction_id":"abababababababababababababababababababababababababababababababab","index":0,"value_atoms":90000,"spendable":false}]"#;
        assert!(pay(&shop.address, 1_000, immature).unwrap_err().contains("Insufficient"));
    }

    #[test]
    fn consolidation_merges_the_smallest_outputs() {
        let me = keys_from_mnemonic(PHRASE, true).unwrap();
        let signed = build_consolidation(&me.private_key_hex, true, 2_000, &utxos(&[5_000, 7_000, 9_000]), None).unwrap();
        assert_eq!(signed.inputs_used, 3);
        assert_eq!(signed.transaction.outputs.len(), 1);
        assert_eq!(signed.transaction.outputs[0].value_atoms, 19_000);
    }
}
