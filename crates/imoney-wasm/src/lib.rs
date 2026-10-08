//! Key handling and transaction signing for wallets that run in the browser.
//!
//! Everything here runs on the user's device. A wallet built on it sends a node only signed
//! transactions and public addresses; private keys and recovery phrases never leave the page.

use bip39::Mnemonic;
use ed25519_dalek::SigningKey;
use imoney_core::constants::MIN_RELAY_FEE_PER_BYTE;
use imoney_core::{
    Address, AddressType, Encode, Hash, MultisigScript, Network, Outpoint, PartialSignature, ScriptPublicKey, Transaction,
    TxOutput,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// Fee rate wallets pay when none is given: twice the default relay minimum, in atoms per byte.
pub const DEFAULT_FEE_PER_BYTE: u64 = 2 * MIN_RELAY_FEE_PER_BYTE;

/// A wallet's keys, as handed to the page.
#[derive(Debug, Serialize)]
pub struct WalletKeys {
    pub private_key_hex: String,
    /// What co-signers need from this wallet to set up a shared wallet with it.
    pub public_key_hex: String,
    pub address: String,
}

/// A shared wallet: coins that need `threshold` of `public_keys` to move.
#[derive(Debug, Serialize)]
pub struct SharedWallet {
    pub address: String,
    pub threshold: u8,
    pub public_keys: Vec<String>,
    /// Everything a co-signer's wallet needs to open the same shared wallet.
    pub script_hex: String,
}

/// One co-signer's signature on one input of a proposed payment.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProposalSignature {
    pub public_key: String,
    pub signature: String,
}

/// A payment from a shared wallet, passed between co-signers until enough have signed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    pub script_hex: String,
    pub testnet: bool,
    pub transaction: Transaction,
    /// Signatures collected so far, per input.
    pub signatures: Vec<Vec<ProposalSignature>>,
}

/// What a proposal does, worked out from the proposal and the coins the reader's own node
/// reports for the shared wallet. Nothing in it is taken on the proposer's word.
#[derive(Debug, Serialize)]
pub struct ProposalSummary {
    pub from: String,
    pub threshold: u8,
    /// Where the money goes, leaving out change returned to the shared wallet.
    pub payments: Vec<ProposalPayment>,
    pub change_atoms: u64,
    pub fee_atoms: u64,
    pub invoice_id: Option<String>,
    /// Public keys that have signed every input.
    pub signed_by: Vec<String>,
    pub ready: bool,
}

#[derive(Debug, Serialize)]
pub struct ProposalPayment {
    pub address: Option<String>,
    pub amount_atoms: u64,
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
    WalletKeys {
        private_key_hex: hex::encode(key.to_bytes()),
        public_key_hex: hex::encode(key.verifying_key().to_bytes()),
        address: address_of(key, testnet).to_string(),
    }
}

fn hex_array<const N: usize>(text: &str, what: &str) -> Result<[u8; N], String> {
    hex::decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("{} must be {} hex characters", what, 2 * N))
}

fn script_from_hex(script_hex: &str) -> Result<MultisigScript, String> {
    let bytes = hex::decode(script_hex.trim()).map_err(|_| "The shared wallet code is not valid".to_string())?;
    MultisigScript::from_bytes(&bytes).map_err(|e| e.to_string())
}

fn shared_wallet(script: &MultisigScript, testnet: bool) -> SharedWallet {
    SharedWallet {
        address: script.address(network(testnet)).to_string(),
        threshold: script.threshold(),
        public_keys: script.keys().iter().map(hex::encode).collect(),
        script_hex: hex::encode(script.to_bytes()),
    }
}

/// The proposal's signatures for one input, as the transaction code takes them.
fn partial_signatures(entries: &[ProposalSignature]) -> Result<Vec<PartialSignature>, String> {
    entries
        .iter()
        .map(|entry| {
            Ok(PartialSignature {
                public_key: hex_array(&entry.public_key, "A public key")?,
                signature: hex_array(&entry.signature, "A signature")?,
            })
        })
        .collect()
}

