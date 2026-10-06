mod genesis;
mod rpc;
mod state;

use clap::Parser;
use genesis::create_testnet_genesis;
use imoney_core::constants::{CURRENCY_NAME, TARGET_TIME_PER_BLOCK_MS, TICKER};
use imoney_core::{BlockHeader, Hash};
use imoney_pow::{DEVNET_DATASET_ITEMS, MoneyPrinterContext, MoneyPrinterPow};
use rpc::create_router;
use state::{DagLedger, SharedLedger};
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Parser, Debug)]
#[command(author, version, about = "Internet Money (IMN) - Full Node Daemon", long_about = None)]
struct Args {
    /// RPC server bind address (e.g., 127.0.0.1:18556)
    #[arg(long, default_value = "127.0.0.1:18556")]
    rpc_bind: String,

    /// Enable automatic background miner (for local devnet/testnet testing)
    #[arg(long, default_value_t = false)]
    auto_mine: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("============================================================");
    println!("  {} ({}) - Full Node Daemon [TESTNET-1]", CURRENCY_NAME, TICKER);
    println!("  Consensus: GHOSTDAG @ 5s Block Time (0.2 BPS)");
    println!("  PoW: Money Printer (ASIC-Resistant Memory-Hard)");
    println!("  RPC / Web Dashboard: http://{}", args.rpc_bind);
    println!("============================================================");

    println!("[*] Initializing Money Printer PoW Context (Devnet mode)...");
    let genesis = create_testnet_genesis();
    let genesis_seed = genesis.pre_pow_hash().unwrap();
    let ctx = Arc::new(MoneyPrinterContext::new(&genesis_seed, DEVNET_DATASET_ITEMS));
    let pow = Arc::new(MoneyPrinterPow::new(ctx));
    println!("[+] PoW Context initialized successfully.");

    let ledger: SharedLedger = Arc::new(RwLock::new(DagLedger::new()));
    println!("[+] Testnet Genesis Block loaded. DAG ledger active.");

    // Optional background auto-miner
    if args.auto_mine {
        let ledger_clone = ledger.clone();
        let pow_clone = pow.clone();
        tokio::spawn(async move {
            println!("[*] Local background auto-miner started (targets ~5s block interval)...");
            loop {
                tokio::time::sleep(Duration::from_millis(TARGET_TIME_PER_BLOCK_MS)).await;

                let template = ledger_clone.read().await.get_mining_template();
                let pre_pow_hash = template.pre_pow_hash;
                let bits = template.bits;
                let stop = Arc::new(AtomicBool::new(false));

                // Mine with single thread in background
                if let Some((nonce, _hash)) = pow_clone.mine(&pre_pow_hash, bits, 0, 10_000_000, stop) {
                    let candidate = BlockHeader {
                        version: template.version,
                        parents: template.parents,
                        hash_merkle_root: Hash::ZERO,
                        accepted_id_merkle_root: Hash::ZERO,
                        utxo_commitment: Hash::ZERO,
                        timestamp_ms: template.timestamp_ms,
                        bits,
                        nonce,
                        daa_score: template.daa_score,
                        blue_score: template.blue_score,
                        blue_work: template.blue_score as u128 * 1000,
                    };

                    let mut writer = ledger_clone.write().await;
                    match writer.add_block(candidate, &pow_clone) {
                        Ok(new_hash) => {
                            println!(
                                "[+] Mined Block #{} | Hash: {} | Blue Score: {}",
                                writer.virtual_daa_score,
                                new_hash,
                                writer.virtual_blue_score
                            );
                        }
                        Err(e) => {
                            eprintln!("[-] Auto-mine error adding block: {}", e);
                        }
                    }
                }
            }
        });
    }

    let app = create_router(ledger, pow);
    let addr: SocketAddr = args.rpc_bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("[+] HTTP RPC & Web Dashboard listening on http://{}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}
