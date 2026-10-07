use clap::Parser;
use imoney_core::constants::*;
use imoney_core::{Block, BlockHeader, Hash};
use imoney_emission::block_subsidy_imn;
use imoney_pow::{MoneyPrinterPow, PowMode, PowParams};
use serde::Deserialize;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Hashes tried on one block template before a fresh one is fetched from the node.
const MINE_BATCH: u64 = 200_000;

#[derive(Parser, Debug)]
#[command(author, version, about = "Internet Money (IMN) - Money Printer PoW Miner & Benchmark", long_about = None)]
struct Args {
    /// Number of worker threads (default: system CPU count)
    #[arg(short, long)]
    threads: Option<usize>,

    /// Mine for a node: its RPC address, e.g. http://127.0.0.1:18556
    #[arg(short, long)]
    node: Option<String>,

    /// Payout address for mined blocks (default: the node's own mining address)
    #[arg(short, long)]
    address: Option<String>,

    /// Bearer token, if the node was started with --rpc-token
    #[arg(long)]
    rpc_token: Option<String>,

    /// Run hashrate benchmark
    #[arg(short, long, default_value_t = false)]
    benchmark: bool,

    /// Number of hashes to calculate during benchmark
    #[arg(long, default_value_t = 100_000)]
    bench_iters: u64,

    /// Mine a test block header
    #[arg(short, long, default_value_t = false)]
    mine_test: bool,
}

/// The part of the node's mining template the miner needs.
#[derive(Deserialize)]
struct Template {
    block: Block,
}

#[derive(Deserialize)]
struct SubmitResponse {
    success: bool,
    block_hash: Option<String>,
    error: Option<String>,
}

fn main() {
    let args = Args::parse();

    if let Some(t) = args.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(t)
            .build_global()
            .unwrap();
    }

    println!("============================================================");
    println!("  {} ({}) - Proof-of-Work Node & Miner", CURRENCY_NAME, TICKER);
    println!("  Algorithm: Money Printer (FishHash, memory-hard)");
    println!("  Block Time: {}s | Launch Subsidy: {} IMN", TARGET_TIME_PER_BLOCK_MS / 1000, block_subsidy_imn(0));
    println!("  Threads: {}", rayon::current_num_threads());
    println!("============================================================");

    if let Some(node) = &args.node {
        if let Err(e) = mine_for_node(node.trim_end_matches('/'), &args) {
            eprintln!("[-] Miner stopped: {}", e);
            std::process::exit(1);
        }
        return;
    }

    println!("[*] Initializing Money Printer memory dataset (testnet size)...");
    let init_start = Instant::now();
    let pow = MoneyPrinterPow::new(PowParams::testnet(), Hash::from_bytes([0x42; 32]), PowMode::Full);
    println!(
        "[+] Dataset ready: {} items (took {:.2?})",
        pow.context().dataset_items(),
        init_start.elapsed()
    );

    if args.benchmark || !args.mine_test {
        println!("\n[*] Running Hashrate Benchmark ({} iterations)...", args.bench_iters);
        let bench_start = Instant::now();
        let dummy_header = Hash::from_bytes([0x01; 32]);
        let stop_signal = Arc::new(AtomicBool::new(false));

        // Bits that won't trigger early exit (hard target)
        let hard_bits = 0x1d00ffff;
        pow.mine(&dummy_header, hard_bits, 0, args.bench_iters, stop_signal);

        let elapsed = bench_start.elapsed().as_secs_f64();
        let h_s = (args.bench_iters as f64) / elapsed;
        println!(
            "[+] Benchmark complete: {:.2} kH/s ({} hashes in {:.2}s)",
            h_s / 1000.0,
            args.bench_iters,
            elapsed
        );
    }

    if args.mine_test {
        println!("\n[*] Assembling sample blockDAG header...");
        let header = BlockHeader {
            version: 1,
            parents: vec![Hash::from_bytes([0x0a; 32])],
            hash_merkle_root: Hash::ZERO,
            accepted_id_merkle_root: Hash::ZERO,
            utxo_commitment: Hash::ZERO,
            timestamp_ms: chrono::Utc::now().timestamp_millis() as u64,
            bits: 0x207fffff, // Easy devnet difficulty
            nonce: 0,
            daa_score: 1,
            blue_score: 1,
            blue_work: 1000,
        };

        let pre_hash = header.pre_pow_hash().expect("valid header");
        println!("[*] Pre-PoW Hash: {}", pre_hash);
        println!("[*] Difficulty Target Bits: 0x{:08x}", header.bits);
        println!("[*] Mining block...");

        let mine_start = Instant::now();
        let stop_signal = Arc::new(AtomicBool::new(false));
        if let Some((nonce, hash)) = pow.mine(&pre_hash, header.bits, 0, 50_000_000, stop_signal) {
            println!("\n============================================================");
            println!("  >>> BLOCK MINED SUCCESSFULLY! <<<");
            println!("  Nonce:     {}", nonce);
            println!("  PoW Hash:  {}", hash);
            println!("  Duration:  {:.2?}", mine_start.elapsed());
            println!("  Verified:  {}", pow.verify(&pre_hash, nonce, header.bits));
            println!("============================================================");
        } else {
            println!("[-] No valid nonce found within iteration budget.");
        }
    }
}

