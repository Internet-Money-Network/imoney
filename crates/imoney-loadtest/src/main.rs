//! Load test for an Internet Money test node.
//!
//! Floods one node with real signed payments at a chosen rate while many clients watch
//! addresses over WebSocket, then reports how the node coped: submit times, API response
//! times, time to confirmation, block intervals and mempool size.
//!
//! The payments are funded by mining, so the node must be mining to this tool's funding
//! address. Print it with `--print-address`, then start a throwaway node with
//! `imoney-node --data-dir <empty dir> --auto-mine --mining-address <that address>`.

use clap::Parser;
use ed25519_dalek::SigningKey;
use imoney_core::{Address, AddressType, Hash, Network, Outpoint, ScriptPublicKey, Transaction, TxInput, TxOutput};
use serde::Deserialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NETWORK: Network = Network::Testnet;
/// Value given to each test wallet, in atoms.
const WALLET_FUNDS: u64 = 3_000_000;
/// Wallets funded by one fan-out transaction.
const FAN_OUT: usize = 150;
/// Fee of a one-input, one-output payment (240 bytes at a little over the 10 atoms/byte minimum).
const PAYMENT_FEE: u64 = 3_000;
const FAN_OUT_FEE: u64 = 100_000;

#[derive(Parser, Debug)]
#[command(about = "Floods an Internet Money test node with payments and watchers and reports how it copes")]
struct Args {
    /// RPC address of the node under test
    #[arg(long, default_value = "http://127.0.0.1:18556")]
    node: String,

    /// Payments to submit per second
    #[arg(long, default_value_t = 20.0)]
    tps: f64,

    /// How long to keep submitting, in seconds
    #[arg(long, default_value_t = 60)]
    duration: u64,

    /// Number of test wallets. Each can have one unconfirmed payment at a time, so this should
    /// be at least 20 times the payment rate.
    #[arg(long, default_value_t = 1500)]
    wallets: usize,

    /// Clients that hold a WebSocket open on a wallet's address, as checkout pages do
    #[arg(long, default_value_t = 200)]
    watchers: usize,

    /// Threads submitting payments
    #[arg(long, default_value_t = 16)]
    workers: usize,

    /// Print the address the node must mine to, and exit
    #[arg(long, default_value_t = false)]
    print_address: bool,
}

fn key_for(label: &str) -> SigningKey {
    SigningKey::from_bytes(blake3::hash(format!("imoney load test: {}", label).as_bytes()).as_bytes())
}

fn address_of(key: &SigningKey) -> Address {
    Address::from_public_key(NETWORK, AddressType::PubKeyHash, key.verifying_key().as_bytes())
}

/// A test wallet: one key holding one coin.
struct Wallet {
    key: SigningKey,
    script: ScriptPublicKey,
    coin: Outpoint,
    value: u64,
}

#[derive(Deserialize)]
struct UtxoItem {
    transaction_id: String,
    index: u32,
    value_atoms: u64,
    spendable: bool,
}

#[derive(Deserialize)]
struct BroadcastResponse {
    success: bool,
    error: Option<String>,
}

fn get(agent: &ureq::Agent, url: &str) -> Result<serde_json::Value, String> {
    agent.get(url).call().map_err(|e| e.to_string())?.into_json().map_err(|e| e.to_string())
}

/// Submits a transaction. `Ok(())` when the node admitted it, otherwise the node's reason.
fn broadcast(agent: &ureq::Agent, node: &str, tx: &Transaction) -> Result<(), String> {
    let body = serde_json::json!({ "transaction": tx });
    let response = match agent.post(&format!("{}/api/v1/tx/broadcast", node)).send_json(body) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(e) => return Err(format!("network: {}", e)),
    };
    let parsed: BroadcastResponse = response.into_json().map_err(|e| e.to_string())?;
    if parsed.success {
        Ok(())
    } else {
        Err(parsed.error.unwrap_or_else(|| "rejected".to_string()))
    }
}

fn spend(inputs: Vec<Outpoint>, outputs: Vec<TxOutput>, key: &SigningKey) -> Transaction {
    let mut tx = Transaction {
        version: 1,
        inputs: inputs
            .into_iter()
            .map(|previous_outpoint| TxInput { previous_outpoint, signature_script: Vec::new() })
            .collect(),
        outputs,
        payload: Vec::new(),
        service: None,
    };
    for i in 0..tx.inputs.len() {
        tx.sign_input(NETWORK, i, key).expect("input exists");
    }
    tx
}

