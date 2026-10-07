use clap::Parser;
use imoney_node::genesis::create_testnet_genesis;
use imoney_core::constants::{CURRENCY_NAME, TARGET_TIME_PER_BLOCK_MS, TICKER};
use imoney_core::{Address, AddressType, Network};
use imoney_pow::{DEVNET_DATASET_ITEMS, MoneyPrinterContext, MoneyPrinterPow};
use imoney_node::p2p::PeerManager;
use imoney_node::rpc::create_router;
use imoney_node::state::{DagLedger, SharedLedger};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
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


/// Loads the node's own mining keypair from the data directory, creating it on first run.
fn load_or_create_miner_key(data_dir: &Path) -> Result<(Address, PathBuf), Box<dyn std::error::Error>> {
    let key_path = data_dir.join("miner-key.hex");
    let signing_key = if key_path.exists() {
        let bytes = hex::decode(fs::read_to_string(&key_path)?.trim())?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "miner-key.hex must contain a 32-byte hex private key")?;
        ed25519_dalek::SigningKey::from_bytes(&key_bytes)
    } else {
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        fs::write(&key_path, hex::encode(key.to_bytes()))?;
        key
    };

    let addr = Address::from_public_key(
        Network::Testnet,
        AddressType::PubKeyHash,
        signing_key.verifying_key().as_bytes(),
    );
    Ok((addr, key_path))
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
            let (addr, key_path) = load_or_create_miner_key(&args.data_dir)?;
            println!("[*] Mining Address (local key): {}", addr);
            println!("[*] Its private key is stored in {:?}. Keep that file private.", key_path);
            Some(addr)
        }
    };

    println!("[*] Initializing Money Printer PoW Context (Devnet mode)...");
    let genesis = create_testnet_genesis();
    let genesis_seed = genesis.header.pre_pow_hash().unwrap();
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

                let template = ledger_clone.read().await.get_mining_template(None);
                let pre_pow_hash = template.pre_pow_hash;
                let bits = template.block.header.bits;
                let stop = Arc::new(AtomicBool::new(false));

                if let Some((nonce, _hash)) = pow_clone.mine(&pre_pow_hash, bits, 0, 10_000_000, stop) {
                    let candidate = template.into_block(nonce);

                    let mut writer = ledger_clone.write().await;
                    match writer.add_block(candidate.clone(), &pow_clone) {
                        Ok(new_hash) => {
                            // Gossip block across P2P network
                            p2p_clone.broadcast_block(candidate);

                            let balance_info = writer.mining_address.as_ref().and_then(|a| writer.get_balance(a).ok()).map(|(_, coins)| coins).unwrap_or(0.0);
                            println!(
                                "[+] Mined Block #{} | Hash: {} | Blue Score: {} | Miner Balance: {:.2} IMN",
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

