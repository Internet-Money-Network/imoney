use imoney_consensus::GhostdagData;
use imoney_core::serialize::{put_bytes, Reader};
use imoney_core::{Address, Block, BlockHeader, Decode, DecodeError, Encode, Hash, Outpoint, ScriptPublicKey, TxOutput};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("Redb storage error: {0}")]
    RedbStorage(#[from] redb::StorageError),
    #[error("Storage commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("Corrupt database record: {0}")]
    Decode(#[from] DecodeError),
    #[error("Block {0} is missing from the database")]
    MissingBlock(Hash),
}

// Table names carry a schema version: records are in the canonical binary encoding. Blocks and
// acceptance data, which make up most of the database, are stored packed (see `pack`).
const BLOCKS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v4/blocks");
const BLOCK_META_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v4/block_meta");
const ACCEPTANCE_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v4/acceptance");
const METADATA_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("v4/metadata");
const UTXO_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("v4/utxos");
const TRANSACTIONS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v4/transactions");
/// Index of unspent outputs by locking script: key is `script key ++ outpoint`, with no value.
const SCRIPT_UTXO_TABLE: TableDefinition<&[u8], ()> = TableDefinition::new("v4/script_utxos");
/// Index of accepted payments by invoice: key is `invoice id ++ 0x00 ++ transaction id`.
const INVOICE_TABLE: TableDefinition<&[u8], ()> = TableDefinition::new("v4/invoices");
/// What each locking script received and sent, oldest first: key is
/// `script key ++ accepting blue score ++ transaction id`.
const HISTORY_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("v4/script_history");
/// Every block by `level ++ hash`, with no value: an order in which parents come before
/// children. It is how a node finds its most recent blocks at startup without reading the
/// whole chain, and how it pages through the chain for a peer that is syncing.
const LEVEL_TABLE: TableDefinition<&[u8], ()> = TableDefinition::new("v4/levels");

fn level_key(level: u64, hash: &Hash) -> [u8; 40] {
    let mut key = [0u8; 40];
    key[..8].copy_from_slice(&level.to_be_bytes());
    key[8..].copy_from_slice(&hash.0);
    key
}

fn parse_level_key(key: &[u8]) -> Result<(u64, Hash), DecodeError> {
    let mut reader = Reader::new(key);
    Ok((reader.u64()?, reader.hash()?))
}

/// Bytes of a history key after the script: blue score and transaction id.
const HISTORY_POSITION_LEN: usize = 40;

/// zstd level for stored records. Higher levels gain under one percent on blocks.
const PACK_LEVEL: i32 = 3;
/// Largest record `unpack` will inflate; far above any block or acceptance record.
const MAX_UNPACKED_BYTES: usize = 256 * 1024 * 1024;
const PACK_RAW: u8 = 0;
const PACK_ZSTD: u8 = 1;

/// Prepares a record for disk: compressed when that makes it smaller, as it does for blocks
/// carrying payments (about 40% smaller), and left as it is otherwise (an empty block is
/// mostly hashes, which do not compress).
fn pack(bytes: &[u8]) -> Vec<u8> {
    if let Ok(compressed) = zstd::bulk::compress(bytes, PACK_LEVEL) {
        if compressed.len() + 4 < bytes.len() {
            let mut out = Vec::with_capacity(5 + compressed.len());
            out.push(PACK_ZSTD);
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(&compressed);
            return out;
        }
    }
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push(PACK_RAW);
    out.extend_from_slice(bytes);
    out
}

/// Reverses `pack`.
fn unpack(stored: &[u8]) -> Result<Vec<u8>, DecodeError> {
    match stored.split_first() {
        Some((&PACK_RAW, bytes)) => Ok(bytes.to_vec()),
        Some((&PACK_ZSTD, rest)) if rest.len() >= 4 => {
            let len = u32::from_be_bytes(rest[..4].try_into().expect("4 bytes")) as usize;
            if len > MAX_UNPACKED_BYTES {
                return Err(DecodeError::Invalid("packed record length"));
            }
            let bytes = zstd::bulk::decompress(&rest[4..], len).map_err(|_| DecodeError::Invalid("packed record"))?;
            if bytes.len() != len {
                return Err(DecodeError::Invalid("packed record length"));
            }
            Ok(bytes)
        }
        _ => Err(DecodeError::Invalid("packed record")),
    }
}

/// Index key prefix shared by every output locked to `script`.
/// The script length is part of the prefix so one script can never be a prefix of another.
fn script_index_prefix(script: &ScriptPublicKey) -> Vec<u8> {
    let mut key = Vec::with_capacity(3 + script.script.len() + 36);
    key.push(script.version);
    key.extend_from_slice(&(script.script.len() as u16).to_be_bytes());
    key.extend_from_slice(&script.script);
    key
}

fn script_index_key(script: &ScriptPublicKey, outpoint: &Outpoint) -> Vec<u8> {
    let mut key = script_index_prefix(script);
    outpoint.encode(&mut key);
    key
}

fn invoice_index_key(invoice_id: &str, tx_id: &Hash) -> Vec<u8> {
    let mut key = invoice_id.as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(&tx_id.0);
    key
}

fn history_key(script: &ScriptPublicKey, blue_score: u64, id: &Hash) -> Vec<u8> {
    let mut key = script_index_prefix(script);
    key.extend_from_slice(&blue_score.to_be_bytes());
    key.extend_from_slice(&id.0);
    key
}

/// Metadata key: hash of the tip the selected chain ends in.
pub const META_SINK: &str = "sink";
/// Metadata key: running total of every unspent output, in atoms (u128, big-endian).
const META_SUPPLY: &str = "supply_atoms";
/// Metadata key: selected-chain blocks at or below this blue score have been pruned.
pub const META_PRUNED_FLOOR: &str = "pruned_floor";
/// Metadata key: the acceptance data of the virtual block.
pub const META_VIRTUAL_ACCEPTANCE: &str = "virtual_acceptance";

/// An unspent output together with what is needed to decide when it may be spent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtxoEntry {
    pub output: TxOutput,
    /// Blue score of the block that accepted the creating transaction.
    pub blue_score: u64,
    /// Block rewards may only be spent after the coinbase maturity period.
    pub is_coinbase: bool,
}