fn confirmations(agent: &ureq::Agent, node: &str, tx_id: &Hash) -> u64 {
    get(agent, &format!("{}/api/v1/tx/{}", node, tx_id.to_hex()))
        .ok()
        .and_then(|status| status["confirmations"].as_u64())
        .unwrap_or(0)
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

fn summary(label: &str, mut values: Vec<f64>, unit: &str) {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if values.is_empty() {
        println!("  {:<28} no samples", label);
    } else {
        println!(
            "  {:<28} median {:>8.1} {unit}   95% under {:>8.1} {unit}   worst {:>8.1} {unit}   ({} samples)",
            label,
            percentile(&values, 0.5),
            percentile(&values, 0.95),
            values[values.len() - 1],
            values.len(),
        );
    }
}

fn main() {
    let args = Args::parse();
    let funding_key = key_for("funding");
    let funding_address = address_of(&funding_key);
    if args.print_address {
        println!("{}", funding_address);
        return;
    }

    let node = args.node.trim_end_matches('/').to_string();
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build();
    let info = get(&agent, &format!("{}/api/v1/info", node)).unwrap_or_else(|e| {
        eprintln!("Cannot reach the node at {}: {}", node, e);
        std::process::exit(1);
    });
    if info["mining_address"].as_str() != Some(funding_address.to_string().as_str()) {
        eprintln!(
            "The node is not mining to this tool's funding address.\nStart a throwaway node with:\n  imoney-node --data-dir <empty dir> --auto-mine --mining-address {}",
            funding_address
        );
        std::process::exit(1);
    }
    println!("Node: {} ({})", node, info["network"].as_str().unwrap_or("?"));
    println!("Plan: {} payments/s for {} s, {} wallets, {} watchers", args.tps, args.duration, args.wallets, args.watchers);

    // ---------------------------------------------------------------- funding
    let fan_outs = args.wallets.div_ceil(FAN_OUT);
    let needed_per_coin = FAN_OUT as u64 * WALLET_FUNDS + FAN_OUT_FEE;
    let utxo_url = format!("{}/api/v1/address/{}/utxos", node, funding_address.to_string().replace(':', "%3A"));
    println!("[1/4] Waiting for {} matured mining rewards...", fan_outs);
    let coins: Vec<UtxoItem> = loop {
        let utxos: Vec<UtxoItem> = serde_json::from_value(get(&agent, &utxo_url).unwrap_or_default()).unwrap_or_default();
        let usable: Vec<UtxoItem> =
            utxos.into_iter().filter(|u| u.spendable && u.value_atoms >= needed_per_coin).take(fan_outs).collect();
        if usable.len() >= fan_outs {
            break usable;
        }
        std::thread::sleep(Duration::from_secs(2));
    };

    println!("[2/4] Funding {} wallets with {} fan-out transactions...", args.wallets, fan_outs);
    let funding_script = ScriptPublicKey::pay_to_address(&funding_address);
    let mut wallets: Vec<Wallet> = Vec::with_capacity(args.wallets);
    let mut fan_out_ids = Vec::new();
    for (batch, coin) in coins.iter().enumerate() {
        let first = batch * FAN_OUT;
        let count = FAN_OUT.min(args.wallets - first);
        let keys: Vec<SigningKey> = (first..first + count).map(|i| key_for(&format!("wallet {}", i))).collect();
        let mut outputs: Vec<TxOutput> = keys
            .iter()
            .map(|key| TxOutput { value_atoms: WALLET_FUNDS, script_public_key: ScriptPublicKey::pay_to_address(&address_of(key)) })
            .collect();
        let change = coin.value_atoms - count as u64 * WALLET_FUNDS - FAN_OUT_FEE;
        outputs.push(TxOutput { value_atoms: change, script_public_key: funding_script.clone() });

        let source = Outpoint { transaction_id: Hash::from_hex(&coin.transaction_id).expect("hex id"), index: coin.index };
        let tx = spend(vec![source], outputs, &funding_key);
        let tx_id = tx.id();
        if let Err(e) = broadcast(&agent, &node, &tx) {
            eprintln!("Funding transaction rejected: {}", e);
            std::process::exit(1);
        }
        fan_out_ids.push(tx_id);
        for (index, key) in keys.into_iter().enumerate() {
            let script = ScriptPublicKey::pay_to_address(&address_of(&key));
            wallets.push(Wallet { key, script, coin: Outpoint { transaction_id: tx_id, index: index as u32 }, value: WALLET_FUNDS });
        }
    }
    while !fan_out_ids.iter().all(|id| confirmations(&agent, &node, id) >= 1) {
        std::thread::sleep(Duration::from_secs(1));
    }

    // ---------------------------------------------------------------- watchers
    println!("[3/4] Opening {} address watchers...", args.watchers);
    let watcher_events = Arc::new(AtomicU64::new(0));
    let watchers_connected = Arc::new(AtomicU64::new(0));
    let ws_base = node.replacen("http", "ws", 1);
    for i in 0..args.watchers {
        let address = address_of(&wallets[i % wallets.len()].key).to_string().replace(':', "%3A");
        let url = format!("{}/api/v1/ws/address/{}", ws_base, address);
        let (events, connected) = (watcher_events.clone(), watchers_connected.clone());
        std::thread::spawn(move || {
            let Ok((mut socket, _)) = tungstenite::connect(url.as_str()) else {
                return;
            };
            connected.fetch_add(1, Ordering::Relaxed);
            while let Ok(message) = socket.read() {
                if message.is_text() && message.to_text().is_ok_and(|text| text.contains("payment_")) {
                    events.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }
    std::thread::sleep(Duration::from_secs(2));
    println!("      {} connected", watchers_connected.load(Ordering::Relaxed));

    // ---------------------------------------------------------------- the run
    println!("[4/4] Submitting payments...");
    let queue = Arc::new(Mutex::new(wallets.into_iter().collect::<VecDeque<Wallet>>()));
    let submit_ms = Arc::new(Mutex::new(Vec::<f64>::new()));
    let tracked = Arc::new(Mutex::new(Vec::<(Hash, Instant)>::new()));
    let (accepted, not_ready, failed) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let first_error = Arc::new(Mutex::new(None::<String>));
    let running = Arc::new(AtomicBool::new(true));

    let (job_sender, job_receiver) = mpsc::channel::<()>();
    let job_receiver = Arc::new(Mutex::new(job_receiver));
    let mut worker_handles = Vec::new();
    for _ in 0..args.workers {
        let (queue, submit_ms, tracked, job_receiver) = (queue.clone(), submit_ms.clone(), tracked.clone(), job_receiver.clone());
        let (accepted, not_ready, failed, first_error) = (accepted.clone(), not_ready.clone(), failed.clone(), first_error.clone());
        let (agent, node) = (agent.clone(), node.clone());
        worker_handles.push(std::thread::spawn(move || loop {
            if job_receiver.lock().unwrap().recv().is_err() {
                return;
            }
            let Some(mut wallet) = queue.lock().unwrap().pop_front() else {
                continue;
            };
            let output = TxOutput { value_atoms: wallet.value - PAYMENT_FEE, script_public_key: wallet.script.clone() };
            let tx = spend(vec![wallet.coin.clone()], vec![output], &wallet.key);
            let started = Instant::now();
            match broadcast(&agent, &node, &tx) {
                Ok(()) => {
                    submit_ms.lock().unwrap().push(started.elapsed().as_secs_f64() * 1000.0);
                    let tx_id = tx.id();
                    // Follow one payment in 25 through to confirmation
                    if accepted.fetch_add(1, Ordering::Relaxed) % 25 == 0 {
                        tracked.lock().unwrap().push((tx_id, started));
                    }
                    wallet.coin = Outpoint { transaction_id: tx_id, index: 0 };
                    wallet.value -= PAYMENT_FEE;
                }
                // The wallet's previous payment has not confirmed yet, so its coin does not exist
                // (or is still claimed in the mempool). The wallet simply waits its next turn.
                Err(e) if e.contains("UTXO not found") || e.contains("already spent") => {
                    not_ready.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    first_error.lock().unwrap().get_or_insert(e);
                }
            }
            queue.lock().unwrap().push_back(wallet);
        }));
    }

    // Samples the node's own view every few seconds, timing the requests as a client would
    let api_ms = Arc::new(Mutex::new(Vec::<f64>::new()));
    let node_samples = Arc::new(Mutex::new(Vec::<(u64, u64)>::new())); // (blue score, mempool)
    let sampler = {
        let (agent, node, running, api_ms, node_samples) = (agent.clone(), node.clone(), running.clone(), api_ms.clone(), node_samples.clone());
        let invoice_url = format!("{}/api/v1/invoice/LOADTEST-1?address={}", node, funding_address.to_string().replace(':', "%3A"));
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let started = Instant::now();
                if let Ok(stats) = get(&agent, &format!("{}/api/v1/stats", node)) {
                    api_ms.lock().unwrap().push(started.elapsed().as_secs_f64() * 1000.0);
                    let (score, mempool) = (stats["blue_score"].as_u64().unwrap_or(0), stats["mempool_size"].as_u64().unwrap_or(0));
                    node_samples.lock().unwrap().push((score, mempool));
                    println!("      blue score {:>6}   mempool {:>6}   block time {:>5.1} s", score, mempool, stats["average_block_time_sec"].as_f64().unwrap_or(0.0));
                }
                let started = Instant::now();
                if get(&agent, &invoice_url).is_ok() {
                    api_ms.lock().unwrap().push(started.elapsed().as_secs_f64() * 1000.0);
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        })
    };

    // Polls the followed payments until each is in a block
    let confirm_s = Arc::new(Mutex::new(Vec::<f64>::new()));
    let tracker = {
        let (agent, node, running, tracked, confirm_s) = (agent.clone(), node.clone(), running.clone(), tracked.clone(), confirm_s.clone());
        std::thread::spawn(move || loop {
            let pending: Vec<(Hash, Instant)> = std::mem::take(&mut *tracked.lock().unwrap());
            let mut still_pending = Vec::new();
            for (tx_id, started) in pending {
                if confirmations(&agent, &node, &tx_id) >= 1 {
                    confirm_s.lock().unwrap().push(started.elapsed().as_secs_f64());
                } else {
                    still_pending.push((tx_id, started));
                }
            }
            let none_left = still_pending.is_empty();
            tracked.lock().unwrap().extend(still_pending);
            if !running.load(Ordering::Relaxed) && none_left {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        })
    };

    let run_started = Instant::now();
    let interval = Duration::from_secs_f64(1.0 / args.tps);
    let total_jobs = (args.tps * args.duration as f64) as u64;
    for job in 0..total_jobs {
        let due = run_started + interval.mul_f64(job as f64);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let _ = job_sender.send(());
    }
    drop(job_sender);
    for handle in worker_handles {
        let _ = handle.join();
    }
    let run_seconds = run_started.elapsed().as_secs_f64();

    // Give the last payments time to confirm, up to a minute
    let drain_started = Instant::now();
    while drain_started.elapsed() < Duration::from_secs(60) {
        let mempool = get(&agent, &format!("{}/api/v1/info", node)).ok().and_then(|i| i["mempool_size"].as_u64()).unwrap_or(0);
        if mempool == 0 && tracked.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let drain_seconds = drain_started.elapsed().as_secs_f64();
    running.store(false, Ordering::Relaxed);
    let _ = sampler.join();
    let unconfirmed = tracked.lock().unwrap().len();
    if unconfirmed == 0 {
        let _ = tracker.join();
    }

    // ---------------------------------------------------------------- report
    let accepted = accepted.load(Ordering::Relaxed);
    let samples = node_samples.lock().unwrap().clone();
    let blocks = samples.last().map_or(0, |last| last.0.saturating_sub(samples[0].0));
    let peak_mempool = samples.iter().map(|s| s.1).max().unwrap_or(0);
    println!("\n==================== RESULT ====================");
    println!("  Asked for                    {:.0} payments/s for {} s", args.tps, args.duration);
    println!("  Accepted by the node         {} ({:.1} per second)", accepted, accepted as f64 / run_seconds);
    println!("  Wallet not ready, skipped    {}", not_ready.load(Ordering::Relaxed));
    println!("  Rejected for other reasons   {}", failed.load(Ordering::Relaxed));
    if let Some(error) = first_error.lock().unwrap().as_ref() {
        println!("    first reason: {}", error);
    }
    summary("Time to submit a payment", submit_ms.lock().unwrap().clone(), "ms");
    summary("API response time", api_ms.lock().unwrap().clone(), "ms");
    summary("Time until in a block", confirm_s.lock().unwrap().clone(), "s ");
    println!("  Followed payments unconfirmed {}", unconfirmed);
    println!("  Blocks during the run        {} (one per {:.1} s)", blocks, if blocks > 0 { (run_seconds + drain_seconds) / blocks as f64 } else { 0.0 });
    println!("  Largest mempool seen         {}", peak_mempool);
    println!("  Mempool emptied after        {:.0} s", drain_seconds);
    println!("  Watchers connected           {} of {}", watchers_connected.load(Ordering::Relaxed), args.watchers);
    println!("  Watcher notifications        {}", watcher_events.load(Ordering::Relaxed));
    std::process::exit(0);
}