/// Mines for a node: fetch a block template, search for a nonce, submit, repeat.
fn mine_for_node(node: &str, args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    // The node's genesis hash seeds the proof of work for its network
    let info: serde_json::Value = ureq::get(&format!("{}/api/v1/info", node)).call()?.into_json()?;
    let genesis = Hash::from_hex(info["genesis_hash"].as_str().ok_or("node did not report a genesis hash")?)?;
    let pow = MoneyPrinterPow::new(PowParams::testnet(), genesis, PowMode::Full);
    println!("[*] Building the mining dataset...");
    pow.context();
    println!("[+] Connected to {} ({})", node, info["network"].as_str().unwrap_or("unknown network"));

    let template_url = match &args.address {
        Some(address) => format!("{}/api/v1/mining/template?address={}", node, address),
        None => format!("{}/api/v1/mining/template", node),
    };
    let submit_url = format!("{}/api/v1/mining/submit", node);

    let mut hashes: u64 = 0;
    let mut blocks_found: u64 = 0;
    let started = Instant::now();
    let mut last_report = Instant::now();
    loop {
        let template: Template = match ureq::get(&template_url).call() {
            Ok(response) => response.into_json()?,
            Err(e) => {
                eprintln!("[-] Could not fetch work: {}. Retrying...", e);
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let mut block = template.block;
        let pre_pow_hash = block.header.pre_pow_hash()?;
        let start_nonce = rand::random::<u64>();

        let found = pow.mine(&pre_pow_hash, block.header.bits, start_nonce, MINE_BATCH, Arc::new(AtomicBool::new(false)));
        hashes += MINE_BATCH;

        if let Some((nonce, _)) = found {
            block.header.nonce = nonce;
            let mut request = ureq::post(&submit_url);
            if let Some(token) = &args.rpc_token {
                request = request.set("Authorization", &format!("Bearer {}", token));
            }
            match request.send_json(serde_json::json!({ "block": block })) {
                Ok(response) => {
                    let result: SubmitResponse = response.into_json()?;
                    if result.success {
                        blocks_found += 1;
                        println!(
                            "[+] Block accepted: {} | DAA score {} | {} found so far",
                            result.block_hash.unwrap_or_default(),
                            block.header.daa_score,
                            blocks_found
                        );
                    }
                }
                // A stale template (another block arrived first) is rejected; that is normal
                Err(ureq::Error::Status(400, response)) => {
                    let result: SubmitResponse = response.into_json()?;
                    println!("[*] Block not accepted: {}", result.error.unwrap_or_default());
                }
                Err(ureq::Error::Status(401, _)) => return Err("the node requires --rpc-token".into()),
                Err(e) => eprintln!("[-] Could not submit block: {}", e),
            }
        }

        if last_report.elapsed() > Duration::from_secs(30) {
            // An upper bound: a batch that finds a block stops early
            println!("[*] Up to {:.0} kH/s", hashes as f64 / started.elapsed().as_secs_f64() / 1000.0);
            last_report = Instant::now();
        }
    }
}
