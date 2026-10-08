//! The running side of a pool: its key, its dealings with the node (reading its coins,
//! sending payouts), its saved state and its small status website. The arithmetic is in
//! `pool.rs`.

use crate::pool::{Accounting, Coin, PoolState, PAYOUT_CONFIRMATIONS};
use ed25519_dalek::SigningKey;
use imoney_core::constants::MIN_RELAY_FEE_PER_BYTE;
use imoney_core::serialize::tagged_hash;
use imoney_core::{Address, AddressType, Encode, Hash, Network, Outpoint, ScriptPublicKey, Transaction, TxInput, TxOutput};
use rand::RngCore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

const POOL_PAGE: &str = include_str!("../../../apps/imoney-pool/index.html");
/// Payouts pay twice the minimum relay fee, as wallets do.
const FEE_PER_BYTE: u64 = 2 * MIN_RELAY_FEE_PER_BYTE;
/// A payout the node has no trace of this many blocks after it was sent is taken as lost.
const PAYOUT_LOST_AFTER_BLOCKS: u64 = 3;

/// What the pool is doing right now, for the status page.
#[derive(Default)]
pub struct Live {
    pub miners: usize,
    pub hashrate: f64,
    pub hashrate_by_address: HashMap<String, f64>,
}

pub struct PoolRuntime {
    node: String,
    key: SigningKey,
    network: Network,
    pub address: String,
    state_path: PathBuf,
    fee_basis_points: u64,
    min_payout_atoms: u64,
    accounting: Mutex<Accounting>,
    pub live: Mutex<Live>,
    started: Instant,
}

/// The coin the ledger creates for a blue block's reward (the same rule the node applies).
pub fn reward_tx_id(block_hash: &Hash) -> Hash {
    tagged_hash("IMN 2026 block reward", &[&block_hash.0])
}

fn load_or_create_key(path: &PathBuf) -> Result<SigningKey, String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let bytes: [u8; 32] = hex::decode(text.trim())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| format!("{} does not hold a 64-character hex key", path.display()))?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    std::fs::write(path, hex::encode(bytes)).map_err(|e| format!("could not write {}: {}", path.display(), e))?;
    Ok(SigningKey::from_bytes(&bytes))
}

impl PoolRuntime {
    pub fn open(
        node: &str,
        key_path: PathBuf,
        state_path: PathBuf,
        fee_percent: f64,
        min_payout_imn: f64,
        mainnet: bool,
    ) -> Result<Self, String> {
        let key = load_or_create_key(&key_path)?;
        let network = if mainnet { Network::Mainnet } else { Network::Testnet };
        let address = Address::from_public_key(network, AddressType::PubKeyHash, key.verifying_key().as_bytes()).to_string();
        let state: PoolState = match std::fs::read_to_string(&state_path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is damaged: {}", state_path.display(), e))?,
            Err(_) => PoolState::default(),
        };
        let fee_basis_points = (fee_percent * 100.0).round().clamp(0.0, 10_000.0) as u64;
        Ok(Self {
            node: node.to_string(),
            key,
            network,
            address,
            state_path,
            fee_basis_points,
            min_payout_atoms: (min_payout_imn * imoney_core::constants::ATOMS_PER_IMN as f64) as u64,
            accounting: Mutex::new(Accounting::new(state, fee_basis_points)),
            live: Mutex::new(Live::default()),
            started: Instant::now(),
        })
    }

    pub fn fee_percent(&self) -> f64 {
        self.fee_basis_points as f64 / 100.0
    }

    /// Written under another name first, so a crash never leaves half a file.
    fn save(&self, accounting: &Accounting) {
        let partial = self.state_path.with_extension("partial");
        let written = serde_json::to_string_pretty(&accounting.state)
            .map_err(|e| e.to_string())
            .and_then(|text| std::fs::write(&partial, text).map_err(|e| e.to_string()))
            .and_then(|_| std::fs::rename(&partial, &self.state_path).map_err(|e| e.to_string()));
        if let Err(e) = written {
            eprintln!("[-] Could not save the pool's state: {}", e);
        }
    }

    pub fn record_share(&self, address: &str, work: f64, block_work: f64) {
        self.accounting.lock().unwrap().record_share(address, work, block_work);
    }

