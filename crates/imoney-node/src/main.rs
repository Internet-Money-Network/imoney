mod genesis;
mod p2p;
mod rpc;
mod state;
mod storage;

use clap::Parser;
use genesis::create_testnet_genesis;
use imoney_core::constants::{CURRENCY_NAME, TARGET_TIME_PER_BLOCK_MS, TICKER};
use imoney_core::{Address, AddressType, BlockHeader, Hash, Network};
use imoney_pow::{DEVNET_DATASET_ITEMS, MoneyPrinterContext, MoneyPrinterPow};
use p2p::PeerManager;
use rpc::create_router;
use state::{DagLedger, SharedLedger};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
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

    /// P2P gossip server bind address (e.g., 0.0.0.0:18555)
    #[arg(long, default_value = "0.0.0.0:18555")]
    p2p_bind: String,

    /// Outbound peer addresses to connect to (comma-separated or multiple flags)
    #[arg(long, value_delimiter = ',')]
    peers: Vec<String>,

    /// Path to persistent database directory
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,

    /// Mining payout address (Bech32, e.g., imntest:q...)
    #[arg(long)]
    mining_address: Option<String>,

    /// Enable automatic background miner (targets ~5s block intervals)
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
    println!("  Storage: Persistent ACID Embedded DB (redb)");
    println!("  Data Directory: {:?}", args.data_dir);
    println!("  RPC / Web Dashboard: http://{}", args.rpc_bind);
    println!("============================================================");

    // Ensure data directory exists
    fs::create_dir_all(&args.data_dir)?;
    let db_path = args.data_dir.join("imoney.redb");

    // Parse or generate mining payout address
    let mining_address = match args.mining_address {
        Some(s) => Some(Address::decode(&s)?),
        None => {
            // Generate a deterministic default testnet miner key for easy local testing
            let dummy_key = [0x77u8; 32];
            let addr = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &dummy_key);
            println!("[*] Default Testnet Mining Address: {}", addr);
            Some(addr)
        }
    };

    println!("[*] Initializing Money Printer PoW Context (Devnet mode)...");
    let genesis = create_testnet_genesis();
    let genesis_seed = genesis.pre_pow_hash().unwrap();
    let ctx = Arc::new(MoneyPrinterContext::new(&genesis_seed, DEVNET_DATASET_ITEMS));
    let pow = Arc::new(MoneyPrinterPow::new(ctx));
    println!("[+] PoW Context initialized successfully.");

    println!("[*] Opening persistent database at {:?}...", db_path);
    let ledger_instance = DagLedger::open(&db_path, mining_address)?;
    let ledger: SharedLedger = Arc::new(RwLock::new(ledger_instance));
    {
        let r = ledger.read().await;
        println!(
            "[+] Persistent ledger active: {} blocks loaded | Blue Score: {}",
            r.blocks.len(),
            r.virtual_blue_score
        );
    }

    // Initialize P2P PeerManager
    let p2p_bind_addr: SocketAddr = args.p2p_bind.parse()?;
    let p2p_manager = Arc::new(PeerManager::new(ledger.clone(), pow.clone(), p2p_bind_addr.port()));
    
    // Start incoming P2P TCP gossip server
    p2p_manager.clone().start_server(p2p_bind_addr).await?;

    // Connect outbound to configured peer nodes
    for peer_str in args.peers {
        let trimmed = peer_str.trim();
        if !trimmed.is_empty() {
            if let Ok(peer_addr) = trimmed.parse::<SocketAddr>() {
                p2p_manager.clone().connect_to_peer(peer_addr);
            } else {
                eprintln!("[-] Warning: Could not parse peer address '{}'", trimmed);
            }
        }
    }

    // Background auto-miner task
    if args.auto_mine {
        let ledger_clone = ledger.clone();
        let pow_clone = pow.clone();
        let p2p_clone = p2p_manager.clone();
        tokio::spawn(async move {
            println!("[*] Local background auto-miner started (targets ~5s block interval)...");
            loop {
                tokio::time::sleep(Duration::from_millis(TARGET_TIME_PER_BLOCK_MS)).await;

                let template = ledger_clone.read().await.get_mining_template();
                let pre_pow_hash = template.pre_pow_hash;
                let bits = template.bits;
                let stop = Arc::new(AtomicBool::new(false));

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
                    match writer.add_block(candidate.clone(), &pow_clone) {
                        Ok(new_hash) => {
                            // Gossip block across P2P network
                            p2p_clone.broadcast_block(candidate);

                            let balance_info = writer.mining_address.as_ref().and_then(|a| writer.get_balance(a).ok()).map(|(_, coins)| coins).unwrap_or(0.0);
                            println!(
                                "[+] Mined Block #{} | Hash: {} | Blue Score: {} | Miner Balance: {:.2} IM",
                                writer.virtual_daa_score,
                                new_hash,
                                writer.virtual_blue_score,
                                balance_info
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

    let app = create_router(ledger, pow, p2p_manager);
    let addr: SocketAddr = args.rpc_bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("[+] HTTP RPC & Web Dashboard listening on http://{}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