impl Encode for UtxoEntry {
    fn encode(&self, out: &mut Vec<u8>) {
        self.output.encode(out);
        out.extend_from_slice(&self.blue_score.to_be_bytes());
        out.push(self.is_coinbase as u8);
    }
}

impl Decode for UtxoEntry {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            output: TxOutput::decode(reader)?,
            blue_score: reader.u64()?,
            is_coinbase: match reader.u8()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::Invalid("coinbase flag")),
            },
        })
    }
}

/// Where an accepted transaction lives and when it was accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxRecord {
    /// The block that carries the transaction.
    pub block_hash: Hash,
    pub tx_index: u32,
    /// Blue score of the block that accepted it into the ledger.
    pub accepting_blue_score: u64,
}

impl Encode for TxRecord {
    fn encode(&self, out: &mut Vec<u8>) {
        self.block_hash.encode(out);
        out.extend_from_slice(&self.tx_index.to_be_bytes());
        out.extend_from_slice(&self.accepting_blue_score.to_be_bytes());
    }
}

impl Decode for TxRecord {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            block_hash: reader.hash()?,
            tx_index: reader.u32()?,
            accepting_blue_score: reader.u64()?,
        })
    }
}

/// A transaction accepted by a block, identified by where it is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedTx {
    pub tx_id: Hash,
    pub block_hash: Hash,
    pub tx_index: u32,
    /// The invoice the transaction settles, if it names one.
    pub invoice_id: Option<String>,
}

