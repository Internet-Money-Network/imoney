use clap::Parser;
use imoney_core::constants::*;
use imoney_core::{BlockHeader, Hash};
use imoney_emission::block_subsidy_imn;
use imoney_pow::{DEVNET_DATASET_ITEMS, MoneyPrinterContext, MoneyPrinterPow};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(author, version, about = "Internet Money (IMN) - Money Printer PoW Miner & Benchmark", long_about = None)]
struct Args {
    /// Number of worker threads (default: system CPU count)
    #[arg(short, long)]
    threads: Option<usize>,

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
    println!("  Algorithm: Money Printer (ASIC-Resistant Memory-Hard PoW)");
    println!("  Block Time: {}s | Launch Subsidy: {} IMN", TARGET_TIME_PER_BLOCK_MS / 1000, block_subsidy_imn(0));
    println!("  Threads: {}", rayon::current_num_threads());
    println!("============================================================");

    println!("[*] Initializing Money Printer memory dataset (Devnet size)...");
    let init_start = Instant::now();
    let seed = Hash::from_bytes([0x42; 32]);
    let ctx = Arc::new(MoneyPrinterContext::new(&seed, DEVNET_DATASET_ITEMS));
    println!(
        "[+] Dataset ready: {} items (took {:.2?})",
        ctx.item_count(),
        init_start.elapsed()
    );

    let pow = MoneyPrinterPow::new(ctx);

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
