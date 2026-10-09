//! The few lines every cookbook example shares: keys, and a node's HTTP API.
//!
//! Everything here goes through the public API of a node (`/api/v1/...`) and the
//! `imoney-core` crate. Nothing is private to this repository, so the same code works in
//! your own project.

use ed25519_dalek::SigningKey;
use imoney_core::{Address, AddressType, Hash, Network, Outpoint, ScriptPublicKey, Transaction, TxOutput};
use serde::Deserialize;
use serde_json::Value;

/// The examples run on test networks. Change this one line for a main network.
pub const NETWORK: Network = Network::Testnet;

/// A fee that comfortably covers an ordinary payment (the minimum is 10 atoms per byte, and a
/// simple payment is about 240 bytes).
pub const FEE_ATOMS: u64 = 10_000;

/// Where the examples look for a node unless `IMN_NODE` says otherwise.
pub fn node_url() -> String {
    std::env::var("IMN_NODE").unwrap_or_else(|_| "http://127.0.0.1:18556".to_string())
}

/// A new random key.
pub fn new_key() -> SigningKey {
    SigningKey::generate(&mut rand::rngs::OsRng)
}

/// Reads a key from 64 hex characters, or from a file holding them (such as the
/// `miner-key.hex` a node writes into its data directory).
pub fn load_key(hex_or_path: &str) -> Result<SigningKey, String> {
    let text = match std::fs::read_to_string(hex_or_path) {
        Ok(contents) => contents,
        Err(_) => hex_or_path.to_string(),
    };
    let bytes: [u8; 32] = hex::decode(text.trim())
        .map_err(|e| format!("not a hex key or a readable file: {}", e))?
        .try_into()
        .map_err(|_| "a key is 32 bytes (64 hex characters)".to_string())?;
    Ok(SigningKey::from_bytes(&bytes))
}

/// The ordinary address of a key.
pub fn address_of(key: &SigningKey) -> Address {
    Address::from_public_key(NETWORK, AddressType::PubKeyHash, key.verifying_key().as_bytes())
}

/// Whole and fractional IMN as text, without floating point.
pub fn imn(atoms: u64) -> String {
    format!("{}.{:08}", atoms / imoney_core::ATOMS_PER_IMN, atoms % imoney_core::ATOMS_PER_IMN)
}

/// One row of `GET /api/v1/address/{address}/utxos`.
#[derive(Deserialize)]
struct Coin {
    transaction_id: String,
    index: u32,
    value_atoms: u64,
    /// False for a mining reward that has not matured yet
    spendable: bool,
}

/// A node's HTTP API.
pub struct Node {
    url: String,
    agent: ureq::Agent,
}

impl Node {
    pub fn new(url: &str) -> Self {
        Self { url: url.trim_end_matches('/').to_string(), agent: ureq::Agent::new() }
    }

    /// `GET` a path under `/api/v1/` and parse the JSON reply.
    pub fn get(&self, path: &str) -> Result<Value, String> {
        // An address contains a colon, which must be escaped in a URL path
        let url = format!("{}/api/v1/{}", self.url, path.replace(':', "%3A"));
        self.agent.get(&url).call().map_err(|e| e.to_string())?.into_json().map_err(|e| e.to_string())
    }

    /// The coins an address can spend right now, in the form the payment builders take.
    pub fn spendable_coins(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput)>, String> {
        let coins: Vec<Coin> =
            serde_json::from_value(self.get(&format!("address/{}/utxos", address))?).map_err(|e| e.to_string())?;
        coins
            .into_iter()
            .filter(|coin| coin.spendable)
            .map(|coin| {
                let transaction_id = Hash::from_hex(&coin.transaction_id).map_err(|e| e.to_string())?;
                let output = TxOutput { value_atoms: coin.value_atoms, script_public_key: ScriptPublicKey::pay_to_address(address) };
                Ok((Outpoint { transaction_id, index: coin.index }, output))
            })
            .collect()
    }

    /// Hands a signed transaction to the node. Returns its ID, or the node's reason for refusing.
    pub fn broadcast(&self, tx: &Transaction) -> Result<String, String> {
        let body = serde_json::json!({ "transaction": tx });
        let response = match self.agent.post(&format!("{}/api/v1/tx/broadcast", self.url)).send_json(body) {
            Ok(response) => response,
            // A refusal comes back as an error status with the reason in the body
            Err(ureq::Error::Status(_, response)) => response,
            Err(e) => return Err(e.to_string()),
        };
        let reply: Value = response.into_json().map_err(|e| e.to_string())?;
        if reply["success"].as_bool() == Some(true) {
            Ok(tx.id().to_hex())
        } else {
            Err(reply["error"].as_str().unwrap_or("refused").to_string())
        }
    }

    /// What has been paid towards an invoice at an address: `(seen, included, final)` atoms.
    /// Each figure includes the ones after it.
    pub fn invoice_paid(&self, invoice_id: &str, address: &Address) -> Result<(u64, u64, u64), String> {
        let status = self.get(&format!("invoice/{}?address={}", invoice_id, address))?;
        let atoms = |field: &str| status[field].as_u64().unwrap_or(0);
        Ok((atoms("seen_atoms"), atoms("included_atoms"), atoms("final_atoms")))
    }
}
