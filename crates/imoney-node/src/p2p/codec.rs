//! Wire format: every message travels in a frame of `magic (4) ++ length (u32) ++ payload`,
//! where the payload is a one-byte message tag followed by the canonical binary encoding.

use imoney_core::serialize::{put_bytes, put_list, Reader};
use imoney_core::{Block, Decode, DecodeError, Encode, Hash, Transaction};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bumped whenever the wire format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 2;

/// Largest frame payload accepted from a peer.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Most hashes or addresses accepted in one list.
const MAX_LIST_ITEMS: usize = 2_000;

/// A position in the `(level, hash)` ordering of blocks, used to page through a peer's DAG.
pub type SyncCursor = (u64, Hash);

/// Wire messages exchanged across P2P TCP connections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// First message on every connection, in both directions.
    Version {
        protocol: u32,
        network: u8,
        genesis: Hash,
        /// Random per-node value used to detect self-connections and duplicate links.
        node_id: u64,
        listen_port: u16,
        user_agent: String,
    },
    /// Request current tips from peer
    GetTips,
    /// Response containing DAG tip hashes
    Tips(Vec<Hash>),
    /// Request a specific block by hash
    GetBlock(Hash),
    /// A full block (header and transactions)
    Block(Block),
    /// Announcement that the sender has a block
    InvBlock(Hash),
    /// Announcement that the sender has a pending transaction
    InvTx(Hash),
    /// Request a pending transaction by ID
    GetTx(Hash),
    /// A pending transaction
    Tx(Transaction),
    /// Request the next batch of blocks. With no cursor, the responder starts just above the
    /// first block in `locator` that it knows.
    GetBlocksAfter { locator: Vec<Hash>, cursor: Option<SyncCursor> },
    /// A batch of blocks in an order where parents come before children.
    /// `next` is the cursor for the following batch, or `None` when there is nothing more.
    BlockBatch { blocks: Vec<Block>, next: Option<SyncCursor> },
    /// Request addresses of other nodes
    GetAddr,
    /// Addresses of other nodes that accept connections
    Addr(Vec<SocketAddr>),
    /// Ping heartbeat
    Ping(u64),
    /// Pong heartbeat reply
    Pong(u64),
}

fn put_cursor(out: &mut Vec<u8>, cursor: &Option<SyncCursor>) {
    match cursor {
        None => out.push(0),
        Some((level, hash)) => {
            out.push(1);
            out.extend_from_slice(&level.to_be_bytes());
            hash.encode(out);
        }
    }
}

fn read_cursor(reader: &mut Reader<'_>) -> Result<Option<SyncCursor>, DecodeError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some((reader.u64()?, reader.hash()?))),
        _ => Err(DecodeError::Invalid("cursor flag")),
    }
}

impl Encode for Message {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Message::Version { protocol, network, genesis, node_id, listen_port, user_agent } => {
                out.push(0);
                out.extend_from_slice(&protocol.to_be_bytes());
                out.push(*network);
                genesis.encode(out);
                out.extend_from_slice(&node_id.to_be_bytes());
                out.extend_from_slice(&listen_port.to_be_bytes());
                put_bytes(out, user_agent.as_bytes());
            }
            Message::GetTips => out.push(1),
            Message::Tips(hashes) => {
                out.push(2);
                put_list(out, hashes);
            }
            Message::GetBlock(hash) => {
                out.push(3);
                hash.encode(out);
            }
            Message::Block(block) => {
                out.push(4);
                block.encode(out);
            }
            Message::InvBlock(hash) => {
                out.push(5);
                hash.encode(out);
            }
            Message::InvTx(hash) => {
                out.push(6);
                hash.encode(out);
            }
            Message::GetTx(hash) => {
                out.push(7);
                hash.encode(out);
            }
            Message::Tx(tx) => {
                out.push(8);
                tx.encode(out);
            }
            Message::GetBlocksAfter { locator, cursor } => {
                out.push(9);
                put_list(out, locator);
                put_cursor(out, cursor);
            }
            Message::BlockBatch { blocks, next } => {
                out.push(10);
                put_list(out, blocks);
                put_cursor(out, next);
            }
            Message::GetAddr => out.push(11),
            Message::Addr(addrs) => {
                out.push(12);
                out.extend_from_slice(&(addrs.len() as u32).to_be_bytes());
                for addr in addrs {
                    // IPv4 addresses travel in their IPv6-mapped form
                    let ip = match addr.ip() {
                        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
                        IpAddr::V6(v6) => v6,
                    };
                    out.extend_from_slice(&ip.octets());
                    out.extend_from_slice(&addr.port().to_be_bytes());
                }
            }
            Message::Ping(nonce) => {
                out.push(13);
                out.extend_from_slice(&nonce.to_be_bytes());
            }
            Message::Pong(nonce) => {
                out.push(14);
                out.extend_from_slice(&nonce.to_be_bytes());
            }
        }
    }
}