impl Encode for AcceptedTx {
    fn encode(&self, out: &mut Vec<u8>) {
        self.tx_id.encode(out);
        self.block_hash.encode(out);
        out.extend_from_slice(&self.tx_index.to_be_bytes());
        put_bytes(out, self.invoice_id.as_deref().unwrap_or("").as_bytes());
    }
}

impl Decode for AcceptedTx {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            tx_id: reader.hash()?,
            block_hash: reader.hash()?,
            tx_index: reader.u32()?,
            invoice_id: {
                let bytes = reader.bytes(64)?;
                let id = String::from_utf8(bytes).map_err(|_| DecodeError::Invalid("invoice id"))?;
                (!id.is_empty()).then_some(id)
            },
        })
    }
}

/// What one accepted transaction, or one block reward, did to the coins of one locking script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryItem {
    pub script: ScriptPublicKey,
    /// The transaction, or for a reward the transaction id of the reward's outpoint.
    pub id: Hash,
    pub received_atoms: u64,
    pub sent_atoms: u64,
    pub is_reward: bool,
    /// Timestamp of the block that carries the transaction or earned the reward.
    pub timestamp_ms: u64,
}

impl HistoryItem {
    fn value_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(25);
        out.extend_from_slice(&self.received_atoms.to_be_bytes());
        out.extend_from_slice(&self.sent_atoms.to_be_bytes());
        out.push(self.is_reward as u8);
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        out
    }
}

impl Encode for HistoryItem {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.script.version);
        put_bytes(out, &self.script.script);
        self.id.encode(out);
        out.extend_from_slice(&self.value_bytes());
    }
}

impl Decode for HistoryItem {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            script: ScriptPublicKey { version: reader.u8()?, script: reader.bytes(u16::MAX as usize)? },
            id: reader.hash()?,
            received_atoms: reader.u64()?,
            sent_atoms: reader.u64()?,
            is_reward: reader.u8()? == 1,
            timestamp_ms: reader.u64()?,
        })
    }
}

/// One line of an address's history, as read back from the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    /// Blue score of the block that accepted the transaction into the ledger.
    pub accepting_blue_score: u64,
    pub id: Hash,
    pub received_atoms: u64,
    pub sent_atoms: u64,
    pub is_reward: bool,
    pub timestamp_ms: u64,
}

impl HistoryRow {
    /// Where this row sits in the history; pass it back to continue reading after it.
    pub fn position(&self) -> Vec<u8> {
        let mut position = self.accepting_blue_score.to_be_bytes().to_vec();
        position.extend_from_slice(&self.id.0);
        position
    }
}

/// The exact ledger change made when a block accepts its mergeset. Storing it lets the
/// change be undone and re-applied when the selected chain changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AcceptanceData {
    /// Blue score of the accepting block.
    pub blue_score: u64,
    pub spent: Vec<(Outpoint, UtxoEntry)>,
    pub created: Vec<(Outpoint, UtxoEntry)>,
    pub accepted: Vec<AcceptedTx>,
    /// What the change did to each locking script involved, for the address history index.
    pub history: Vec<HistoryItem>,
}

fn put_utxo_list(out: &mut Vec<u8>, entries: &[(Outpoint, UtxoEntry)]) {
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (outpoint, entry) in entries {
        outpoint.encode(out);
        entry.encode(out);
    }
}

fn read_utxo_list(reader: &mut Reader<'_>) -> Result<Vec<(Outpoint, UtxoEntry)>, DecodeError> {
    let count = reader.len(u32::MAX as usize)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        entries.push((Outpoint::decode(reader)?, UtxoEntry::decode(reader)?));
    }
    Ok(entries)
}

impl Encode for AcceptanceData {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.blue_score.to_be_bytes());
        put_utxo_list(out, &self.spent);
        put_utxo_list(out, &self.created);
        imoney_core::serialize::put_list(out, &self.accepted);
        imoney_core::serialize::put_list(out, &self.history);
    }
}

