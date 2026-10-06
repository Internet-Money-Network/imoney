use crate::state::SharedLedger;
use imoney_core::{BlockHeader, Hash, Transaction};
use imoney_pow::MoneyPrinterPow;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, RwLock};

/// Wire messages exchanged across P2P TCP connections.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PeerMessage {
    /// Initial handshake with agent version and current blue score
    Handshake {
        version: u32,
        user_agent: String,
        blue_score: u64,
        listen_port: Option<u16>,
    },
    /// Request current tips from peer
    GetTips,
    /// Response containing DAG tip hashes
    Tips(Vec<Hash>),
    /// Request a specific block by hash
    GetBlock(Hash),
    /// Response with full block header
    Block(BlockHeader),
    /// Gossip announcement of a newly mined block
    NewBlock(BlockHeader),
    /// Gossip announcement of a new pending transaction
    NewTransaction(Transaction),
    /// Ping heartbeat
    Ping(u64),
    /// Pong heartbeat reply
    Pong(u64),
}

/// Manages active P2P TCP connections, gossip broadcasting, and DAG synchronization.
pub struct PeerManager {
    ledger: SharedLedger,
    pow: Arc<MoneyPrinterPow>,
    peers: Arc<RwLock<HashSet<SocketAddr>>>,
    broadcast_tx: broadcast::Sender<PeerMessage>,
    p2p_port: u16,
}

impl PeerManager {
    pub fn new(ledger: SharedLedger, pow: Arc<MoneyPrinterPow>, p2p_port: u16) -> Self {
        let (broadcast_tx, _) = broadcast::channel(1024);
        Self {
            ledger,
            pow,
            peers: Arc::new(RwLock::new(HashSet::new())),
            broadcast_tx,
            p2p_port,
        }
    }

    /// Broadcast a newly mined block to all connected peers.
    pub fn broadcast_block(&self, header: BlockHeader) {
        let _ = self.broadcast_tx.send(PeerMessage::NewBlock(header));
    }

    /// Broadcast a new transaction to all connected peers.
    pub fn broadcast_transaction(&self, tx: Transaction) {
        let _ = self.broadcast_tx.send(PeerMessage::NewTransaction(tx));
    }

    /// Returns list of currently connected peers.
    pub async fn get_connected_peers(&self) -> Vec<String> {
        self.peers.read().await.iter().map(|p| p.to_string()).collect()
    }