impl Decode for Message {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(match reader.u8()? {
            0 => Message::Version {
                protocol: reader.u32()?,
                network: reader.u8()?,
                genesis: reader.hash()?,
                node_id: reader.u64()?,
                listen_port: reader.u16()?,
                user_agent: String::from_utf8(reader.bytes(256)?).map_err(|_| DecodeError::Invalid("user agent"))?,
            },
            1 => Message::GetTips,
            2 => Message::Tips(reader.list(MAX_LIST_ITEMS)?),
            3 => Message::GetBlock(reader.hash()?),
            4 => Message::Block(Block::decode(reader)?),
            5 => Message::InvBlock(reader.hash()?),
            6 => Message::InvTx(reader.hash()?),
            7 => Message::GetTx(reader.hash()?),
            8 => Message::Tx(Transaction::decode(reader)?),
            9 => Message::GetBlocksAfter { locator: reader.list(MAX_LIST_ITEMS)?, cursor: read_cursor(reader)? },
            10 => Message::BlockBatch { blocks: reader.list(MAX_LIST_ITEMS)?, next: read_cursor(reader)? },
            11 => Message::GetAddr,
            12 => {
                let count = reader.len(MAX_LIST_ITEMS)?;
                let mut addrs = Vec::new();
                for _ in 0..count {
                    let octets: [u8; 16] = reader.take(16)?.try_into().unwrap();
                    let ip = Ipv6Addr::from(octets);
                    let ip = ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(ip));
                    addrs.push(SocketAddr::new(ip, reader.u16()?));
                }
                Message::Addr(addrs)
            }
            13 => Message::Ping(reader.u64()?),
            14 => Message::Pong(reader.u64()?),
            _ => return Err(DecodeError::Invalid("message tag")),
        })
    }
}

fn invalid(reason: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, reason)
}

/// Writes one framed message.
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, magic: [u8; 4], message: &Message) -> std::io::Result<()> {
    let payload = message.to_bytes();
    if payload.len() > MAX_FRAME_BYTES {
        return Err(invalid("outgoing message too large"));
    }
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&magic);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    writer.write_all(&frame).await
}

/// Reads framed messages. Bytes received so far are kept between calls, so a pending
/// `read` may be dropped (for example inside `select!`) without losing data.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// Returns the next message, or `None` when the peer closed the connection cleanly.
    /// A wrong network magic, an oversized frame or an undecodable payload is an error.
    pub async fn read<R: AsyncRead + Unpin>(&mut self, reader: &mut R, magic: [u8; 4]) -> std::io::Result<Option<Message>> {
        loop {
            if self.buf.len() >= 8 {
                if self.buf[..4] != magic {
                    return Err(invalid("wrong network magic"));
                }
                let len = u32::from_be_bytes(self.buf[4..8].try_into().unwrap()) as usize;
                if len > MAX_FRAME_BYTES {
                    return Err(invalid("frame too large"));
                }
                if self.buf.len() >= 8 + len {
                    let message = Message::from_bytes(&self.buf[8..8 + len]).map_err(|_| invalid("malformed message"))?;
                    self.buf.drain(..8 + len);
                    return Ok(Some(message));
                }
            }
            if reader.read_buf(&mut self.buf).await? == 0 {
                return if self.buf.is_empty() { Ok(None) } else { Err(invalid("connection closed mid-frame")) };
            }
        }
    }
}