impl Decode for AcceptanceData {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            blue_score: reader.u64()?,
            spent: read_utxo_list(reader)?,
            created: read_utxo_list(reader)?,
            accepted: reader.list(u32::MAX as usize)?,
            // Records written before the history index existed end here
            history: if reader.remaining() == 0 { Vec::new() } else { reader.list(u32::MAX as usize)? },
        })
    }
}

/// Consensus data stored next to each block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockMeta {
    pub level: u64,
    pub ghostdag: GhostdagData,
}

impl Encode for BlockMeta {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.level.to_be_bytes());
        put_bytes(out, &self.ghostdag.to_bytes());
    }
}

impl Decode for BlockMeta {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            level: reader.u64()?,
            ghostdag: GhostdagData::from_bytes(&reader.bytes(u32::MAX as usize)?)?,
        })
    }
}

/// A set of changes written to disk in one database transaction.
#[derive(Default)]
pub struct WriteBatch {
    pub blocks: Vec<(Hash, Vec<u8>)>,
    pub block_meta: Vec<(Hash, Vec<u8>)>,
    /// New blocks' places in the level order.
    pub levels: Vec<(u64, Hash)>,
    pub acceptance: Vec<(Hash, Vec<u8>)>,
    pub acceptance_deletes: Vec<Hash>,
    pub metadata: Vec<(&'static str, Vec<u8>)>,
    pub utxo_puts: HashMap<Outpoint, UtxoEntry>,
    pub utxo_deletes: HashSet<Outpoint>,
    pub record_puts: HashMap<Hash, TxRecord>,
    pub record_deletes: HashSet<Hash>,
    pub invoice_puts: HashSet<(String, Hash)>,
    pub invoice_deletes: HashSet<(String, Hash)>,
    /// History index keys with their values.
    pub history_puts: HashMap<Vec<u8>, Vec<u8>>,
    pub history_deletes: HashSet<Vec<u8>>,
}

impl WriteBatch {
    pub fn put_history(&mut self, blue_score: u64, item: &HistoryItem) {
        self.history_puts.insert(history_key(&item.script, blue_score, &item.id), item.value_bytes());
    }

    pub fn delete_history(&mut self, blue_score: u64, item: &HistoryItem) {
        let key = history_key(&item.script, blue_score, &item.id);
        self.history_puts.remove(&key);
        self.history_deletes.insert(key);
    }
}

/// Persistent embedded ACID database for Internet Money.
#[derive(Clone)]
pub struct Storage {
    db: Arc<Database>,
}

impl Storage {
    /// Opens or creates the on-disk database at the specified path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let db = Database::create(path.as_ref())?;

        // Ensure initial tables exist
        let write_tx = db.begin_write()?;
        {
            let _ = write_tx.open_table(BLOCKS_TABLE)?;
            let _ = write_tx.open_table(BLOCK_META_TABLE)?;
            let _ = write_tx.open_table(ACCEPTANCE_TABLE)?;
            let _ = write_tx.open_table(METADATA_TABLE)?;
            let _ = write_tx.open_table(UTXO_TABLE)?;
            let _ = write_tx.open_table(TRANSACTIONS_TABLE)?;
            let _ = write_tx.open_table(SCRIPT_UTXO_TABLE)?;
            let _ = write_tx.open_table(INVOICE_TABLE)?;
            let _ = write_tx.open_table(HISTORY_TABLE)?;
            let _ = write_tx.open_table(LEVEL_TABLE)?;
        }
        write_tx.commit()?;

        let storage = Self { db: Arc::new(db) };
        storage.build_level_index_if_missing()?;
        // A database from before the running total existed: count once and remember
        if storage.get_metadata(META_SUPPLY)?.is_none() {
            let total = storage.scan_utxo_atoms()?;
            let write_tx = storage.db.begin_write()?;
            {
                let mut metadata_table = write_tx.open_table(METADATA_TABLE)?;
                metadata_table.insert(META_SUPPLY, total.to_be_bytes().as_slice())?;
            }
            write_tx.commit()?;
        }
        Ok(storage)
    }

