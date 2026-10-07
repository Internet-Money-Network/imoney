use super::codec::{write_frame, FrameReader, Message, MAX_FRAME_BYTES, PROTOCOL_VERSION};
use super::*;
use crate::state::ConsensusParams;
use ed25519_dalek::SigningKey;
use imoney_core::{Address, AddressType, Decode, Encode, Network};
use imoney_pow::{PowMode, PowParams};
use std::sync::atomic::AtomicBool;
use tokio::sync::RwLock;

const MAGIC: [u8; 4] = TESTNET_MAGIC;

fn address_of(key: &SigningKey) -> Address {
    Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, key.verifying_key().as_bytes())
}

struct TestNode {
    manager: Arc<PeerManager>,
    ledger: SharedLedger,
    pow: Arc<MoneyPrinterPow>,
    addr: SocketAddr,
}

impl TestNode {
    async fn start(name: &str) -> Self {
        Self::start_with_magic(name, MAGIC).await
    }

    async fn start_with_magic(name: &str, magic: [u8; 4]) -> Self {
        let path = std::env::temp_dir().join(format!("imoney-p2p-{}-{}.redb", name, std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut params = ConsensusParams::testnet();
        params.coinbase_maturity = 2;
        params.daa.retarget = false;
        let ledger: SharedLedger = Arc::new(RwLock::new(DagLedger::open_with_params(path, None, params).unwrap()));
        let pow = Arc::new(MoneyPrinterPow::new(PowParams::tiny(), Hash([1u8; 32]), PowMode::Full));
        let manager = Arc::new(
            PeerManager::with_magic(ledger.clone(), pow.clone(), 0, magic)
                .with_timing(Duration::from_millis(300), Duration::from_secs(1)),
        );
        let addr = manager.clone().start_server("127.0.0.1:0".parse().unwrap()).await.unwrap();
        Self { manager, ledger, pow, addr }
    }

    fn connect(&self, other: &TestNode) {
        self.manager.clone().connect_to_peer(other.addr);
    }

    /// Mines one block and adds it locally without telling any peer.
    async fn mine_silently(&self, payout: &Address) -> Hash {
        let mut ledger = self.ledger.write().await;
        let template = ledger.get_mining_template(Some(payout));
        let (nonce, _) = self
            .pow
            .mine(&template.pre_pow_hash, template.block.header.bits, 0, 1_000_000, Arc::new(AtomicBool::new(false)))
            .unwrap();
        ledger.add_block(template.into_block(nonce), &self.pow).unwrap()
    }

    /// Mines one block on the current tips, adds it locally and announces it.
    async fn mine(&self, payout: &Address) -> Hash {
        let mut ledger = self.ledger.write().await;
        let template = ledger.get_mining_template(Some(payout));
        let (nonce, _) = self
            .pow
            .mine(&template.pre_pow_hash, template.block.header.bits, 0, 1_000_000, Arc::new(AtomicBool::new(false)))
            .unwrap();
        let block = template.into_block(nonce);
        let hash = ledger.add_block(block.clone(), &self.pow).unwrap();
        drop(ledger);
        self.manager.broadcast_block(block);
        hash
    }

    async fn block_count(&self) -> usize {
        self.ledger.read().await.blocks.len()
    }

    async fn sink(&self) -> Hash {
        self.ledger.read().await.virtual_selected_parent
    }

    async fn peer_count(&self) -> usize {
        self.manager.get_connected_peers().await.len()
    }
}

/// Polls an async condition for up to 20 seconds.
macro_rules! eventually {
    ($what:expr, $condition:expr) => {{
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if $condition {
                break;
            }
            assert!(Instant::now() < deadline, "timed out waiting for: {}", $what);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

#[test]
fn every_message_round_trips() {
    let block = crate::genesis::create_testnet_genesis();
    let tx = block.transactions[0].clone();
    let messages = vec![
        Message::Version {
            protocol: PROTOCOL_VERSION,
            network: 1,
            genesis: block.hash(),
            node_id: 77,
            listen_port: 18555,
            user_agent: "/imoney:0.1.0/".to_string(),
        },
        Message::GetTips,
        Message::Tips(vec![Hash([1u8; 32]), Hash([2u8; 32])]),
        Message::GetBlock(Hash([3u8; 32])),
        Message::Block(block.clone()),
        Message::InvBlock(Hash([4u8; 32])),
        Message::InvTx(Hash([5u8; 32])),
        Message::GetTx(Hash([6u8; 32])),
        Message::Tx(tx),
        Message::GetBlocksAfter { locator: vec![Hash([7u8; 32])], cursor: None },
        Message::GetBlocksAfter { locator: Vec::new(), cursor: Some((9, Hash([8u8; 32]))) },
        Message::BlockBatch { blocks: vec![block.clone(), block], next: Some((1, Hash([9u8; 32]))) },
        Message::GetAddr,
        Message::Addr(vec!["203.0.113.7:18555".parse().unwrap(), "[2001:db8::1]:18555".parse().unwrap()]),
        Message::Ping(1),
        Message::Pong(2),
    ];
    for message in messages {
        assert_eq!(Message::from_bytes(&message.to_bytes()).unwrap(), message);
    }
    assert!(Message::from_bytes(&[200]).is_err());
}

#[tokio::test]
async fn frames_survive_arbitrary_chunking_and_reject_bad_input() {
    let mut wire = Vec::new();
    write_frame(&mut wire, MAGIC, &Message::Ping(42)).await.unwrap();
    write_frame(&mut wire, MAGIC, &Message::Tips(vec![Hash([1u8; 32])])).await.unwrap();

    // Delivered a few bytes at a time
    let (mut client, mut server) = tokio::io::duplex(5);
    let sent = wire.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        client.write_all(&sent).await.unwrap();
    });
    let mut frames = FrameReader::default();
    assert_eq!(frames.read(&mut server, MAGIC).await.unwrap(), Some(Message::Ping(42)));
    assert_eq!(frames.read(&mut server, MAGIC).await.unwrap(), Some(Message::Tips(vec![Hash([1u8; 32])])));
    assert_eq!(frames.read(&mut server, MAGIC).await.unwrap(), None);

    // Another network's magic
    let mut frames = FrameReader::default();
    assert!(frames.read(&mut &wire[..], *b"XXXX").await.is_err());

    // A frame that claims to be larger than the limit is rejected before it is buffered
    let mut oversized = MAGIC.to_vec();
    oversized.extend_from_slice(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes());
    let mut frames = FrameReader::default();
    assert!(frames.read(&mut &oversized[..], MAGIC).await.is_err());

    // Valid frame, undecodable payload
    let mut garbage = MAGIC.to_vec();
    garbage.extend_from_slice(&1u32.to_be_bytes());
    garbage.push(250);
    let mut frames = FrameReader::default();
    assert!(frames.read(&mut &garbage[..], MAGIC).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn late_joiner_syncs_a_long_history() {
    let miner = address_of(&SigningKey::from_bytes(&[1u8; 32]));
    let a = TestNode::start("sync-a").await;
    for _ in 0..450 {
        a.mine(&miner).await;
    }

    // More blocks than one batch or the orphan pool could cover
    let b = TestNode::start("sync-b").await;
    b.manager.require_initial_sync();
    assert!(!b.manager.is_synced());
    b.connect(&a);

    eventually!("B has every block", b.block_count().await == 451);
    eventually!("B reports synced", b.manager.is_synced());
    assert_eq!(b.sink().await, a.sink().await);
    let (ledger_a, ledger_b) = (a.ledger.read().await, b.ledger.read().await);
    assert_eq!(ledger_a.get_balance(&miner).unwrap(), ledger_b.get_balance(&miner).unwrap());
    assert_eq!(ledger_a.storage.total_utxo_atoms().unwrap(), ledger_b.storage.total_utxo_atoms().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_and_transactions_relay_across_nodes_that_are_not_directly_connected() {
    let payer = SigningKey::from_bytes(&[2u8; 32]);
    let other = address_of(&SigningKey::from_bytes(&[3u8; 32]));
    let a = TestNode::start("relay-a").await;
    let b = TestNode::start("relay-b").await;
    let c = TestNode::start("relay-c").await;
    b.connect(&a);
    c.connect(&b);
    eventually!("links are up", a.peer_count().await >= 1 && c.peer_count().await >= 1);

    // Blocks mined on A reach C through B
    a.mine(&address_of(&payer)).await;
    for _ in 0..3 {
        a.mine(&other).await;
    }
    eventually!("C has A's blocks", c.block_count().await == 5);
    assert_eq!(c.sink().await, a.sink().await);

    // A payment submitted on C reaches A's mempool
    let tx_id = {
        let mut ledger = c.ledger.write().await;
        let utxos = ledger.get_spendable_utxos(&address_of(&payer)).unwrap();
        let tx = Transaction::build_payment(&payer, Network::Testnet, &other, 5_000, 1_000, utxos, None).unwrap();
        let tx_id = ledger.broadcast_transaction(tx.clone()).unwrap();
        drop(ledger);
        c.manager.broadcast_transaction(tx);
        tx_id
    };
    eventually!("A has the transaction", a.ledger.read().await.mempool.contains(&tx_id));

    // A mines it; C sees it confirmed
    a.mine(&other).await;
    eventually!(
        "C sees the payment confirmed",
        c.ledger.read().await.get_transaction(&tx_id).unwrap().is_some_and(|info| info.block_hash.is_some())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nodes_discover_each_other_through_a_shared_peer() {
    let a = TestNode::start("addr-a").await;
    let b = TestNode::start("addr-b").await;
    let c = TestNode::start("addr-c").await;
    b.connect(&a);
    eventually!("A and B are linked", a.peer_count().await == 1);

    // C is only told about A, and learns B's address from it
    c.connect(&a);
    eventually!("C is linked to both", c.peer_count().await == 2);
    eventually!("B is linked to both", b.peer_count().await == 2);

    // No duplicate links form between any pair
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(a.peer_count().await, 2);
    assert_eq!(b.peer_count().await, 2);
    assert_eq!(c.peer_count().await, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn nodes_on_another_network_are_refused_and_banned() {
    let a = TestNode::start("net-a").await;
    let stranger = TestNode::start_with_magic("net-stranger", *b"XXXX").await;
    stranger.connect(&a);

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(a.peer_count().await, 0);
    assert_eq!(stranger.peer_count().await, 0);
    assert!(a.manager.is_banned(&stranger.addr.ip()));
}

#[tokio::test(flavor = "multi_thread")]
async fn peer_sending_invalid_blocks_is_banned() {
    let a = TestNode::start("ban-a").await;
    let miner = address_of(&SigningKey::from_bytes(&[4u8; 32]));

    // A hand-driven peer: completes the handshake, then sends blocks with a forged blue score
    let mut stream = TcpStream::connect(a.addr).await.unwrap();
    let genesis = a.ledger.read().await.genesis_hash;
    let version = Message::Version {
        protocol: PROTOCOL_VERSION,
        network: Network::Testnet.id(),
        genesis,
        node_id: 12345,
        listen_port: 0,
        user_agent: "/test/".to_string(),
    };
    write_frame(&mut stream, MAGIC, &version).await.unwrap();
    eventually!("handshake completes", a.peer_count().await == 1);

    for nonce in 0..5u64 {
        let mut block = a.ledger.read().await.get_mining_template(Some(&miner)).block;
        block.header.blue_score += 1 + nonce;
        let pre_pow_hash = block.header.pre_pow_hash().unwrap();
        let (found, _) = a
            .pow
            .mine(&pre_pow_hash, block.header.bits, 0, 1_000_000, Arc::new(AtomicBool::new(false)))
            .unwrap();
        block.header.nonce = found;
        write_frame(&mut stream, MAGIC, &Message::Block(block)).await.unwrap();
    }

    eventually!("the peer is dropped", a.peer_count().await == 0);
    assert!(a.manager.is_banned(&"127.0.0.1".parse().unwrap()));
    assert_eq!(a.block_count().await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_that_were_never_announced_are_found_by_tip_polling() {
    let miner = address_of(&SigningKey::from_bytes(&[5u8; 32]));
    let a = TestNode::start("poll-a").await;
    let b = TestNode::start("poll-b").await;
    b.connect(&a);
    eventually!("link is up", a.peer_count().await == 1 && b.peer_count().await == 1);

    // As if every announcement had been lost on the way
    for _ in 0..6 {
        a.mine_silently(&miner).await;
    }
    eventually!("B catches up anyway", b.block_count().await == 7);
    assert_eq!(b.sink().await, a.sink().await);

    // And in the other direction
    for _ in 0..3 {
        b.mine_silently(&miner).await;
    }
    eventually!("A catches up too", a.block_count().await == 10);
    assert_eq!(a.sink().await, b.sink().await);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_isolated_chains_converge_when_the_nodes_meet() {
    let (miner_a, miner_b) = (address_of(&SigningKey::from_bytes(&[6u8; 32])), address_of(&SigningKey::from_bytes(&[7u8; 32])));
    let a = TestNode::start("split-a").await;
    let b = TestNode::start("split-b").await;

    // Each node mines alone: far more blocks than one block could ever merge
    for _ in 0..120 {
        a.mine_silently(&miner_a).await;
    }
    for _ in 0..150 {
        b.mine_silently(&miner_b).await;
    }
    assert_ne!(a.sink().await, b.sink().await);

    b.connect(&a);
    eventually!("both nodes hold both chains", a.block_count().await == 271 && b.block_count().await == 271);
    // Both follow the heavier chain and agree on every balance
    assert_eq!(a.sink().await, b.sink().await);
    let (ledger_a, ledger_b) = (a.ledger.read().await, b.ledger.read().await);
    for miner in [&miner_a, &miner_b] {
        assert_eq!(ledger_a.get_balance(miner).unwrap(), ledger_b.get_balance(miner).unwrap());
    }
    assert_eq!(ledger_a.storage.total_utxo_atoms().unwrap(), ledger_b.storage.total_utxo_atoms().unwrap());
    assert!(ledger_a.get_balance(&miner_b).unwrap().0 > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn node_stops_waiting_for_sync_when_no_peer_is_reachable() {
    let a = TestNode::start("lonely").await;
    a.manager.require_initial_sync();
    // Nothing listens here
    a.manager.clone().connect_to_peer("127.0.0.1:9".parse().unwrap());
    assert!(!a.manager.is_synced());
    eventually!("the node gives up waiting", a.manager.is_synced());
}

#[tokio::test(flavor = "multi_thread")]
async fn learned_peer_addresses_survive_a_restart() {
    let path = std::env::temp_dir().join(format!("imoney-peers-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let learned: SocketAddr = "203.0.113.9:18555".parse().unwrap();

    let first = TestNode::start("store-1").await;
    first.manager.use_peer_store(path.clone());
    first.manager.remember_addr(learned);
    first.manager.save_peers();

    let second = TestNode::start("store-2").await;
    assert!(!second.manager.known_addrs.lock().unwrap().contains(&learned));
    second.manager.use_peer_store(path);
    assert!(second.manager.known_addrs.lock().unwrap().contains(&learned));
}
