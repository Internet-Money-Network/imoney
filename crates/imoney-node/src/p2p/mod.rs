//! Peer-to-peer networking: framed binary protocol, handshake, block and transaction relay,
//! batch sync for nodes that are behind, peer discovery and misbehaviour bans.

pub mod codec;
#[cfg(test)]
mod tests;

use crate::genesis::TESTNET_MAGIC;
use crate::state::{DagLedger, SharedLedger, StateError};
use codec::{write_frame, FrameReader, Message, PROTOCOL_VERSION};
use imoney_core::{Block, Hash, Transaction};
use imoney_pow::HallmarkPow;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};

/// Most blocks held while waiting for their parents to arrive.
const MAX_ORPHAN_BLOCKS: usize = 1_000;
/// Most connections accepted from other nodes.
const MAX_INBOUND_PEERS: usize = 64;
/// Number of connections this node tries to keep open to other nodes.
const TARGET_OUTBOUND_PEERS: usize = 8;
/// Most node addresses remembered for dialing.
const MAX_KNOWN_ADDRS: usize = 2_000;
/// Most addresses sent in one reply.
const MAX_ADDRS_PER_REPLY: usize = 100;
/// Misbehaviour score at which a peer is disconnected and its IP banned.
const BAN_THRESHOLD: u32 = 100;
const BAN_DURATION: Duration = Duration::from_secs(10 * 60);
/// A peer's misbehaviour score fades by one point for each of these that passes.
const SCORE_FADE: Duration = Duration::from_secs(30);
/// A block that fails proof of work cost its sender nothing to make: abuse, and nothing else.
const PENALTY_INVALID_POW: u32 = BAN_THRESHOLD;
/// A block that breaks another rule. An honest peer can relay one, for instance when it runs
/// an older version than ours, so this only adds up if it keeps happening.
const PENALTY_INVALID_BLOCK: u32 = 5;
/// With this many blocks waiting for parents, fetching them one by one is the slow way round:
/// ask for a batch sync instead.
const ORPHANS_BEFORE_RESYNC: usize = 32;
/// Limits on one batch of blocks sent to a syncing peer.
const SYNC_BATCH_BLOCKS: usize = 200;
const SYNC_BATCH_BYTES: usize = 2_000_000;
const DIAL_INTERVAL: Duration = Duration::from_secs(1);
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Messages queued for one peer before further ones are dropped.
const PEER_QUEUE: usize = 2_048;
/// How often each peer is asked for its tips, so a missed announcement cannot leave a node behind.
const TIP_POLL_INTERVAL: Duration = Duration::from_secs(15);
/// How long a node waits for its configured peers before it stops holding back the miner.
const SYNC_WAIT: Duration = Duration::from_secs(30);
/// How often learned peer addresses are written to disk.
const PEER_SAVE_INTERVAL: Duration = Duration::from_secs(60);

struct PeerHandle {
    sender: mpsc::Sender<Message>,
    outbound: bool,
    /// Set once the handshake has completed.
    node_id: Option<u64>,
    /// Where the peer accepts connections, as it told us.
    listen_addr: Option<SocketAddr>,
    score: u32,
    /// When the score last changed. It fades with time, so only sustained trouble adds up.
    scored_at: Instant,
    /// Wakes the connection's task so it closes.
    shutdown: Arc<Notify>,
}

/// Manages active P2P TCP connections, gossip broadcasting, and DAG synchronization.
pub struct PeerManager {
    ledger: SharedLedger,
    pow: Arc<HallmarkPow>,
    magic: [u8; 4],
    node_id: u64,
    listen_port: AtomicU16,
    peers: Mutex<HashMap<SocketAddr, PeerHandle>>,
    /// Addresses of nodes worth dialing: configured seeds plus those learned from peers.
    known_addrs: Mutex<HashSet<SocketAddr>>,
    /// Addresses that turned out to be this node itself.
    self_addrs: Mutex<HashSet<SocketAddr>>,
    dialing: Mutex<HashSet<SocketAddr>>,
    banned: Mutex<HashMap<IpAddr, Instant>>,
    /// Blocks received before their parents, keyed by block hash.
    orphans: Mutex<HashMap<Hash, Block>>,
    /// False while the node still has to catch up with the peers it was told to connect to.
    synced: AtomicBool,
    /// When initial sync was requested; after `sync_wait` with no peer, the node stops waiting.
    sync_requested_at: Mutex<Option<Instant>>,
    tip_poll_interval: Duration,
    sync_wait: Duration,
    /// File that learned peer addresses are saved to, so a restart does not depend on seeds alone.
    peer_store: Mutex<Option<PathBuf>>,
}