    pub fn block_found(&self, block_hash: &Hash, height: u64, block_work: f64) {
        let mut accounting = self.accounting.lock().unwrap();
        accounting.block_found(block_hash.to_hex(), reward_tx_id(block_hash).to_hex(), height, block_work);
        self.save(&accounting);
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        ureq::get(&format!("{}{}", self.node, path))
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())
    }

    /// Credits matured rewards, follows up on the last payout, and sends the next one.
    /// Called every few seconds.
    pub fn tick(&self) {
        let Ok(info) = self.get("/api/v1/info") else { return };
        let Some(height) = info["virtual_blue_score"].as_u64() else { return };
        let Ok(coins) = self.get(&format!("/api/v1/address/{}/utxos", self.address)) else { return };
        let Ok(coins) = serde_json::from_value::<Vec<Coin>>(coins) else { return };

        let mut accounting = self.accounting.lock().unwrap();
        let before = accounting.state.clone();

        let credited = accounting.settle(&coins, height);
        if credited > 0 {
            println!("[+] {} block reward(s) matured and were credited to miners", credited);
        }

        // One payout at a time: the next waits until the last is in the ledger
        if let Some(payout) = accounting.state.in_flight.clone() {
            if let Ok(tx) = self.get(&format!("/api/v1/tx/{}", payout.tx_id)) {
                let deep = tx["confirmations"].as_u64().unwrap_or(0) >= PAYOUT_CONFIRMATIONS;
                match tx["status"].as_str() {
                    Some("confirmed") if deep => {
                        println!("[+] Payout {} confirmed ({} miners)", payout.tx_id, payout.payees.len());
                        accounting.payout_confirmed();
                    }
                    // Not waiting to be mined and not in the ledger: recent blocks were
                    // reordered around it, or the node never took it
                    Some("not_found") if height > payout.sent_at_height + PAYOUT_LOST_AFTER_BLOCKS => {
                        eprintln!("[-] Payout {} is not in the ledger; the miners are owed again and will be paid afresh", payout.tx_id);
                        accounting.payout_lost();
                    }
                    _ => {}
                }
            }
        } else {
            let mut due = accounting.due(self.min_payout_atoms);
            // When the settled coins cannot cover everyone yet, pay those they can cover:
            // leave out the largest amounts first, as they are what does not fit
            let mut built = self.build_payout(&coins, &due, &accounting.state.respend);
            while built.is_err() && due.len() > 1 {
                due.remove(0);
                built = self.build_payout(&coins, &due, &accounting.state.respend);
            }
            if !due.is_empty() {
                match built {
                    Ok(tx) => {
                        let tx_id = tx.id().to_hex();
                        let inputs = tx
                            .inputs
                            .iter()
                            .map(|input| (input.previous_outpoint.transaction_id.to_hex(), input.previous_outpoint.index))
                            .collect();
                        // Recorded before it is sent: if the reply is lost the pool still knows
                        // it may have paid, and checks the ledger rather than paying again
                        accounting.payout_sent(tx_id.clone(), due.clone(), inputs, height);
                        self.save(&accounting);
                        match ureq::post(&format!("{}/api/v1/tx/broadcast", self.node)).send_json(json!({ "transaction": tx })) {
                            Ok(_) => println!("[+] Payout {} sent to {} miners", tx_id, due.len()),
                            Err(e) => eprintln!("[-] Payout not accepted by the node ({}); it will be retried", e),
                        }
                    }
                    // Usually: the pool's coins are not deep enough yet. It tries again shortly.
                    Err(e) => {
                        if self.started.elapsed().as_secs() % 60 < 6 {
                            println!("[*] Payout waiting: {}", e);
                        }
                    }
                }
            }
        }

        if accounting.state != before {
            self.save(&accounting);
        }
    }

    /// One transaction paying every miner in `payees` from the pool's spendable coins, with
    /// change back to the pool. The pool pays the network fee.
    ///
    /// Only settled coins are spent. Coins in `respend` (those a lost payout tried to spend)
    /// go in first, so that payout can never also be accepted.
    fn build_payout(&self, coins: &[Coin], payees: &[(String, u64)], respend: &[(String, u32)]) -> Result<Transaction, String> {
        let pool = Address::decode(&self.address).map_err(|e| e.to_string())?;
        let pool_script = ScriptPublicKey::pay_to_address(&pool);
        let mut outputs = Vec::new();
        for (address, atoms) in payees {
            let payee = Address::decode(address).map_err(|e| format!("{}: {}", address, e))?;
            outputs.push(TxOutput { value_atoms: *atoms, script_public_key: ScriptPublicKey::pay_to_address(&payee) });
        }
        let total: u64 = payees.iter().map(|(_, atoms)| atoms).sum();
        let mut spendable: Vec<&Coin> = coins.iter().filter(|coin| coin.settled()).collect();
        let lost = |coin: &Coin| respend.iter().any(|(tx_id, index)| *tx_id == coin.transaction_id && *index == coin.index);
        spendable.sort_by_key(|coin| (!lost(coin), std::cmp::Reverse(coin.value_atoms)));
        // At least one coin is always spent, so a lost payout's coin goes in even when the
        // amount could be covered without it

        // The fee depends on the size, which depends on how many coins the fee pulls in
        let mut fee = FEE_PER_BYTE * 400;
        for _ in 0..6 {
            let mut gathered = 0u64;
            let mut inputs = Vec::new();
            for coin in &spendable {
                if gathered >= total + fee {
                    break;
                }
                let transaction_id = Hash::from_hex(&coin.transaction_id).map_err(|e| e.to_string())?;
                inputs.push(TxInput {
                    previous_outpoint: Outpoint { transaction_id, index: coin.index },
                    signature_script: Vec::new(),
                });
                gathered += coin.value_atoms;
            }
            if gathered < total + fee {
                return Err(format!("the pool holds {} settled atoms and owes {} plus the fee", gathered, total));
            }
            let mut tx_outputs = outputs.clone();
            if gathered > total + fee {
                tx_outputs.push(TxOutput { value_atoms: gathered - total - fee, script_public_key: pool_script.clone() });
            }
            let mut tx = Transaction { version: 1, inputs, outputs: tx_outputs, payload: b"pool payout".to_vec(), service: None };
            for index in 0..tx.inputs.len() {
                tx.sign_input(self.network, index, &self.key).map_err(|e| e.to_string())?;
            }
            let needed = FEE_PER_BYTE * tx.to_bytes().len() as u64;
            if fee >= needed {
                return Ok(tx);
            }
            fee = needed;
        }
        Err("could not settle on a fee".to_string())
    }

    fn stats(&self) -> Value {
        let accounting = self.accounting.lock().unwrap();
        let live = self.live.lock().unwrap();
        let state = &accounting.state;
        json!({
            "pool_address": self.address,
            "fee_percent": self.fee_percent(),
            "min_payout_atoms": self.min_payout_atoms,
            "miners": live.miners,
            "hashrate_hps": live.hashrate,
            "blocks_found": state.blocks_found,
            "blocks_rewarded": state.blocks_rewarded,
            "blocks_unpaid": state.blocks_unpaid,
            "blocks_maturing": state.pending.len(),
            "owed_atoms": accounting.owed(),
            "paid_atoms": state.paid.values().sum::<u64>(),
            "uptime_seconds": self.started.elapsed().as_secs(),
        })
    }

    fn miner(&self, address: &str) -> Value {
        let accounting = self.accounting.lock().unwrap();
        let live = self.live.lock().unwrap();
        let state = &accounting.state;
        let in_flight = state
            .in_flight
            .iter()
            .flat_map(|payout| payout.payees.iter())
            .filter(|(payee, _)| payee == address)
            .map(|(_, atoms)| *atoms)
            .sum::<u64>();
        json!({
            "address": address,
            "hashrate_hps": live.hashrate_by_address.get(address).copied().unwrap_or(0.0),
            "balance_atoms": state.balances.get(address).copied().unwrap_or(0),
            "being_paid_atoms": in_flight,
            "paid_atoms": state.paid.get(address).copied().unwrap_or(0),
        })
    }

    /// Serves the status page and its two JSON endpoints. Read-only.
    pub fn serve_status(&self, listener: TcpListener) {
        for stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let Ok(mut writer) = stream.try_clone() else { continue };
            let mut request_line = String::new();
            if BufReader::new(stream).read_line(&mut request_line).is_err() {
                continue;
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or("/");
            let (status, kind, body) = match path.split('?').next().unwrap_or("/") {
                "/" => ("200 OK", "text/html; charset=utf-8", POOL_PAGE.to_string()),
                "/api/stats" => ("200 OK", "application/json", self.stats().to_string()),
                other => match other.strip_prefix("/api/miner/") {
                    Some(address) if Address::decode(address).is_ok() => {
                        ("200 OK", "application/json", self.miner(address).to_string())
                    }
                    _ => ("404 Not Found", "text/plain", "Not found".to_string()),
                },
            };
            let _ = write!(
                writer,
                "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
                status,
                kind,
                body.len(),
                body
            );
        }
    }
}
