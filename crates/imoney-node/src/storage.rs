use imoney_core::{Address, BlockHeader, Hash, Outpoint, TxOutput};
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
}

const HEADERS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("headers");
const BLUE_SCORES_TABLE: TableDefinition<&[u8; 32], u64> = TableDefinition::new("blue_scores");
const METADATA_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("metadata");
const UTXO_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("utxos");

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeMeta {
    pub virtual_selected_parent: Hash,
    pub virtual_blue_score: u64,
    pub virtual_daa_score: u64,
    pub difficulty_bits: u32,
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
            let _ = write_tx.open_table(HEADERS_TABLE)?;
            let _ = write_tx.open_table(BLUE_SCORES_TABLE)?;
            let _ = write_tx.open_table(METADATA_TABLE)?;
            let _ = write_tx.open_table(UTXO_TABLE)?;
        }
        write_tx.commit()?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Stores a block header and its GHOSTDAG blue score atomically.
    pub fn save_block(
        &self,
        hash: &Hash,
        header: &BlockHeader,
        blue_score: u64,
        meta: Option<&NodeMeta>,
    ) -> Result<(), StorageError> {
        let write_tx = self.db.begin_write()?;
        {
            let mut headers_table = write_tx.open_table(HEADERS_TABLE)?;
            let header_bytes = serde_json::to_vec(header)?;
            headers_table.insert(&hash.0, header_bytes.as_slice())?;

            let mut blue_table = write_tx.open_table(BLUE_SCORES_TABLE)?;
            blue_table.insert(&hash.0, blue_score)?;

            if let Some(m) = meta {
                let mut meta_table = write_tx.open_table(METADATA_TABLE)?;
                let meta_bytes = serde_json::to_vec(m)?;
                meta_table.insert("node_meta", meta_bytes.as_slice())?;
            }
        }
        write_tx.commit()?;
        Ok(())
    }

    /// Loads all blocks and metadata from disk on node startup.
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

        let headers_table = read_tx.open_table(HEADERS_TABLE)?;
        for entry in headers_table.iter()? {
            let (key, val) = entry?;
            let hash = Hash(*key.value());
            let header: BlockHeader = serde_json::from_slice(val.value())?;
            blocks.insert(hash, header);
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

    /// Adds a new UTXO to the unspent transaction output database.
    pub fn add_utxo(&self, outpoint: &Outpoint, output: &TxOutput) -> Result<(), StorageError> {
        let write_tx = self.db.begin_write()?;
        {
            let mut utxo_table = write_tx.open_table(UTXO_TABLE)?;
            let key_bytes = serde_json::to_vec(outpoint)?;
            let val_bytes = serde_json::to_vec(output)?;
            utxo_table.insert(key_bytes.as_slice(), val_bytes.as_slice())?;
        }
        write_tx.commit()?;
        Ok(())
    }

    /// Queries spendable balance for an address (summing atomic units).
    pub fn get_balance(&self, address: &Address) -> Result<u64, StorageError> {
        let read_tx = self.db.begin_read()?;
        let utxo_table = read_tx.open_table(UTXO_TABLE)?;
        let mut total_atoms: u64 = 0;

        for entry in utxo_table.iter()? {
            let (_, val) = entry?;
            let output: TxOutput = serde_json::from_slice(val.value())?;
            // Output script holds address hash
            if output.script_public_key == address.hash.0 {
                total_atoms += output.value_atoms;
            }
        }

        Ok(total_atoms)
    }

    /// Returns list of all UTXOs belonging to an address.
    pub fn get_utxos(&self, address: &Address) -> Result<Vec<(Outpoint, TxOutput)>, StorageError> {
        let read_tx = self.db.begin_read()?;
        let utxo_table = read_tx.open_table(UTXO_TABLE)?;
        let mut results = Vec::new();

        for entry in utxo_table.iter()? {
            let (k, val) = entry?;
            let outpoint: Outpoint = serde_json::from_slice(k.value())?;
            let output: TxOutput = serde_json::from_slice(val.value())?;
            if output.script_public_key == address.hash.0 {
                results.push((outpoint, output));
            }
        }

        Ok(results)
    }
}