    /// Writes a batch atomically: after a crash either all of it is on disk or none of it is.
    pub fn commit(&self, batch: &WriteBatch) -> Result<(), StorageError> {
        let write_tx = self.db.begin_write()?;
        {
            let mut blocks_table = write_tx.open_table(BLOCKS_TABLE)?;
            for (hash, bytes) in &batch.blocks {
                blocks_table.insert(&hash.0, pack(bytes).as_slice())?;
            }

            let mut meta_table = write_tx.open_table(BLOCK_META_TABLE)?;
            for (hash, bytes) in &batch.block_meta {
                meta_table.insert(&hash.0, bytes.as_slice())?;
            }

            let mut level_table = write_tx.open_table(LEVEL_TABLE)?;
            for (level, hash) in &batch.levels {
                level_table.insert(level_key(*level, hash).as_slice(), ())?;
            }

            let mut acceptance_table = write_tx.open_table(ACCEPTANCE_TABLE)?;
            for (hash, bytes) in &batch.acceptance {
                acceptance_table.insert(&hash.0, pack(bytes).as_slice())?;
            }
            for hash in &batch.acceptance_deletes {
                acceptance_table.remove(&hash.0)?;
            }

            let mut metadata_table = write_tx.open_table(METADATA_TABLE)?;
            for (key, bytes) in &batch.metadata {
                metadata_table.insert(*key, bytes.as_slice())?;
            }

            // The supply total moves by exactly what this batch adds to and removes from the UTXO set
            let mut added: u128 = 0;
            let mut removed_total: u128 = 0;
            let mut utxo_table = write_tx.open_table(UTXO_TABLE)?;
            let mut script_table = write_tx.open_table(SCRIPT_UTXO_TABLE)?;
            for outpoint in &batch.utxo_deletes {
                let removed = match utxo_table.remove(outpoint.to_bytes().as_slice())? {
                    Some(old) => Some(UtxoEntry::from_bytes(old.value())?),
                    None => None,
                };
                if let Some(entry) = removed {
                    removed_total += entry.output.value_atoms as u128;
                    script_table.remove(script_index_key(&entry.output.script_public_key, outpoint).as_slice())?;
                }
            }
            for (outpoint, entry) in &batch.utxo_puts {
                let replaced = match utxo_table.insert(outpoint.to_bytes().as_slice(), entry.to_bytes().as_slice())? {
                    Some(old) => Some(UtxoEntry::from_bytes(old.value())?),
                    None => None,
                };
                if let Some(old) = replaced {
                    removed_total += old.output.value_atoms as u128;
                    script_table.remove(script_index_key(&old.output.script_public_key, outpoint).as_slice())?;
                }
                added += entry.output.value_atoms as u128;
                script_table.insert(script_index_key(&entry.output.script_public_key, outpoint).as_slice(), ())?;
            }
            if added != removed_total {
                let current = match metadata_table.get(META_SUPPLY)? {
                    Some(bytes) => bytes.value().try_into().map(u128::from_be_bytes).unwrap_or(0),
                    None => 0,
                };
                let updated = (current + added).saturating_sub(removed_total);
                metadata_table.insert(META_SUPPLY, updated.to_be_bytes().as_slice())?;
            }

            let mut tx_table = write_tx.open_table(TRANSACTIONS_TABLE)?;
            for tx_id in &batch.record_deletes {
                tx_table.remove(&tx_id.0)?;
            }
            for (tx_id, record) in &batch.record_puts {
                tx_table.insert(&tx_id.0, record.to_bytes().as_slice())?;
            }

            let mut invoice_table = write_tx.open_table(INVOICE_TABLE)?;
            for (invoice_id, tx_id) in &batch.invoice_deletes {
                invoice_table.remove(invoice_index_key(invoice_id, tx_id).as_slice())?;
            }
            for (invoice_id, tx_id) in &batch.invoice_puts {
                invoice_table.insert(invoice_index_key(invoice_id, tx_id).as_slice(), ())?;
            }

            let mut history_table = write_tx.open_table(HISTORY_TABLE)?;
            for key in &batch.history_deletes {
                history_table.remove(key.as_slice())?;
            }
            for (key, value) in &batch.history_puts {
                history_table.insert(key.as_slice(), value.as_slice())?;
            }
        }
        write_tx.commit()?;
        Ok(())
    }

