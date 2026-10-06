use crate::hash::Hash;
use serde::{Deserialize, Serialize};

/// Reference to a transaction output.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Outpoint {
    pub transaction_id: Hash,
    pub index: u32,
}

/// Transaction input spending a previous output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxInput {
    pub previous_outpoint: Outpoint,
    pub signature_script: Vec<u8>,
    pub sequence: u64,
}

/// Transaction output creating a new UTXO.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxOutput {
    pub value_atoms: u64,
    pub script_public_key: Vec<u8>,
}

/// UTXO-based Internet Money transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub version: u16,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub lock_time: u64,
    pub subnetwork_id: [u8; 20],
    pub gas: u64,
    pub payload: Vec<u8>,
}

impl Transaction {
    /// Computes the unique ID (hash) of the transaction.
    pub fn id(&self) -> Hash {
        let serialized = serde_json::to_vec(self).unwrap_or_default();
        let hash = blake3::hash(&serialized);
        Hash(*hash.as_bytes())
    }
}
