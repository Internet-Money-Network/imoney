use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum TransactionError {
    #[error("No inputs provided")]
    EmptyInputs,
    #[error("No outputs provided")]
    EmptyOutputs,
    #[error("Signature script missing or invalid length: {0}")]
    InvalidSignatureScript(usize),
    #[error("Public key verification failed: {0}")]
    InvalidSignature(String),
    #[error("Public key does not match address hash")]
    PublicKeyAddressMismatch,
}

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
    /// Encodes: [32-byte Ed25519 VerifyingKey] ++ [64-byte Ed25519 Signature]
    pub signature_script: Vec<u8>,
    pub sequence: u64,
}

/// Transaction output creating a new UTXO.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxOutput {
    pub value_atoms: u64,
    /// Usually 32-byte Blake3 address hash or script payload
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
    /// Computes the unique ID (hash) of the transaction including signatures.
    pub fn id(&self) -> Hash {
        let serialized = serde_json::to_vec(self).unwrap_or_default();
        let hash = blake3::hash(&serialized);
        Hash(*hash.as_bytes())
    }

    /// Computes the signing hash (sighash) for an input.
    /// Zeroes out the signature_scripts of all inputs before hashing to prevent circular dependencies.
    pub fn sig_hash(&self, input_index: usize) -> Hash {
        let mut unsigned = self.clone();
        for input in &mut unsigned.inputs {
            input.signature_script.clear();
        }
        let serialized = serde_json::to_vec(&unsigned).unwrap_or_default();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&serialized);
        hasher.update(&(input_index as u32).to_le_bytes());
        Hash(*hasher.finalize().as_bytes())
    }

    /// Signs an input using the specified private key.
    pub fn sign_input(
        &mut self,
        input_index: usize,
        signing_key: &SigningKey,
    ) -> Result<(), TransactionError> {
        if input_index >= self.inputs.len() {
            return Err(TransactionError::EmptyInputs);
        }

        let sighash = self.sig_hash(input_index);
        let signature = signing_key.sign(sighash.as_bytes());
        let verifying_key = signing_key.verifying_key();

        // Format signature script: [32-byte verifying key] ++ [64-byte signature] = 96 bytes
        let mut script = Vec::with_capacity(96);
        script.extend_from_slice(verifying_key.as_bytes());
        script.extend_from_slice(&signature.to_bytes());

        self.inputs[input_index].signature_script = script;
        Ok(())
    }

    /// Verifies the signature of an input against an expected UTXO script_public_key (address hash).
    pub fn verify_input(
        &self,
        input_index: usize,
        expected_address_hash: &[u8],
    ) -> Result<(), TransactionError> {
        let input = self.inputs.get(input_index).ok_or(TransactionError::EmptyInputs)?;
        if input.signature_script.len() != 96 {
            return Err(TransactionError::InvalidSignatureScript(input.signature_script.len()));
        }

        let pubkey_bytes: [u8; 32] = input.signature_script[0..32].try_into().unwrap();
        let sig_bytes: [u8; 64] = input.signature_script[32..96].try_into().unwrap();

        // 1. Verify that public key hashes to the expected address hash
        let computed_addr_hash = blake3::hash(&pubkey_bytes);
        if computed_addr_hash.as_bytes() != expected_address_hash {
            return Err(TransactionError::PublicKeyAddressMismatch);
        }

        // 2. Verify signature on sig_hash
        let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes)
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))?;
        let signature = Signature::from_bytes(&sig_bytes);

        let sighash = self.sig_hash(input_index);
        verifying_key
            .verify(sighash.as_bytes(), &signature)
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))?;

        Ok(())
    }

    /// Creates a transfer transaction sending `amount_atoms` to `recipient`, returning change to `sender`.
    pub fn build_payment(
        signing_key: &SigningKey,
        network: Network,
        recipient_addr: &Address,
        amount_atoms: u64,
        fee_atoms: u64,
        available_utxos: Vec<(Outpoint, TxOutput)>,
    ) -> Result<Self, String> {
        let verifying_key = signing_key.verifying_key();
        let sender_addr = Address::from_public_key(network, AddressType::PubKeyHash, verifying_key.as_bytes());

        let total_required = amount_atoms + fee_atoms;
        let mut accumulated: u64 = 0;
        let mut selected_utxos = Vec::new();

        for (outpoint, output) in available_utxos {
            accumulated += output.value_atoms;
            selected_utxos.push((outpoint, output));
            if accumulated >= total_required {
                break;
            }
        }

        if accumulated < total_required {
            return Err(format!(
                "Insufficient funds: have {} atoms, require {} atoms",
                accumulated, total_required
            ));
        }

        let change_atoms = accumulated - total_required;

        let inputs: Vec<TxInput> = selected_utxos
            .iter()
            .map(|(outpoint, _)| TxInput {
                previous_outpoint: outpoint.clone(),
                signature_script: Vec::new(),
                sequence: 0,
            })
            .collect();

        let mut outputs = vec![TxOutput {
            value_atoms: amount_atoms,
            script_public_key: recipient_addr.hash.0.to_vec(),
        }];

        if change_atoms > 0 {
            outputs.push(TxOutput {
                value_atoms: change_atoms,
                script_public_key: sender_addr.hash.0.to_vec(),
            });
        }

        let mut tx = Transaction {
            version: 1,
            inputs,
            outputs,
            lock_time: 0,
            subnetwork_id: [0u8; 20],
            gas: 0,
            payload: Vec::new(),
        };

        for i in 0..tx.inputs.len() {
            tx.sign_input(i, signing_key)
                .map_err(|e| format!("Failed to sign input {}: {}", i, e))?;
        }

        Ok(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn test_transaction_signing_and_verification() {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        let sender_addr = Address::from_public_key(
            Network::Testnet,
            AddressType::PubKeyHash,
            verifying_key.as_bytes(),
        );

        let outpoint = Outpoint {
            transaction_id: Hash([7u8; 32]),
            index: 0,
        };
        let utxo = TxOutput {
            value_atoms: 10_000_000,
            script_public_key: sender_addr.hash.0.to_vec(),
        };

        let recipient_key = SigningKey::generate(&mut csprng);
        let recipient_addr = Address::from_public_key(
            Network::Testnet,
            AddressType::PubKeyHash,
            recipient_key.verifying_key().as_bytes(),
        );

        let tx = Transaction::build_payment(
            &signing_key,
            Network::Testnet,
            &recipient_addr,
            6_000_000,
            1_000,
            vec![(outpoint, utxo)],
        )
        .expect("build_payment failed");

        assert_eq!(tx.inputs.len(), 1);
        assert_eq!(tx.outputs.len(), 2); // 6_000_000 to recipient, 3_999_000 change
        assert_eq!(tx.outputs[0].value_atoms, 6_000_000);
        assert_eq!(tx.outputs[1].value_atoms, 3_999_000);

        // Verify input signature
        let verify_res = tx.verify_input(0, &sender_addr.hash.0);
        assert!(verify_res.is_ok(), "Signature verification failed: {:?}", verify_res);
    }
}