impl PeerManager {
    pub fn new(ledger: SharedLedger, pow: Arc<HallmarkPow>, p2p_port: u16) -> Self {
        Self::with_magic(ledger, pow, p2p_port, TESTNET_MAGIC)
    }

    pub fn with_magic(ledger: SharedLedger, pow: Arc<HallmarkPow>, p2p_port: u16, magic: [u8; 4]) -> Self {
        Self {
            ledger,
            pow,
            magic,
            node_id: rand::random(),
            listen_port: AtomicU16::new(p2p_port),
            peers: Mutex::new(HashMap::new()),
            known_addrs: Mutex::new(HashSet::new()),
            self_addrs: Mutex::new(HashSet::new()),
            dialing: Mutex::new(HashSet::new()),
            banned: Mutex::new(HashMap::new()),
            orphans: Mutex::new(HashMap::new()),
            synced: AtomicBool::new(true),
            sync_requested_at: Mutex::new(None),
            tip_poll_interval: TIP_POLL_INTERVAL,
            sync_wait: SYNC_WAIT,
            peer_store: Mutex::new(None),
        }
    }

    /// Overrides the tip-polling interval and the initial-sync wait.
    pub fn with_timing(mut self, tip_poll_interval: Duration, sync_wait: Duration) -> Self {
        self.tip_poll_interval = tip_poll_interval;
        self.sync_wait = sync_wait;
        self
    }

