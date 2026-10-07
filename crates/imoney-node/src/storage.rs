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

// Table names carry a schema version: records are in the canonical binary encoding.
const BLOCKS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v3/blocks");
const BLOCK_META_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v3/block_meta");
const ACCEPTANCE_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v3/acceptance");
const METADATA_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("v3/metadata");
const UTXO_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("v3/utxos");
const TRANSACTIONS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v3/transactions");
/// Index of unspent outputs by locking script: key is `script key ++ outpoint`, with no value.
const SCRIPT_UTXO_TABLE: TableDefinition<&[u8], ()> = TableDefinition::new("v3/script_utxos");

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

/// Metadata key: hash of the tip the selected chain ends in.
pub const META_SINK: &str = "sink";
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
}

impl Encode for AcceptedTx {
    fn encode(&self, out: &mut Vec<u8>) {
        self.tx_id.encode(out);
        self.block_hash.encode(out);
        out.extend_from_slice(&self.tx_index.to_be_bytes());
    }
}

impl Decode for AcceptedTx {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            tx_id: reader.hash()?,
            block_hash: reader.hash()?,
            tx_index: reader.u32()?,
        })
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
    }
}

impl Decode for AcceptanceData {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            blue_score: reader.u64()?,
            spent: read_utxo_list(reader)?,
            created: read_utxo_list(reader)?,
            accepted: reader.list(u32::MAX as usize)?,
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
    pub acceptance: Vec<(Hash, Vec<u8>)>,
    pub metadata: Vec<(&'static str, Vec<u8>)>,
    pub utxo_puts: HashMap<Outpoint, UtxoEntry>,
    pub utxo_deletes: HashSet<Outpoint>,
    pub record_puts: HashMap<Hash, TxRecord>,
    pub record_deletes: HashSet<Hash>,
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
        }
        write_tx.commit()?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Writes a batch atomically: after a crash either all of it is on disk or none of it is.
    pub fn commit(&self, batch: &WriteBatch) -> Result<(), StorageError> {
        let write_tx = self.db.begin_write()?;
        {
            let mut blocks_table = write_tx.open_table(BLOCKS_TABLE)?;
            for (hash, bytes) in &batch.blocks {
                blocks_table.insert(&hash.0, bytes.as_slice())?;
            }

            let mut meta_table = write_tx.open_table(BLOCK_META_TABLE)?;
            for (hash, bytes) in &batch.block_meta {
                meta_table.insert(&hash.0, bytes.as_slice())?;
            }

            let mut acceptance_table = write_tx.open_table(ACCEPTANCE_TABLE)?;
            for (hash, bytes) in &batch.acceptance {
                acceptance_table.insert(&hash.0, bytes.as_slice())?;
            }

            let mut metadata_table = write_tx.open_table(METADATA_TABLE)?;
            for (key, bytes) in &batch.metadata {
                metadata_table.insert(*key, bytes.as_slice())?;
            }

            let mut utxo_table = write_tx.open_table(UTXO_TABLE)?;
            let mut script_table = write_tx.open_table(SCRIPT_UTXO_TABLE)?;
            for outpoint in &batch.utxo_deletes {
                let removed = match utxo_table.remove(outpoint.to_bytes().as_slice())? {
                    Some(old) => Some(UtxoEntry::from_bytes(old.value())?),
                    None => None,
                };
                if let Some(entry) = removed {
                    script_table.remove(script_index_key(&entry.output.script_public_key, outpoint).as_slice())?;
                }
            }
            for (outpoint, entry) in &batch.utxo_puts {
                utxo_table.insert(outpoint.to_bytes().as_slice(), entry.to_bytes().as_slice())?;
                script_table.insert(script_index_key(&entry.output.script_public_key, outpoint).as_slice(), ())?;
            }

            let mut tx_table = write_tx.open_table(TRANSACTIONS_TABLE)?;
            for tx_id in &batch.record_deletes {
                tx_table.remove(&tx_id.0)?;
            }
            for (tx_id, record) in &batch.record_puts {
                tx_table.insert(&tx_id.0, record.to_bytes().as_slice())?;
            }
        }
        write_tx.commit()?;
        Ok(())
    }

    /// Loads every block header with its consensus data on node startup.
    pub fn load_blocks(&self) -> Result<Vec<(Hash, BlockHeader, BlockMeta)>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let blocks_table = read_tx.open_table(BLOCKS_TABLE)?;
        let meta_table = read_tx.open_table(BLOCK_META_TABLE)?;
        let mut loaded = Vec::new();

        for entry in blocks_table.iter()? {
            let (key, val) = entry?;
            let hash = Hash(*key.value());
            let block = Block::from_bytes(val.value())?;
            let meta = match meta_table.get(&hash.0)? {
                Some(bytes) => BlockMeta::from_bytes(bytes.value())?,
                None => return Err(StorageError::MissingBlock(hash)),
            };
            loaded.push((hash, block.header, meta));
        }
        Ok(loaded)
    }

    /// Returns a full block (header and transactions) by its hash.
    pub fn get_block(&self, hash: &Hash) -> Result<Option<Block>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let blocks_table = read_tx.open_table(BLOCKS_TABLE)?;
        match blocks_table.get(&hash.0)? {
            Some(val) => Ok(Some(Block::from_bytes(val.value())?)),
            None => Ok(None),
        }
    }

    /// Returns the stored acceptance data of a selected-chain block.
    pub fn get_acceptance(&self, hash: &Hash) -> Result<Option<AcceptanceData>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let table = read_tx.open_table(ACCEPTANCE_TABLE)?;
        match table.get(&hash.0)? {
            Some(val) => Ok(Some(AcceptanceData::from_bytes(val.value())?)),
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

    /// Sum of every unspent output: the circulating supply in atoms.
    pub fn total_utxo_atoms(&self) -> Result<u128, StorageError> {
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