    /// A database written before the level index existed gets one, once.
    fn build_level_index_if_missing(&self) -> Result<(), StorageError> {
        let write_tx = self.db.begin_write()?;
        {
            let mut level_table = write_tx.open_table(LEVEL_TABLE)?;
            if level_table.first()?.is_none() {
                let meta_table = write_tx.open_table(BLOCK_META_TABLE)?;
                for entry in meta_table.iter()? {
                    let (key, value) = entry?;
                    let level = BlockMeta::from_bytes(value.value())?.level;
                    level_table.insert(level_key(level, &Hash(*key.value())).as_slice(), ())?;
                }
            }
        }
        write_tx.commit()?;
        Ok(())
    }

    /// Removes the level index, leaving a database as older versions wrote it.
    #[cfg(test)]
    pub fn drop_level_index(&self) -> Result<(), StorageError> {
        let write_tx = self.db.begin_write()?;
        write_tx.delete_table(LEVEL_TABLE)?;
        write_tx.commit()?;
        Ok(())
    }

    /// How many blocks the database holds.
    pub fn block_count(&self) -> Result<u64, StorageError> {
        let read_tx = self.db.begin_read()?;
        Ok(redb::ReadableTableMetadata::len(&read_tx.open_table(LEVEL_TABLE)?)?)
    }