    /// Loads previously saved peer addresses from `path` and keeps saving them there.
    pub fn use_peer_store(&self, path: PathBuf) {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for addr in contents.lines().filter_map(|line| line.trim().parse::<SocketAddr>().ok()) {
                self.remember_addr(addr);
            }
        }
        *self.peer_store.lock().unwrap() = Some(path);
    }

    /// Writes the known peer addresses to the peer store, if one is configured.
    pub fn save_peers(&self) {
        let Some(path) = self.peer_store.lock().unwrap().clone() else {
            return;
        };
        let mut lines: Vec<String> = self.known_addrs.lock().unwrap().iter().map(|addr| addr.to_string()).collect();
        lines.sort();
        let _ = std::fs::write(path, lines.join("\n"));
    }

    /// Stops holding back the miner when no configured peer could be reached in time.
    /// Without this, a node whose seeds are all down would wait forever and the chain would stall.
    fn give_up_waiting_for_sync(&self) {
        if self.is_synced() {
            return;
        }
        let waited_long_enough =
            self.sync_requested_at.lock().unwrap().is_some_and(|since| since.elapsed() > self.sync_wait);
        let has_peer = self.peers.lock().unwrap().values().any(|p| p.node_id.is_some());
        if waited_long_enough && !has_peer {
            println!("[*] No configured peer reachable after {:?}; continuing without initial sync.", self.sync_wait);
            self.synced.store(true, Ordering::SeqCst);
        }
    }

    /// Announce a newly mined block to all connected peers.
    pub fn broadcast_block(&self, block: Block) {
        self.announce(Message::InvBlock(block.hash()), None);
    }

    /// Announce a new transaction to all connected peers.
    pub fn broadcast_transaction(&self, tx: Transaction) {
        self.announce(Message::InvTx(tx.id()), None);
    }

    /// Queues a message for every handshaken peer except `except`.
    fn announce(&self, message: Message, except: Option<SocketAddr>) {
        for (addr, peer) in self.peers.lock().unwrap().iter() {
            if peer.node_id.is_some() && Some(*addr) != except {
                let _ = peer.sender.try_send(message.clone());
            }
        }
    }

    /// Marks the node as behind until it has caught up with a peer. Called when outbound
    /// peers are configured, so the miner does not build a private chain while syncing.
    pub fn require_initial_sync(&self) {
        *self.sync_requested_at.lock().unwrap() = Some(Instant::now());
        self.synced.store(false, Ordering::SeqCst);
    }

    /// True once the node holds every block its peers have told it about.
    pub fn is_synced(&self) -> bool {
        self.synced.load(Ordering::SeqCst)
    }

    /// Returns list of currently connected peers.
    pub async fn get_connected_peers(&self) -> Vec<String> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, peer)| peer.node_id.is_some())
            .map(|(addr, _)| addr.to_string())
            .collect()
    }

    /// Starts the incoming TCP P2P listener and the task that keeps outbound connections open.
    /// Returns the address actually bound.
    pub async fn start_server(self: Arc<Self>, bind_addr: SocketAddr) -> Result<SocketAddr, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        self.listen_port.store(local_addr.port(), Ordering::SeqCst);
        println!("[+] P2P Gossip Server listening on tcp://{}", local_addr);

        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
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

        let manager = self.clone();
        tokio::spawn(async move {
            let mut last_save = Instant::now();
            loop {
                tokio::time::sleep(DIAL_INTERVAL).await;
                manager.dial_more_peers();
                manager.give_up_waiting_for_sync();
                if last_save.elapsed() > PEER_SAVE_INTERVAL {
                    manager.save_peers();
                    last_save = Instant::now();
                }
            }
        });

        Ok(local_addr)
    }

    /// Adds a seed node or peer to dial. The connection is retried for as long as it is wanted.
    pub fn connect_to_peer(self: Arc<Self>, peer_addr: SocketAddr) {
        self.known_addrs.lock().unwrap().insert(peer_addr);
    }

    fn is_banned(&self, ip: &IpAddr) -> bool {
        let mut banned = self.banned.lock().unwrap();
        banned.retain(|_, until| *until > Instant::now());
        banned.contains_key(ip)
    }

    /// Dials known addresses until the outbound target is met.
    fn dial_more_peers(self: &Arc<Self>) {
        let candidates: Vec<SocketAddr> = {
            let peers = self.peers.lock().unwrap();
            let dialing = self.dialing.lock().unwrap();
            let self_addrs = self.self_addrs.lock().unwrap();
            let outbound = peers.values().filter(|p| p.outbound).count() + dialing.len();
            let wanted = TARGET_OUTBOUND_PEERS.saturating_sub(outbound);
            self.known_addrs
                .lock()
                .unwrap()
                .iter()
                .filter(|addr| {
                    !peers.contains_key(*addr)
                        && !peers.values().any(|p| p.listen_addr.as_ref() == Some(*addr))
                        && !dialing.contains(*addr)
                        && !self_addrs.contains(*addr)
                })
                .take(wanted)
                .copied()
                .collect()
        };

        for addr in candidates {
            if self.is_banned(&addr.ip()) {
                continue;
            }
            self.dialing.lock().unwrap().insert(addr);
            let manager = self.clone();
            tokio::spawn(async move {
                if let Ok(Ok(stream)) = tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(addr)).await {
                    manager.dialing.lock().unwrap().remove(&addr);
                    manager.handle_peer(stream, addr, true).await;
                } else {
                    manager.dialing.lock().unwrap().remove(&addr);
                }
            });
        }
    }

    /// Runs one connection from handshake to close.
    async fn handle_peer(&self, stream: TcpStream, peer_addr: SocketAddr, is_outbound: bool) {
        if self.is_banned(&peer_addr.ip()) {
            return;
        }
        let shutdown = Arc::new(Notify::new());
        let (sender, mut outgoing) = mpsc::channel::<Message>(PEER_QUEUE);
        {
            let mut peers = self.peers.lock().unwrap();
            if !is_outbound && peers.values().filter(|p| !p.outbound).count() >= MAX_INBOUND_PEERS {
                return;
            }
            peers.insert(
                peer_addr,
                PeerHandle {
                    sender: sender.clone(),
                    outbound: is_outbound,
                    node_id: None,
                    listen_addr: None,
                    score: 0,
                    scored_at: Instant::now(),
                    shutdown: shutdown.clone(),
                },
            );
        }

        let (mut reader, mut writer) = stream.into_split();
        let magic = self.magic;
        let writer_task = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if write_frame(&mut writer, magic, &message).await.is_err() {
                    break;
                }
            }
        });

        // Send Initial Handshake
        let (network, genesis, archival) = {
            let ledger = self.ledger.read().await;
            (ledger.network.id(), ledger.genesis_hash, ledger.prune_depth.is_none())
        };
        let _ = sender.try_send(Message::Version {
            protocol: PROTOCOL_VERSION,
            network,
            genesis,
            node_id: self.node_id,
            listen_port: self.listen_port.load(Ordering::SeqCst),
            user_agent: "/imoney:0.1.0/".to_string(),
            archival,
        });

        let mut frames = FrameReader::default();
        let mut ping = tokio::time::interval(PING_INTERVAL);
        let mut tip_poll = tokio::time::interval(self.tip_poll_interval);
        let mut last_heard = Instant::now();
        let mut handshaken = false;
        loop {
            tokio::select! {
                result = frames.read(&mut reader, magic) => {
                    match result {
                        Ok(Some(message)) => {
                            last_heard = Instant::now();
                            if !handshaken {
                                if !self.on_version(peer_addr, is_outbound, &sender, message).await {
                                    break;
                                }
                                handshaken = true;
                            } else {
                                self.on_message(peer_addr, &sender, message).await;
                            }
                        }
                        Ok(None) => break, // Connection closed
                        Err(e) => {
                            // Bytes that are not this protocol: wrong network or garbage
                            if e.kind() == std::io::ErrorKind::InvalidData {
                                self.misbehave(peer_addr, BAN_THRESHOLD);
                            }
                            break;
                        }
                    }
                }
                _ = ping.tick() => {
                    if last_heard.elapsed() > IDLE_TIMEOUT {
                        break;
                    }
                    let _ = sender.try_send(Message::Ping(chrono::Utc::now().timestamp_millis() as u64));
                }
                // Announcements can be lost (a full queue, a dropped link). Comparing tips
                // regularly means any difference is noticed and repaired within one interval.
                _ = tip_poll.tick() => {
                    if handshaken {
                        let _ = sender.try_send(Message::GetTips);
                    }
                }
                _ = shutdown.notified() => break,
            }
        }

        // Only remove the entry if it still belongs to this connection
        {
            let mut peers = self.peers.lock().unwrap();
            if peers.get(&peer_addr).is_some_and(|p| Arc::ptr_eq(&p.shutdown, &shutdown)) {
                peers.remove(&peer_addr);
            }
        }
        writer_task.abort();
        if handshaken {
            println!("[-] Peer session ended: {}", peer_addr);
        }
    }

    /// Checks the peer's first message. Returns false when the connection must be dropped.
    async fn on_version(&self, peer_addr: SocketAddr, is_outbound: bool, sender: &mpsc::Sender<Message>, message: Message) -> bool {
        let Message::Version { protocol, network, genesis, node_id, listen_port, archival, .. } = message else {
            return false;
        };
        let (our_network, our_genesis, locator) = {
            let ledger = self.ledger.read().await;
            (ledger.network.id(), ledger.genesis_hash, ledger.locator())
        };
        if protocol != PROTOCOL_VERSION || network != our_network || genesis != our_genesis {
            return false;
        }
        if node_id == self.node_id {
            // Dialed ourselves: remember not to try again
            self.self_addrs.lock().unwrap().insert(peer_addr);
            return false;
        }

        let listen_addr = (listen_port != 0).then(|| SocketAddr::new(peer_addr.ip(), listen_port));
        {
            let mut peers = self.peers.lock().unwrap();
            let duplicate = peers
                .iter()
                .find(|(addr, p)| **addr != peer_addr && p.node_id == Some(node_id))
                .map(|(addr, p)| (*addr, p.shutdown.clone()));
            if let Some((_, other_shutdown)) = duplicate {
                // Two links to the same node: keep the one opened by the node with the lower ID
                let initiator_is_lower = if is_outbound { self.node_id < node_id } else { node_id < self.node_id };
                if !initiator_is_lower {
                    return false;
                }
                other_shutdown.notify_one();
            }
            if let Some(peer) = peers.get_mut(&peer_addr) {
                peer.node_id = Some(node_id);
                peer.listen_addr = listen_addr;
            }
        }
        if let Some(addr) = listen_addr {
            self.remember_addr(addr);
        }
        println!("[+] Peer connected: {} ({})", peer_addr, if is_outbound { "outbound" } else { "inbound" });

        // Learn about other nodes, and fetch whatever this peer has that we lack
        let _ = sender.try_send(Message::GetAddr);
        if archival {
            let _ = sender.try_send(Message::GetBlocksAfter { locator, cursor: None });
        } else {
            // A pruned peer cannot serve history; it can still tell us the current tips
            let _ = sender.try_send(Message::GetTips);
        }
        true
    }

    fn remember_addr(&self, addr: SocketAddr) {
        let mut known = self.known_addrs.lock().unwrap();
        if known.len() < MAX_KNOWN_ADDRS {
            known.insert(addr);
        }
    }

    /// Raises a peer's misbehaviour score; at the threshold the peer is dropped and its IP banned.
    fn misbehave(&self, peer_addr: SocketAddr, points: u32) {
        let mut peers = self.peers.lock().unwrap();
        if let Some(peer) = peers.get_mut(&peer_addr) {
            let faded = (peer.scored_at.elapsed().as_secs() / SCORE_FADE.as_secs()) as u32;
            peer.score = peer.score.saturating_sub(faded) + points;
            peer.scored_at = Instant::now();
            if peer.score >= BAN_THRESHOLD {
                self.banned.lock().unwrap().insert(peer_addr.ip(), Instant::now() + BAN_DURATION);
                peer.shutdown.notify_one();
            }
        }
    }

    fn is_orphan(&self, hash: &Hash) -> bool {
        self.orphans.lock().unwrap().contains_key(hash)
    }

    fn mark_synced_if_caught_up(&self) {
        if self.orphans.lock().unwrap().is_empty() {
            self.synced.store(true, Ordering::SeqCst);
        }
    }

    /// Holds a block until its parents arrive.
    fn park_orphan(&self, block: Block) {
        let mut orphans = self.orphans.lock().unwrap();
        if orphans.len() < MAX_ORPHAN_BLOCKS {
            orphans.insert(block.hash(), block);
        }
    }

    /// Adds a block whose parents are known, then any waiting blocks it unblocks.
    /// Returns the hash of each block added, and the misbehaviour points the given block itself
    /// earns its sender (zero when it was acceptable).
    fn connect_block_and_orphans(&self, ledger: &mut DagLedger, block: Block) -> (Vec<Hash>, u32) {
        let mut connected = Vec::new();
        let mut penalty = 0;
        let mut is_first = true;
        let mut queue = vec![block];
        while let Some(next) = queue.pop() {
            let result = ledger.add_block(next, &self.pow);
            let was_first = std::mem::replace(&mut is_first, false);
            let hash = match result {
                Ok(hash) => hash,
                Err(StateError::BlockAlreadyExists(_)) => continue,
                // A clock disagreement is not misbehaviour; the block is fetched again later
                Err(StateError::TimestampInFuture) => continue,
                // Our own disk failing says nothing about the peer
                Err(StateError::Storage(e)) => {
                    eprintln!("[-] Could not store a block: {}", e);
                    continue;
                }
                Err(e) => {
                    if was_first {
                        penalty = if matches!(e, StateError::InvalidPoW(_)) { PENALTY_INVALID_POW } else { PENALTY_INVALID_BLOCK };
                    }
                    continue;
                }
            };
            connected.push(hash);

            let mut orphans = self.orphans.lock().unwrap();
            let ready: Vec<Hash> = orphans
                .iter()
                .filter(|(_, orphan)| orphan.header.parents.iter().all(|p| ledger.has_block(p)))
                .map(|(orphan_hash, _)| *orphan_hash)
                .collect();
            for orphan_hash in ready {
                if let Some(orphan) = orphans.remove(&orphan_hash) {
                    queue.push(orphan);
                }
            }
        }
        (connected, penalty)
    }

    /// Handles a block from a peer: connects it, or parks it and asks for its missing parents.
    async fn receive_block(&self, peer_addr: SocketAddr, sender: &mpsc::Sender<Message>, block: Block) {
        // The ledger lock is released before anything is queued for sending
        let (missing_parents, connected, penalty) = {
            let mut ledger = self.ledger.write().await;
            if ledger.has_block(&block.hash()) {
                return;
            }
            let missing: Vec<Hash> = block
                .header
                .parents
                .iter()
                .filter(|p| !ledger.has_block(p))
                .copied()
                .collect();
            if missing.is_empty() {
                let (connected, penalty) = self.connect_block_and_orphans(&mut ledger, block);
                (missing, connected, penalty)
            } else {
                self.park_orphan(block);
                (missing, Vec::new(), 0)
            }
        };

        for parent in missing_parents {
            if !self.is_orphan(&parent) {
                let _ = sender.try_send(Message::GetBlock(parent));
            }
        }
        if penalty > 0 {
            self.misbehave(peer_addr, penalty);
        }
        for hash in &connected {
            self.announce(Message::InvBlock(*hash), Some(peer_addr));
        }
        if !connected.is_empty() {
            self.mark_synced_if_caught_up();
        }
    }

    /// Dispatches inbound peer messages after the handshake.
    async fn on_message(&self, peer_addr: SocketAddr, sender: &mpsc::Sender<Message>, message: Message) {
        match message {
            Message::Version { .. } => self.misbehave(peer_addr, 10),
            Message::GetTips => {
                let tips = self.ledger.read().await.tips.iter().copied().collect();
                let _ = sender.try_send(Message::Tips(tips));
            }
            Message::Tips(tips) => {
                // Request any unknown tip blocks
                let unknown: Vec<Hash> = {
                    let ledger = self.ledger.read().await;
                    tips.into_iter().filter(|t| !ledger.has_block(t)).collect()
                };
                if unknown.is_empty() {
                    self.mark_synced_if_caught_up();
                } else if self.orphans.lock().unwrap().len() >= ORPHANS_BEFORE_RESYNC {
                    // Far behind on a link that stayed up: catch up in batches
                    let locator = self.ledger.read().await.locator();
                    let _ = sender.try_send(Message::GetBlocksAfter { locator, cursor: None });
                    return;
                }
                for tip in unknown {
                    if !self.is_orphan(&tip) {
                        let _ = sender.try_send(Message::GetBlock(tip));
                    }
                }
            }
            Message::InvBlock(hash) => {
                let known = self.ledger.read().await.has_block(&hash);
                if !known && !self.is_orphan(&hash) {
                    let _ = sender.try_send(Message::GetBlock(hash));
                }
            }
            Message::GetBlock(hash) => {
                let block = self.ledger.read().await.storage.get_block(&hash).ok().flatten();
                if let Some(block) = block {
                    let _ = sender.try_send(Message::Block(block));
                }
            }
            Message::Block(block) => self.receive_block(peer_addr, sender, block).await,
            Message::GetBlocksAfter { locator, cursor } => {
                let reply = {
                    let ledger = self.ledger.read().await;
                    // Without a cursor, start just above the newest block the peer shares with us
                    let start = cursor.unwrap_or_else(|| (ledger.sync_start_level(&locator), Hash([0xff; 32])));
                    ledger.blocks_after(start, SYNC_BATCH_BLOCKS, SYNC_BATCH_BYTES)
                };
                if let Ok((blocks, next)) = reply {
                    let _ = sender.try_send(Message::BlockBatch { blocks, next });
                }
            }
            Message::BlockBatch { blocks, next } => {
                {
                    // One wait for the disk per batch instead of one per block
                    let _flush_at_end = self.ledger.read().await.storage.defer_flushes();
                    for block in blocks {
                        self.receive_block(peer_addr, sender, block).await;
                    }
                }
                match next {
                    Some(cursor) => {
                        let _ = sender.try_send(Message::GetBlocksAfter { locator: Vec::new(), cursor: Some(cursor) });
                    }
                    None => {
                        // Batches are done: pick up any tips that were not part of them
                        let _ = sender.try_send(Message::GetTips);
                    }
                }
            }
            Message::InvTx(tx_id) => {
                let known = {
                    let ledger = self.ledger.read().await;
                    ledger.mempool.contains(&tx_id) || matches!(ledger.storage.get_tx_record(&tx_id), Ok(Some(_)))
                };
                if !known {
                    let _ = sender.try_send(Message::GetTx(tx_id));
                }
            }
            Message::GetTx(tx_id) => {
                let tx = self.ledger.read().await.mempool.get(&tx_id).cloned();
                if let Some(tx) = tx {
                    let _ = sender.try_send(Message::Tx(tx));
                }
            }
            Message::Tx(tx) => {
                // Duplicate, conflicting or underpaying transactions are simply not relayed
                let admitted = self.ledger.write().await.broadcast_transaction(tx);
                if let Ok(tx_id) = admitted {
                    self.announce(Message::InvTx(tx_id), Some(peer_addr));
                }
            }
            Message::GetAddr => {
                let addrs: Vec<SocketAddr> =
                    self.known_addrs.lock().unwrap().iter().take(MAX_ADDRS_PER_REPLY).copied().collect();
                let _ = sender.try_send(Message::Addr(addrs));
            }
            Message::Addr(addrs) => {
                for addr in addrs.into_iter().take(MAX_ADDRS_PER_REPLY) {
                    self.remember_addr(addr);
                }
            }
            Message::Ping(nonce) => {
                let _ = sender.try_send(Message::Pong(nonce));
            }
            Message::Pong(_) => {}
        }
    }
}
