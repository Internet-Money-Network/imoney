use imoney_core::{Address, Block, BlockHeader, Decode, DecodeError, Encode, Hash, Outpoint, ScriptPublicKey, Transaction, TxOutput};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::HashMap;
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
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Corrupt database record: {0}")]
    Decode(#[from] DecodeError),
}

// Table names carry a schema version: records are in the canonical binary encoding.
const BLOCKS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v1/blocks");
const BLUE_SCORES_TABLE: TableDefinition<&[u8; 32], u64> = TableDefinition::new("v1/blue_scores");
const METADATA_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("v1/metadata");
const UTXO_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("v1/utxos");
const TRANSACTIONS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("v1/transactions");

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeMeta {
    pub virtual_selected_parent: Hash,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    pub difficulty_bits: u32,
}

/// A confirmed transaction together with the block that carried it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTxRecord {
    pub tx: Transaction,
    pub block_hash: Hash,
    pub daa_score: u64,
    pub timestamp_ms: u64,
}

impl Encode for StoredTxRecord {
    fn encode(&self, out: &mut Vec<u8>) {
        self.block_hash.encode(out);
        out.extend_from_slice(&self.daa_score.to_be_bytes());
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        self.tx.encode(out);
    }
}

impl Decode for StoredTxRecord {
    fn decode(reader: &mut imoney_core::serialize::Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            block_hash: reader.hash()?,
            daa_score: reader.u64()?,
            timestamp_ms: reader.u64()?,
            tx: Transaction::decode(reader)?,
        })
    }
}

/// Everything a block changes on disk. Written in a single database transaction.
pub struct BlockUpdate<'a> {
    pub hash: Hash,
    pub block: &'a Block,
    pub blue_score: u64,
    /// New virtual state, when this block became the selected tip.
    pub meta: Option<&'a NodeMeta>,
    pub spent: &'a [Outpoint],
    pub created: &'a [(Outpoint, TxOutput)],
    pub records: &'a [StoredTxRecord],
}

/// Persistent embedded ACID database for Internet Money.
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
            let _ = write_tx.open_table(BLUE_SCORES_TABLE)?;
            let _ = write_tx.open_table(METADATA_TABLE)?;
            let _ = write_tx.open_table(UTXO_TABLE)?;
            let _ = write_tx.open_table(TRANSACTIONS_TABLE)?;
        }
        write_tx.commit()?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Stores a block and applies its UTXO changes atomically: after a crash either all of it
    /// is on disk or none of it is.
    pub fn apply_block(&self, update: &BlockUpdate<'_>) -> Result<(), StorageError> {
        let write_tx = self.db.begin_write()?;
        {
            let mut blocks_table = write_tx.open_table(BLOCKS_TABLE)?;
            blocks_table.insert(&update.hash.0, update.block.to_bytes().as_slice())?;

            let mut blue_table = write_tx.open_table(BLUE_SCORES_TABLE)?;
            blue_table.insert(&update.hash.0, update.blue_score)?;

            if let Some(m) = update.meta {
                let mut meta_table = write_tx.open_table(METADATA_TABLE)?;
                let meta_bytes = serde_json::to_vec(m)?;
                meta_table.insert("node_meta", meta_bytes.as_slice())?;
            }

            let mut utxo_table = write_tx.open_table(UTXO_TABLE)?;
            for outpoint in update.spent {
                utxo_table.remove(outpoint.to_bytes().as_slice())?;
            }
            for (outpoint, output) in update.created {
                utxo_table.insert(outpoint.to_bytes().as_slice(), output.to_bytes().as_slice())?;
            }

            let mut tx_table = write_tx.open_table(TRANSACTIONS_TABLE)?;
            for record in update.records {
                tx_table.insert(&record.tx.id().0, record.to_bytes().as_slice())?;
            }
        }
        write_tx.commit()?;
        Ok(())
    }

    /// Loads all block headers and metadata from disk on node startup.
    pub fn load_state(
        &self,
    ) -> Result<
        (
            HashMap<Hash, BlockHeader>,
            HashMap<Hash, u64>,
            Option<NodeMeta>,
        ),
        StorageError,
    > {
        let read_tx = self.db.begin_read()?;
        let mut blocks = HashMap::new();
        let mut blue_scores = HashMap::new();

        let blocks_table = read_tx.open_table(BLOCKS_TABLE)?;
        for entry in blocks_table.iter()? {
            let (key, val) = entry?;
            let hash = Hash(*key.value());
            let block = Block::from_bytes(val.value())?;
            blocks.insert(hash, block.header);
        }

        let blue_table = read_tx.open_table(BLUE_SCORES_TABLE)?;
        for entry in blue_table.iter()? {
            let (key, val) = entry?;
            let hash = Hash(*key.value());
            blue_scores.insert(hash, val.value());
        }

        let mut meta = None;
        let meta_table = read_tx.open_table(METADATA_TABLE)?;
        if let Some(val) = meta_table.get("node_meta")? {
            let m: NodeMeta = serde_json::from_slice(val.value())?;
            meta = Some(m);
        }

        Ok((blocks, blue_scores, meta))
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

    /// Returns a specific UTXO if it exists.
    pub fn get_utxo(&self, outpoint: &Outpoint) -> Result<Option<TxOutput>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let utxo_table = read_tx.open_table(UTXO_TABLE)?;
        match utxo_table.get(outpoint.to_bytes().as_slice())? {
            Some(val) => Ok(Some(TxOutput::from_bytes(val.value())?)),
            None => Ok(None),
        }
    }

    /// Queries spendable balance for an address (summing atomic units).
    pub fn get_balance(&self, address: &Address) -> Result<u64, StorageError> {
        Ok(self
            .get_utxos(address)?
            .iter()
            .fold(0u64, |total, (_, output)| total.saturating_add(output.value_atoms)))
    }

    /// Returns list of all UTXOs belonging to an address.
    pub fn get_utxos(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput)>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let utxo_table = read_tx.open_table(UTXO_TABLE)?;
        let script = ScriptPublicKey::pay_to_address(address);
        let mut results = Vec::new();

        for entry in utxo_table.iter()? {
            let (k, val) = entry?;
            let output = TxOutput::from_bytes(val.value())?;
            if output.script_public_key == script {
                results.push((Outpoint::from_bytes(k.value())?, output));
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
            total += TxOutput::from_bytes(val.value())?.value_atoms as u128;
        }
        Ok(total)
    }

    /// Queries a confirmed transaction record by its transaction ID.
    pub fn get_transaction(&self, tx_id: &Hash) -> Result<Option<StoredTxRecord>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let tx_table = read_tx.open_table(TRANSACTIONS_TABLE)?;
        match tx_table.get(&tx_id.0)? {
            Some(val) => Ok(Some(StoredTxRecord::from_bytes(val.value())?)),
            None => Ok(None),
        }
    }
}