    /// The highest level any stored block has.
    pub fn top_level(&self) -> Result<Option<u64>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let table = read_tx.open_table(LEVEL_TABLE)?;
        let top = match table.last()? {
            Some((key, _)) => Some(parse_level_key(key.value())?.0),
            None => None,
        };
        Ok(top)
    }

    /// Loads the headers and consensus data of every block at `min_level` or above. A node
    /// starting up needs only its recent blocks in memory.
    pub fn load_blocks_from_level(&self, min_level: u64) -> Result<Vec<(Hash, BlockHeader, BlockMeta)>, StorageError> {
        let start = level_key(min_level, &Hash::ZERO);
        let read_tx = self.db.begin_read()?;
        let level_table = read_tx.open_table(LEVEL_TABLE)?;
        let blocks_table = read_tx.open_table(BLOCKS_TABLE)?;
        let meta_table = read_tx.open_table(BLOCK_META_TABLE)?;
        let mut loaded = Vec::new();
        for entry in level_table.range::<&[u8]>(start.as_slice()..)? {
            let (_, hash) = parse_level_key(entry?.0.value())?;
            let (Some(block), Some(meta)) = (blocks_table.get(&hash.0)?, meta_table.get(&hash.0)?) else {
                return Err(StorageError::MissingBlock(hash));
            };
            let header = Block::from_bytes(&unpack(block.value())?)?.header;
            loaded.push((hash, header, BlockMeta::from_bytes(meta.value())?));
        }
        Ok(loaded)
    }

    /// Up to `limit` positions after `cursor` in `(level, hash)` order.
    pub fn levels_after(&self, cursor: (u64, Hash), limit: usize) -> Result<Vec<(u64, Hash)>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let table = read_tx.open_table(LEVEL_TABLE)?;
        let start = level_key(cursor.0, &cursor.1);
        let mut positions = Vec::new();
        for entry in table.range::<&[u8]>((std::ops::Bound::Excluded(start.as_slice()), std::ops::Bound::Unbounded))? {
            positions.push(parse_level_key(entry?.0.value())?);
            if positions.len() >= limit {
                break;
            }
        }
        Ok(positions)
    }

    /// True when the block is stored, in full or as a pruned header.
    pub fn has_block(&self, hash: &Hash) -> Result<bool, StorageError> {
        let read_tx = self.db.begin_read()?;
        Ok(read_tx.open_table(BLOCK_META_TABLE)?.get(&hash.0)?.is_some())
    }

    /// A stored block's header and consensus data, whether or not its body was pruned.
    pub fn get_header_and_meta(&self, hash: &Hash) -> Result<Option<(BlockHeader, BlockMeta)>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let blocks_table = read_tx.open_table(BLOCKS_TABLE)?;
        let meta_table = read_tx.open_table(BLOCK_META_TABLE)?;
        let Some(block) = blocks_table.get(&hash.0)? else { return Ok(None) };
        let header = Block::from_bytes(&unpack(block.value())?)?.header;
        match meta_table.get(&hash.0)? {
            Some(meta) => Ok(Some((header, BlockMeta::from_bytes(meta.value())?))),
            None => Err(StorageError::MissingBlock(*hash)),
        }
    }

    /// Returns a full block (header and transactions) by its hash. A block whose transactions
    /// have been pruned is reported as absent: every real block has at least a coinbase.
    pub fn get_block(&self, hash: &Hash) -> Result<Option<Block>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let blocks_table = read_tx.open_table(BLOCKS_TABLE)?;
        match blocks_table.get(&hash.0)? {
            Some(val) => {
                let block = Block::from_bytes(&unpack(val.value())?)?;
                Ok((!block.transactions.is_empty()).then_some(block))
            }
            None => Ok(None),
        }
    }

    /// Bytes a block takes in the database and bytes of the block itself.
    pub fn block_stored_size(&self, hash: &Hash) -> Result<Option<(usize, usize)>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let blocks_table = read_tx.open_table(BLOCKS_TABLE)?;
        match blocks_table.get(&hash.0)? {
            Some(val) => Ok(Some((val.value().len(), unpack(val.value())?.len()))),
            None => Ok(None),
        }
    }

    /// Returns the stored acceptance data of a selected-chain block.
    pub fn get_acceptance(&self, hash: &Hash) -> Result<Option<AcceptanceData>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let table = read_tx.open_table(ACCEPTANCE_TABLE)?;
        match table.get(&hash.0)? {
            Some(val) => Ok(Some(AcceptanceData::from_bytes(&unpack(val.value())?)?)),
            None => Ok(None),
        }
    }

    pub fn get_metadata(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let table = read_tx.open_table(METADATA_TABLE)?;
        Ok(table.get(key)?.map(|val| val.value().to_vec()))
    }

    /// Returns a specific UTXO if it exists.
    pub fn get_utxo(&self, outpoint: &Outpoint) -> Result<Option<UtxoEntry>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let utxo_table = read_tx.open_table(UTXO_TABLE)?;
        match utxo_table.get(outpoint.to_bytes().as_slice())? {
            Some(val) => Ok(Some(UtxoEntry::from_bytes(val.value())?)),
            None => Ok(None),
        }
    }

    /// Queries the balance of an address (summing atomic units), including immature rewards.
    pub fn get_balance(&self, address: &Address) -> Result<u64, StorageError> {
        Ok(self
            .get_utxos(address)?
            .iter()
            .fold(0u64, |total, (_, entry)| total.saturating_add(entry.output.value_atoms)))
    }

    /// Returns list of all UTXOs belonging to an address, found through the script index.
    pub fn get_utxos(&self, address: &Address) -> Result<Vec<(Outpoint, UtxoEntry)>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let script_table = read_tx.open_table(SCRIPT_UTXO_TABLE)?;
        let utxo_table = read_tx.open_table(UTXO_TABLE)?;

        let prefix = script_index_prefix(&ScriptPublicKey::pay_to_address(address));
        let mut end = prefix.clone();
        end.extend_from_slice(&[0xff; 36]);

        let mut results = Vec::new();
        for item in script_table.range::<&[u8]>(prefix.as_slice()..=end.as_slice())? {
            let (key, _) = item?;
            let outpoint_bytes = &key.value()[prefix.len()..];
            if let Some(val) = utxo_table.get(outpoint_bytes)? {
                results.push((Outpoint::from_bytes(outpoint_bytes)?, UtxoEntry::from_bytes(val.value())?));
            }
        }

        Ok(results)
    }

    /// IDs of the accepted transactions that name `invoice_id`.
    pub fn get_invoice_tx_ids(&self, invoice_id: &str) -> Result<Vec<Hash>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let table = read_tx.open_table(INVOICE_TABLE)?;
        let mut start = invoice_id.as_bytes().to_vec();
        start.push(0);
        let mut end = start.clone();
        end.extend_from_slice(&[0xff; 32]);

        let mut tx_ids = Vec::new();
        for item in table.range::<&[u8]>(start.as_slice()..=end.as_slice())? {
            let (key, _) = item?;
            tx_ids.push(<Hash as Decode>::from_bytes(&key.value()[start.len()..])?);
        }
        Ok(tx_ids)
    }

    /// The newest `limit` history rows of an address, newest first. `before` is the position of
    /// a row already read: only older rows are returned.
    pub fn get_history(
        &self,
        address: &Address,
        limit: usize,
        before: Option<&[u8]>,
    ) -> Result<Vec<HistoryRow>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let table = read_tx.open_table(HISTORY_TABLE)?;
        let prefix = script_index_prefix(&ScriptPublicKey::pay_to_address(address));
        let mut end = prefix.clone();
        match before {
            Some(position) => end.extend_from_slice(position),
            // One byte longer than any real key, so it sorts after all of them
            None => end.extend_from_slice(&[0xff; HISTORY_POSITION_LEN + 1]),
        }

        let mut rows = Vec::new();
        for item in table.range::<&[u8]>(prefix.as_slice()..end.as_slice())?.rev().take(limit) {
            let (key, value) = item?;
            let mut position = Reader::new(&key.value()[prefix.len()..]);
            let mut fields = Reader::new(value.value());
            rows.push(HistoryRow {
                accepting_blue_score: position.u64()?,
                id: position.hash()?,
                received_atoms: fields.u64()?,
                sent_atoms: fields.u64()?,
                is_reward: fields.u8()? == 1,
                timestamp_ms: fields.u64()?,
            });
        }
        Ok(rows)
    }

    /// The circulating supply in atoms: the sum of every unspent output, kept as a running total
    /// that each write updates, so reading it does not depend on how many coins exist.
    pub fn total_utxo_atoms(&self) -> Result<u128, StorageError> {
        Ok(self
            .get_metadata(META_SUPPLY)?
            .and_then(|bytes| bytes.try_into().ok())
            .map(u128::from_be_bytes)
            .unwrap_or(0))
    }

    /// Adds up every unspent output by reading the whole UTXO set. Slow on a large chain; used
    /// to seed the running total and to check it.
    pub fn scan_utxo_atoms(&self) -> Result<u128, StorageError> {
        let read_tx = self.db.begin_read()?;
        let utxo_table = read_tx.open_table(UTXO_TABLE)?;
        let mut total: u128 = 0;
        for entry in utxo_table.iter()? {
            let (_, val) = entry?;
            total += UtxoEntry::from_bytes(val.value())?.output.value_atoms as u128;
        }
        Ok(total)
    }

    /// Queries the record of an accepted transaction by its transaction ID.
    pub fn get_tx_record(&self, tx_id: &Hash) -> Result<Option<TxRecord>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let tx_table = read_tx.open_table(TRANSACTIONS_TABLE)?;
        match tx_table.get(&tx_id.0)? {
            Some(val) => Ok(Some(TxRecord::from_bytes(val.value())?)),
            None => Ok(None),
        }
    }
}