    /// Starts the incoming TCP P2P listener.
    pub async fn start_server(self: Arc<Self>, bind_addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind(bind_addr).await?;
        println!("[+] P2P Gossip Server listening on tcp://{}", bind_addr);

        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        println!("[+] Incoming P2P connection from {}", peer_addr);
                        let m = manager.clone();
                        tokio::spawn(async move {
                            m.handle_peer(stream, peer_addr, false).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("[-] P2P accept error: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    /// Connects outbound to a seed node or peer.
    pub fn connect_to_peer(self: Arc<Self>, peer_addr: SocketAddr) {
        let manager = self.clone();
        tokio::spawn(async move {
            // Retry loop
            let mut backoff = Duration::from_secs(2);
            loop {
                println!("[*] Attempting outbound P2P connection to {}...", peer_addr);
                match TcpStream::connect(peer_addr).await {
                    Ok(stream) => {
                        println!("[+] Connected outbound to peer {}", peer_addr);
                        manager.handle_peer(stream, peer_addr, true).await;
                        println!("[-] Disconnected from peer {}. Reconnecting...", peer_addr);
                        backoff = Duration::from_secs(3);
                    }
                    Err(e) => {
                        println!("[-] Failed to connect to peer {}: {}. Retrying in {:?}...", peer_addr, e, backoff);
                    }
                }
                tokio::time::sleep(backoff).await;
                if backoff < Duration::from_secs(30) {
                    backoff *= 2;
                }
            }
        });
    }

    /// Handles bidirectional communication with a single peer.
    async fn handle_peer(&self, stream: TcpStream, peer_addr: SocketAddr, is_outbound: bool) {
        self.peers.write().await.insert(peer_addr);
        let mut bcast_rx = self.broadcast_tx.subscribe();

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);

        // Send Initial Handshake
        let blue_score = self.ledger.read().await.virtual_blue_score;
        let handshake = PeerMessage::Handshake {
            version: 1,
            user_agent: "/imoney:0.1.0/".to_string(),
            blue_score,
            listen_port: Some(self.p2p_port),
        };
        if let Ok(line) = serde_json::to_string(&handshake) {
            let _ = writer.write_all(format!("{}\n", line).as_bytes()).await;
        }

        // If outbound connection, request tips to start initial sync
        if is_outbound {
            if let Ok(line) = serde_json::to_string(&PeerMessage::GetTips) {
                let _ = writer.write_all(format!("{}\n", line).as_bytes()).await;
            }
        }

        // Periodic Ping Heartbeat task
        let (ping_tx, mut ping_rx) = tokio::sync::mpsc::channel::<PeerMessage>(16);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                let ts = chrono::Utc::now().timestamp_millis() as u64;
                if ping_tx.send(PeerMessage::Ping(ts)).await.is_err() {
                    break;
                }
            }
        });

        // Loop handling incoming peer lines and outbound broadcast messages
        let mut line_buf = String::new();
        loop {
            tokio::select! {
                // Outbound messages to send to this peer (blocks/transactions to gossip)
                Ok(msg) = bcast_rx.recv() => {
                    if let Ok(serialized) = serde_json::to_string(&msg) {
                        if writer.write_all(format!("{}\n", serialized).as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
                // Outbound pings
                Some(ping_msg) = ping_rx.recv() => {
                    if let Ok(serialized) = serde_json::to_string(&ping_msg) {
                        if writer.write_all(format!("{}\n", serialized).as_bytes()).await.is_err() {
                            break;
                        }
                    }
                }
                // Inbound messages from the peer
                res = reader.read_line(&mut line_buf) => {
                    match res {
                        Ok(0) => break, // Connection closed EOF
                        Ok(_) => {
                            let trimmed = line_buf.trim();
                            if !trimmed.is_empty() {
                                if let Ok(msg) = serde_json::from_str::<PeerMessage>(trimmed) {
                                    self.process_peer_message(&msg, &mut writer).await;
                                }
                            }
                            line_buf.clear();
                        }
                        Err(_) => break,
                    }
                }
            }
        }

        self.peers.write().await.remove(&peer_addr);
        println!("[-] Peer session ended: {}", peer_addr);
    }

    /// Dispatches inbound peer messages.
    async fn process_peer_message(&self, msg: &PeerMessage, writer: &mut tokio::net::tcp::OwnedWriteHalf) {
        match msg {
            PeerMessage::Handshake { blue_score, .. } => {
                let local_blue = self.ledger.read().await.virtual_blue_score;
                if *blue_score > local_blue {
                    // Remote peer is ahead, ask for their tips
                    let req = PeerMessage::GetTips;
                    if let Ok(s) = serde_json::to_string(&req) {
                        let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                    }
                }
            }
            PeerMessage::GetTips => {
                let tips = self.ledger.read().await.tips.iter().copied().collect();
                let reply = PeerMessage::Tips(tips);
                if let Ok(s) = serde_json::to_string(&reply) {
                    let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                }
            }
            PeerMessage::Tips(tips) => {
                // Request any unknown tip headers
                let ledger = self.ledger.read().await;
                for tip in tips {
                    if !ledger.blocks.contains_key(tip) {
                        let req = PeerMessage::GetBlock(*tip);
                        if let Ok(s) = serde_json::to_string(&req) {
                            let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                        }
                    }
                }
            }
            PeerMessage::GetBlock(hash) => {
                let ledger = self.ledger.read().await;
                if let Some(header) = ledger.blocks.get(hash) {
                    let reply = PeerMessage::Block(header.clone());
                    if let Ok(s) = serde_json::to_string(&reply) {
                        let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                    }
                }
            }
            PeerMessage::Block(header) | PeerMessage::NewBlock(header) => {
                // Attempt to insert incoming block into DAG
                let mut ledger = self.ledger.write().await;
                let parents_needed: Vec<Hash> = header
                    .parents
                    .iter()
                    .filter(|p| !ledger.blocks.contains_key(p))
                    .copied()
                    .collect();

                // If missing parents, request them from peer
                for parent in parents_needed {
                    let req = PeerMessage::GetBlock(parent);
                    if let Ok(s) = serde_json::to_string(&req) {
                        let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                    }
                }

                match ledger.add_block(header.clone(), &self.pow) {
                    Ok(new_hash) => {
                        println!(
                            "[+] P2P Block Synchronized: {} | Blue Score: {}",
                            new_hash, ledger.virtual_blue_score
                        );
                        // Forward to other peers if it was a newly announced block
                        if matches!(msg, PeerMessage::NewBlock(_)) {
                            let _ = self.broadcast_tx.send(PeerMessage::NewBlock(header.clone()));
                        }
                    }
                    Err(e) => {
                        // Could be unknown parent (requested above) or duplicate, ignore benign errors
                        let err_str = e.to_string();
                        if !err_str.contains("Block already exists") {
                            // Non-duplicate error
                        }
                    }
                }
            }
            PeerMessage::NewTransaction(tx) => {
                let mut ledger = self.ledger.write().await;
                match ledger.broadcast_transaction(tx.clone()) {
                    Ok(tx_id) => {
                        println!("[+] P2P Transaction Admitted to Mempool: {}", tx_id);
                        // Forward to other peers
                        let _ = self.broadcast_tx.send(PeerMessage::NewTransaction(tx.clone()));
                    }
                    Err(_) => {
                        // Duplicate or invalid, skip
                    }
                }
            }
            PeerMessage::Ping(nonce) => {
                let reply = PeerMessage::Pong(*nonce);
                if let Ok(s) = serde_json::to_string(&reply) {
                    let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                }
            }
            PeerMessage::Pong(_) => {}
        }
    }
}
