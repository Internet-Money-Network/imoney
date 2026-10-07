use clap::Parser;
use imoney_node::genesis::create_testnet_genesis;
use imoney_core::constants::{CURRENCY_NAME, TICKER};
use imoney_core::{Address, AddressType, Network};
use imoney_pow::{MoneyPrinterPow, PowMode, PowParams};
use imoney_node::p2p::PeerManager;
use imoney_node::rpc::create_router;
use imoney_node::state::{DagLedger, SharedLedger};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Hashes tried per auto-miner round before the block template is refreshed.
const AUTO_MINE_BATCH: u64 = 100_000;

#[derive(Parser, Debug)]
#[command(author, version, about = "Internet Money (IMN) - Full Node Daemon", long_about = None)]
struct Args {
    /// RPC server bind address (e.g., 127.0.0.1:18556)
    #[arg(long, default_value = "127.0.0.1:18556")]
    rpc_bind: String,

    /// P2P gossip server bind address (e.g., 0.0.0.0:18555)
    #[arg(long, default_value = "0.0.0.0:18555")]
    p2p_bind: String,

    /// Peers to connect to, as host:port or ip:port (comma-separated or multiple flags)
    #[arg(long, value_delimiter = ',')]
    peers: Vec<String>,

    /// Path to persistent database directory
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,

    /// Mining payout address (Bech32, e.g., imntest:q...)
    #[arg(long)]
    mining_address: Option<String>,

    /// Address that receives this node's half of the fee on payments it serves
    /// (defaults to the mining address)
    #[arg(long)]
    service_address: Option<String>,

    /// Run on a private network with this name. Nodes only talk to nodes started with the
    /// same name, so a test network can never mix with the public one.
    #[arg(long)]
    devnet: Option<String>,

    /// Delete the transactions of blocks buried deeper than three times the finality depth
    /// (36 hours). The node keeps headers and the current ledger, uses far less disk, and can
    /// no longer serve old history to nodes that are syncing.
    #[arg(long, default_value_t = false)]
    prune: bool,

    /// Confirmations after which a payment is reported as final (default 60, about 5 minutes)
    #[arg(long)]
    final_confirmations: Option<u64>,

    /// Require this bearer token for block submission
    #[arg(long)]
    rpc_token: Option<String>,

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
    println!("  PoW: Money Printer (FishHash, memory-hard)");
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

    // Money Printer is FishHash with a seed derived from this network's genesis hash.
    // Verifying blocks needs only the light cache; mining holds the full dataset in memory.
    let pow_mode = if args.auto_mine { PowMode::Full } else { PowMode::Light };
    let pow = Arc::new(MoneyPrinterPow::new(PowParams::testnet(), create_testnet_genesis().hash(), pow_mode));
    println!("[*] Building Money Printer (FishHash) memory in {:?} mode...", pow_mode);
    pow.context();
    println!("[+] Money Printer PoW ready.");

    println!("[*] Opening persistent database at {:?}...", db_path);
    let mut ledger_instance = DagLedger::open(&db_path, mining_address)?;
    if args.prune {
        let depth = 3 * ledger_instance.params.finality_depth;
        let pruned = ledger_instance.enable_pruning(depth)?;
        println!("[*] Pruning enabled: block contents deeper than {} blue score are deleted ({} blocks pruned now).", depth, pruned);
    }
    if let Some(confirmations) = args.final_confirmations {
        ledger_instance.final_confirmations = confirmations;
    }
    if let Some(s) = &args.service_address {
        ledger_instance.service_address = Some(Address::decode(s)?);
    }
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
    let p2p_manager = Arc::new(match &args.devnet {
        Some(name) => {
            // A network identifier derived from the name keeps this network apart from all others
            let magic: [u8; 4] = blake3::hash(format!("imoney devnet: {}", name).as_bytes()).as_bytes()[..4].try_into().unwrap();
            println!("[*] Private network '{}': only nodes started with the same --devnet name can connect.", name);
            PeerManager::with_magic(ledger.clone(), pow.clone(), p2p_bind_addr.port(), magic)
        }
        None => PeerManager::new(ledger.clone(), pow.clone(), p2p_bind_addr.port()),
    });
    
    // Start incoming P2P TCP gossip server
    p2p_manager.clone().start_server(p2p_bind_addr).await?;

    // Peers learned in earlier runs are remembered, so the node can rejoin without its seeds
    p2p_manager.use_peer_store(args.data_dir.join("peers.txt"));

    // Connect outbound to configured peer nodes. A host name may resolve to several nodes.
    for peer_str in args.peers {
        let trimmed = peer_str.trim();
        if !trimmed.is_empty() {
            match tokio::net::lookup_host(trimmed).await {
                Ok(resolved) => {
                    for peer_addr in resolved {
                        p2p_manager.require_initial_sync();
                        p2p_manager.clone().connect_to_peer(peer_addr);
                    }
                }
                Err(e) => eprintln!("[-] Warning: Could not resolve peer '{}': {}", trimmed, e),
            }
        }
    }

    // Background auto-miner task
    if args.auto_mine {
        let ledger_clone = ledger.clone();
        let pow_clone = pow.clone();
        let p2p_clone = p2p_manager.clone();
        tokio::spawn(async move {
            println!("[*] Local background auto-miner started (difficulty targets ~5s block interval)...");
            loop {
                // Mining before catching up with peers would only build a private fork
                if !p2p_clone.is_synced() {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                }

                // Work on a fresh template each round so new tips and transactions are picked up
                let template = ledger_clone.read().await.get_mining_template(None);
                let pre_pow_hash = template.pre_pow_hash;
                let bits = template.block.header.bits;
                let start_nonce = rand::random::<u32>() as u64;
                let worker = pow_clone.clone();
                let found = tokio::task::spawn_blocking(move || {
                    worker.mine(&pre_pow_hash, bits, start_nonce, AUTO_MINE_BATCH, Arc::new(AtomicBool::new(false)))
                })
                .await
                .ok()
                .flatten();

                if let Some((nonce, _hash)) = found {
                    let candidate = template.into_block(nonce);

                    let mut writer = ledger_clone.write().await;
                    match writer.add_block(candidate.clone(), &pow_clone) {
                        Ok(new_hash) => {
                            // Gossip block across P2P network
                            p2p_clone.broadcast_block(candidate);

                            let balance_info = writer.mining_address.as_ref().and_then(|a| writer.get_balance(a).ok()).map(|(_, coins)| coins).unwrap_or(0.0);
                            println!(
                                "[+] Mined Block #{} | Hash: {} | Blue Score: {} | Bits: 0x{:08x} | Miner Balance: {:.2} IMN",
                                writer.virtual_daa_score,
                                new_hash,
                                writer.virtual_blue_score,
                                bits,
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

    let shutdown_peers = p2p_manager.clone();
    let app = create_router(ledger, pow, p2p_manager, args.rpc_token);
    let addr: SocketAddr = args.rpc_bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("[+] HTTP RPC & Web Dashboard listening on http://{}", addr);

    // On Ctrl-C: stop taking requests and save what has been learned about peers. The ledger
    // needs no special handling: every block is written in one atomic database transaction.
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            println!("[*] Shutting down...");
        })
        .await?;
    shutdown_peers.save_peers();
    Ok(())
}

