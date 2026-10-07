use crate::state::{DagLedger, SharedLedger};
use imoney_core::{Block, Hash, Transaction};
use imoney_pow::MoneyPrinterPow;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, RwLock};

/// Most blocks held while waiting for their parents to arrive.
const MAX_ORPHAN_BLOCKS: usize = 1_000;

/// Largest single wire message accepted from a peer.
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Reads one newline-terminated message into `buf`, failing once it exceeds `MAX_MESSAGE_BYTES`.
/// Returns `Ok(false)` on EOF. Progress is kept in `buf`, so the future may be dropped and re-polled.
async fn read_bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(false);
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let take = newline.unwrap_or(available.len());
        buf.extend_from_slice(&available[..take]);
        reader.consume(newline.map_or(take, |pos| pos + 1));

        if buf.len() > MAX_MESSAGE_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "peer message too large"));
        }
        if newline.is_some() {
            return Ok(true);
        }
    }
}

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
    /// Response with a full block (header and transactions)
    Block(Block),
    /// Gossip announcement of a newly mined block
    NewBlock(Block),
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
    /// Blocks received before their parents, keyed by block hash.
    orphans: Mutex<HashMap<Hash, Block>>,
    /// False while the node still has to catch up with the peers it was told to connect to.
    synced: AtomicBool,
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
            orphans: Mutex::new(HashMap::new()),
            synced: AtomicBool::new(true),
            broadcast_tx,
            p2p_port,
        }
    }

    /// Broadcast a newly mined block to all connected peers.
    pub fn broadcast_block(&self, block: Block) {
        let _ = self.broadcast_tx.send(PeerMessage::NewBlock(block));
    }

    /// Broadcast a new transaction to all connected peers.
    pub fn broadcast_transaction(&self, tx: Transaction) {
        let _ = self.broadcast_tx.send(PeerMessage::NewTransaction(tx));
    }

    /// Marks the node as behind until it has caught up with a peer. Called when outbound
    /// peers are configured, so the miner does not build a private chain while syncing.
    pub fn require_initial_sync(&self) {
        self.synced.store(false, Ordering::SeqCst);
    }

    /// True once the node holds every block its peers have told it about.
    pub fn is_synced(&self) -> bool {
        self.synced.load(Ordering::SeqCst)
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
        let mut line_buf: Vec<u8> = Vec::new();
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
                res = read_bounded_line(&mut reader, &mut line_buf) => {
                    match res {
                        Ok(false) => break, // Connection closed EOF
                        Ok(true) => {
                            if let Ok(msg) = serde_json::from_slice::<PeerMessage>(&line_buf) {
                                self.process_peer_message(&msg, &mut writer).await;
                            }
                            line_buf.clear();
                        }
                        Err(_) => break, // I/O error or oversized message
                    }
                }
            }
        }

        self.peers.write().await.remove(&peer_addr);
        println!("[-] Peer session ended: {}", peer_addr);
    }

    /// Holds a block until its parents arrive.
    fn park_orphan(&self, block: Block) {
        let mut orphans = self.orphans.lock().unwrap();
        if orphans.len() < MAX_ORPHAN_BLOCKS {
            orphans.insert(block.hash(), block);
        }
    }

    /// Adds a block whose parents are known, then any waiting blocks it unblocks.
    /// Returns the hash and resulting virtual blue score of each block added.
    fn connect_block_and_orphans(&self, ledger: &mut DagLedger, block: Block) -> Vec<(Hash, u64)> {
        let mut connected = Vec::new();
        let mut queue = vec![block];
        while let Some(next) = queue.pop() {
            // Invalid and duplicate blocks are dropped
            let Ok(hash) = ledger.add_block(next, &self.pow) else {
                continue;
            };
            connected.push((hash, ledger.virtual_blue_score));

            let mut orphans = self.orphans.lock().unwrap();
            let ready: Vec<Hash> = orphans
                .iter()
                .filter(|(_, orphan)| orphan.header.parents.iter().all(|p| ledger.blocks.contains_key(p)))
                .map(|(orphan_hash, _)| *orphan_hash)
                .collect();
            for orphan_hash in ready {
                if let Some(orphan) = orphans.remove(&orphan_hash) {
                    queue.push(orphan);
                }
            }
        }
        // Everything received so far now connects to the DAG
        if !connected.is_empty() && self.orphans.lock().unwrap().is_empty() {
            self.synced.store(true, Ordering::SeqCst);
        }
        connected
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
                // Request any unknown tip blocks
                let unknown: Vec<Hash> = {
                    let ledger = self.ledger.read().await;
                    tips.iter().filter(|t| !ledger.blocks.contains_key(*t)).copied().collect()
                };
                if unknown.is_empty() && self.orphans.lock().unwrap().is_empty() {
                    self.synced.store(true, Ordering::SeqCst);
                }
                for tip in unknown {
                    let req = PeerMessage::GetBlock(tip);
                    if let Ok(s) = serde_json::to_string(&req) {
                        let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                    }
                }
            }
            PeerMessage::GetBlock(hash) => {
                let block = self.ledger.read().await.storage.get_block(hash).ok().flatten();
                if let Some(block) = block {
                    let reply = PeerMessage::Block(block);
                    if let Ok(s) = serde_json::to_string(&reply) {
                        let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                    }
                }
            }
            PeerMessage::Block(block) | PeerMessage::NewBlock(block) => {
                // Attempt to insert incoming block into DAG. The ledger lock is released before any socket write.
                let (parents_needed, connected) = {
                    let mut ledger = self.ledger.write().await;
                    let parents_needed: Vec<Hash> = block
                        .header
                        .parents
                        .iter()
                        .filter(|p| !ledger.blocks.contains_key(p))
                        .copied()
                        .collect();
                    let connected = if parents_needed.is_empty() {
                        self.connect_block_and_orphans(&mut ledger, block.clone())
                    } else {
                        self.park_orphan(block.clone());
                        Vec::new()
                    };
                    (parents_needed, connected)
                };

                // If missing parents, request them from peer
                for parent in parents_needed {
                    let req = PeerMessage::GetBlock(parent);
                    if let Ok(s) = serde_json::to_string(&req) {
                        let _ = writer.write_all(format!("{}\n", s).as_bytes()).await;
                    }
                }

                for (new_hash, blue_score) in &connected {
                    println!(
                        "[+] P2P Block Synchronized: {} | Blue Score: {}",
                        new_hash, blue_score
                    );
                }
                // Forward to other peers if it was a newly announced block
                if !connected.is_empty() && matches!(msg, PeerMessage::NewBlock(_)) {
                    let _ = self.broadcast_tx.send(PeerMessage::NewBlock(block.clone()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_line_reader_splits_messages_and_reports_eof() {
        let mut reader = BufReader::with_capacity(4, &b"first\nsecond\n"[..]);
        let mut buf = Vec::new();

        assert!(read_bounded_line(&mut reader, &mut buf).await.unwrap());
        assert_eq!(buf, b"first");
        buf.clear();
        assert!(read_bounded_line(&mut reader, &mut buf).await.unwrap());
        assert_eq!(buf, b"second");
        buf.clear();
        assert!(!read_bounded_line(&mut reader, &mut buf).await.unwrap());
    }

    #[tokio::test]
    async fn bounded_line_reader_rejects_oversized_message() {
        let oversized = vec![b'a'; MAX_MESSAGE_BYTES + 2];
        let mut reader = BufReader::new(&oversized[..]);
        let mut buf = Vec::new();

        let err = read_bounded_line(&mut reader, &mut buf).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