/// Checks a proposal against the coins the reader's node reports for the shared wallet, and
/// returns the script and the total value of the coins it spends.
fn check_proposal(proposal: &Proposal, utxos_json: &str) -> Result<(MultisigScript, u64), String> {
    let script = script_from_hex(&proposal.script_hex)?;
    let vault = script.address(network(proposal.testnet));
    let coins = spendable_utxos(utxos_json, &vault)?;
    let tx = &proposal.transaction;
    if tx.inputs.is_empty() || proposal.signatures.len() != tx.inputs.len() {
        return Err("The proposal is malformed".to_string());
    }
    let mut total: u64 = 0;
    for input in &tx.inputs {
        let (_, coin) = coins
            .iter()
            .find(|(outpoint, _)| *outpoint == input.previous_outpoint)
            .ok_or_else(|| "The proposal spends a coin your node does not list for this shared wallet. It may already be spent.".to_string())?;
        total = total.checked_add(coin.value_atoms).ok_or_else(|| "Input total overflows".to_string())?;
    }
    let outputs = tx.outputs.iter().try_fold(0u64, |sum, o| sum.checked_add(o.value_atoms));
    match outputs {
        Some(outputs) if outputs <= total => Ok((script, total)),
        _ => Err("The proposal pays out more than it spends".to_string()),
    }
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

    /// Sets up a shared wallet from its co-signers' public keys (hex, in any order).
    pub fn shared_wallet_create(threshold: u8, public_keys: &[&str], testnet: bool) -> Result<SharedWallet, String> {
        let keys: Vec<[u8; 32]> =
            public_keys.iter().map(|key| hex_array(key, "A public key")).collect::<Result<_, _>>()?;
        let script = MultisigScript::new(threshold, &keys).map_err(|e| e.to_string())?;
        Ok(shared_wallet(&script, testnet))
    }

    /// Opens a shared wallet from the code another co-signer shared.
    pub fn shared_wallet_open(script_hex: &str, testnet: bool) -> Result<SharedWallet, String> {
        Ok(shared_wallet(&script_from_hex(script_hex)?, testnet))
    }

    /// Starts a payment from a shared wallet. Nobody has signed it yet. A fee of 0 sets the
    /// fee from the size the payment will have once signed.
    #[allow(clippy::too_many_arguments)]
    pub fn shared_propose(
        script_hex: &str,
        testnet: bool,
        recipient: &str,
        amount_atoms: u64,
        fee_atoms: u64,
        utxos_json: &str,
        service_address: Option<&str>,
        invoice_id: Option<&str>,
    ) -> Result<Proposal, String> {
        let script = script_from_hex(script_hex)?;
        let recipient = parse_address(recipient, testnet, "recipient")?;
        let service = service_address.map(|a| parse_address(a, testnet, "service")).transpose()?;
        let utxos = spendable_utxos(utxos_json, &script.address(network(testnet)))?;
        let build = |fee: u64| {
            Transaction::build_multisig_payment(
                &script,
                network(testnet),
                &recipient,
                amount_atoms,
                fee,
                utxos.clone(),
                invoice_id.filter(|id| !id.is_empty()),
                service.as_ref(),
            )
        };
        let proposal = |tx: Transaction| Proposal {
            script_hex: hex::encode(script.to_bytes()),
            testnet,
            signatures: vec![Vec::new(); tx.inputs.len()],
            transaction: tx,
        };
        if fee_atoms > 0 {
            return Ok(proposal(build(fee_atoms)?));
        }
        // The signatures are not there yet, so add the room they will take
        let signed_size = |tx: &Transaction| (tx.to_bytes().len() + tx.inputs.len() * script.signature_script_len()) as u64;
        let mut fee = DEFAULT_FEE_PER_BYTE * 400;
        for _ in 0..8 {
            let tx = build(fee)?;
            let needed = DEFAULT_FEE_PER_BYTE * signed_size(&tx);
            if fee >= needed {
                return Ok(proposal(tx));
            }
            fee = needed;
        }
        Err("Could not settle on a fee for this payment".to_string())
    }

    /// What a proposal would do, checked against the coins the reader's own node reports.
    pub fn shared_summary(proposal: &Proposal, utxos_json: &str) -> Result<ProposalSummary, String> {
        let (script, total) = check_proposal(proposal, utxos_json)?;
        let net = network(proposal.testnet);
        let vault = ScriptPublicKey::pay_to_address(&script.address(net));
        let tx = &proposal.transaction;

        let mut payments = Vec::new();
        let mut change_atoms: u64 = 0;
        for output in &tx.outputs {
            if output.script_public_key == vault {
                change_atoms += output.value_atoms;
            } else {
                let address = AddressType::from_u8(output.script_public_key.version)
                    .zip(<[u8; 32]>::try_from(output.script_public_key.script.as_slice()).ok())
                    .map(|(kind, hash)| Address::new(net, kind, Hash(hash)).to_string());
                payments.push(ProposalPayment { address, amount_atoms: output.value_atoms });
            }
        }
        let paid_out: u64 = tx.outputs.iter().map(|o| o.value_atoms).sum();

        // A co-signer counts only when their signature on every input is valid
        let signed_by: Vec<String> = script
            .keys()
            .iter()
            .filter(|key| {
                (0..tx.inputs.len()).all(|index| {
                    proposal.signatures[index].iter().any(|entry| {
                        hex_array::<32>(&entry.public_key, "key").ok().as_ref() == Some(*key)
                            && hex_array::<64>(&entry.signature, "signature").is_ok_and(|signature| {
                                let mut probe = tx.clone();
                                let one = MultisigScript::new(1, &[**key]).expect("one valid key");
                                probe
                                    .set_multisig_signatures(index, &one, &[PartialSignature { public_key: **key, signature }])
                                    .is_ok()
                                    && probe
                                        .verify_input(net, index, &ScriptPublicKey::pay_to_address(&one.address(net)))
                                        .is_ok()
                            })
                    })
                })
            })
            .map(hex::encode)
            .collect();

        Ok(ProposalSummary {
            from: script.address(net).to_string(),
            threshold: script.threshold(),
            payments,
            change_atoms,
            fee_atoms: total - paid_out,
            invoice_id: tx.invoice_id().map(str::to_string),
            ready: signed_by.len() >= script.threshold() as usize,
            signed_by,
        })
    }

    /// Adds this wallet's signature to a proposal. Refuses a proposal that does not check out
    /// against the coins this wallet's own node reports.
    pub fn shared_sign(proposal: &Proposal, private_key_hex: &str, utxos_json: &str) -> Result<Proposal, String> {
        let (script, _) = check_proposal(proposal, utxos_json)?;
        let key = signing_key(private_key_hex)?;
        let public_key = key.verifying_key().to_bytes();
        if !script.keys().contains(&public_key) {
            return Err("This wallet is not one of the shared wallet's co-signers".to_string());
        }
        let mut signed = proposal.clone();
        for (index, entries) in signed.signatures.iter_mut().enumerate() {
            let partial = proposal.transaction.multisig_sign(network(proposal.testnet), index, &key);
            entries.retain(|entry| entry.public_key != hex::encode(public_key));
            entries.push(ProposalSignature {
                public_key: hex::encode(partial.public_key),
                signature: hex::encode(partial.signature),
            });
        }
        Ok(signed)
    }

    /// Turns a proposal with enough signatures into a transaction ready to broadcast.
    pub fn shared_finish(proposal: &Proposal, utxos_json: &str) -> Result<SignedTransaction, String> {
        let (script, total) = check_proposal(proposal, utxos_json)?;
        let net = network(proposal.testnet);
        let vault = ScriptPublicKey::pay_to_address(&script.address(net));
        let mut tx = proposal.transaction.clone();
        for index in 0..tx.inputs.len() {
            let partials = partial_signatures(&proposal.signatures[index])?;
            tx.set_multisig_signatures(index, &script, &partials)
                .map_err(|_| format!("Not enough co-signers have signed yet: {} are needed", script.threshold()))?;
            tx.verify_input(net, index, &vault).map_err(|e| format!("A signature does not check out: {}", e))?;
        }
        let paid_out: u64 = tx.outputs.iter().map(|o| o.value_atoms).sum();
        Ok(signed(tx, total - paid_out))
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

fn parse_proposal(proposal_json: &str) -> Result<Proposal, String> {
    serde_json::from_str(proposal_json.trim()).map_err(|_| "That is not a payment proposal".to_string())
}

/// Sets up a shared wallet. `public_keys` is the co-signers' public keys separated by commas,
/// spaces or new lines. Returns `{ address, threshold, public_keys, script_hex }` as JSON.
#[wasm_bindgen]
pub fn shared_wallet_create(threshold: u8, public_keys: &str, testnet: bool) -> Result<String, JsError> {
    let keys: Vec<&str> = public_keys.split(|c: char| c == ',' || c.is_whitespace()).filter(|key| !key.is_empty()).collect();
    to_json(wallet::shared_wallet_create(threshold, &keys, testnet))
}

/// Opens a shared wallet from the code (`script_hex`) another co-signer shared.
#[wasm_bindgen]
pub fn shared_wallet_open(script_hex: &str, testnet: bool) -> Result<String, JsError> {
    to_json(wallet::shared_wallet_open(script_hex, testnet))
}

/// Starts a payment from a shared wallet and returns the proposal as JSON, unsigned.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn shared_propose(
    script_hex: &str,
    testnet: bool,
    recipient: &str,
    amount_atoms: u64,
    fee_atoms: u64,
    utxos_json: &str,
    service_address: Option<String>,
    invoice_id: Option<String>,
) -> Result<String, JsError> {
    to_json(wallet::shared_propose(
        script_hex,
        testnet,
        recipient,
        amount_atoms,
        fee_atoms,
        utxos_json,
        service_address.as_deref(),
        invoice_id.as_deref(),
    ))
}

/// Describes a proposal, checked against the shared wallet's coins as this wallet's node reports them.
#[wasm_bindgen]
pub fn shared_summary(proposal_json: &str, utxos_json: &str) -> Result<String, JsError> {
    to_json(parse_proposal(proposal_json).and_then(|proposal| wallet::shared_summary(&proposal, utxos_json)))
}

/// Adds this wallet's signature to a proposal and returns the updated proposal as JSON.
#[wasm_bindgen]
pub fn shared_sign(proposal_json: &str, private_key_hex: &str, utxos_json: &str) -> Result<String, JsError> {
    to_json(parse_proposal(proposal_json).and_then(|proposal| wallet::shared_sign(&proposal, private_key_hex, utxos_json)))
}

/// Turns a proposal with enough signatures into `{ tx_id, transaction, inputs_used, fee_atoms }`.
#[wasm_bindgen]
pub fn shared_finish(proposal_json: &str, utxos_json: &str) -> Result<String, JsError> {
    to_json(parse_proposal(proposal_json).and_then(|proposal| wallet::shared_finish(&proposal, utxos_json)))
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
    use crate::{Proposal, DEFAULT_FEE_PER_BYTE};
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
    fn shared_wallet_payment_passes_between_co_signers_until_enough_have_signed() {
        let alice = keys_from_private_key(&"a1".repeat(32), true).unwrap();
        let bob = keys_from_private_key(&"b2".repeat(32), true).unwrap();
        let carol = keys_from_private_key(&"c3".repeat(32), true).unwrap();
        let outsider = keys_from_private_key(&"d4".repeat(32), true).unwrap();
        let shop = keys_from_private_key(&"11".repeat(32), true).unwrap();

        // Each co-signer sets it up from the same public keys, in whatever order, and gets the same wallet
        let vault = shared_wallet_create(2, &[&alice.public_key_hex, &bob.public_key_hex, &carol.public_key_hex], true).unwrap();
        let again = shared_wallet_create(2, &[&carol.public_key_hex, &alice.public_key_hex, &bob.public_key_hex], true).unwrap();
        assert_eq!(vault.address, again.address);
        assert_eq!(shared_wallet_open(&vault.script_hex, true).unwrap().address, vault.address);
        assert!(shared_wallet_create(4, &[&alice.public_key_hex, &bob.public_key_hex], true).is_err());

        let coins = utxos(&[400_000, 400_000]);
        let proposal = shared_propose(&vault.script_hex, true, &shop.address, 500_000, 0, &coins, None, Some("INV-3")).unwrap();
        let summary = shared_summary(&proposal, &coins).unwrap();
        assert_eq!(summary.payments.len(), 1);
        assert_eq!(summary.payments[0].address.as_deref(), Some(shop.address.as_str()));
        assert_eq!(summary.payments[0].amount_atoms, 500_000);
        assert_eq!(summary.change_atoms + summary.fee_atoms, 300_000);
        assert_eq!(summary.invoice_id.as_deref(), Some("INV-3"));
        assert!(summary.signed_by.is_empty() && !summary.ready);

        // Not sendable yet, and an outsider cannot sign
        assert!(shared_finish(&proposal, &coins).unwrap_err().contains("Not enough"));
        assert!(shared_sign(&proposal, &outsider.private_key_hex, &coins).unwrap_err().contains("not one of"));

        // Carol signs, then Bob, on their own devices; the text passed between them is JSON
        let after_carol = shared_sign(&proposal, &carol.private_key_hex, &coins).unwrap();
        let passed_on: Proposal = serde_json::from_str(&serde_json::to_string(&after_carol).unwrap()).unwrap();
        assert_eq!(shared_summary(&passed_on, &coins).unwrap().signed_by, vec![carol.public_key_hex.clone()]);
        // Signing twice changes nothing
        assert_eq!(shared_sign(&passed_on, &carol.private_key_hex, &coins).unwrap(), passed_on);
        let after_bob = shared_sign(&passed_on, &bob.private_key_hex, &coins).unwrap();
        assert!(shared_summary(&after_bob, &coins).unwrap().ready);

        let sent = shared_finish(&after_bob, &coins).unwrap();
        let vault_script = ScriptPublicKey::pay_to_address(&Address::decode(&vault.address).unwrap());
        for index in 0..sent.transaction.inputs.len() {
            assert!(sent.transaction.verify_input(Network::Testnet, index, &vault_script).is_ok());
        }
        // The automatic fee covers the size including the signatures
        let size = sent.transaction.to_bytes().len() as u64;
        assert!(sent.fee_atoms >= DEFAULT_FEE_PER_BYTE * size, "fee {} for {} bytes", sent.fee_atoms, size);
        assert_eq!(sent.fee_atoms, summary.fee_atoms);
    }

    #[test]
    fn shared_wallet_refuses_proposals_that_do_not_check_out() {
        let alice = keys_from_private_key(&"a1".repeat(32), true).unwrap();
        let bob = keys_from_private_key(&"b2".repeat(32), true).unwrap();
        let shop = keys_from_private_key(&"11".repeat(32), true).unwrap();
        let vault = shared_wallet_create(2, &[&alice.public_key_hex, &bob.public_key_hex], true).unwrap();
        let coins = utxos(&[400_000]);
        let proposal = shared_propose(&vault.script_hex, true, &shop.address, 100_000, 0, &coins, None, None).unwrap();
        let signed_by_alice = shared_sign(&proposal, &alice.private_key_hex, &coins).unwrap();

        // The coin it spends is not in the co-signer's own node's list (already spent, say)
        assert!(shared_sign(&signed_by_alice, &bob.private_key_hex, "[]").unwrap_err().contains("does not list"));

        // Someone changes where the money goes after Alice signed: her signature no longer counts
        let mut redirected = signed_by_alice.clone();
        redirected.transaction.outputs[0].value_atoms += 1;
        redirected.transaction.outputs[1].value_atoms -= 1;
        assert!(shared_summary(&redirected, &coins).unwrap().signed_by.is_empty());
        let both = shared_sign(&redirected, &bob.private_key_hex, &coins).unwrap();
        assert!(shared_finish(&both, &coins).is_err());

        // A proposal claiming to pay out more than the coins hold
        let mut inflated = proposal.clone();
        inflated.transaction.outputs[0].value_atoms = 500_000;
        assert!(shared_summary(&inflated, &coins).unwrap_err().contains("more than it spends"));
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
