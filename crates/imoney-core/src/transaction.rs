use crate::address::{Address, AddressType, Network};
use crate::constants::MAX_TX_BYTES;
use crate::hash::Hash;
use crate::serialize::{put_bytes, put_list, tagged_hash, Decode, DecodeError, Encode, Reader};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const TX_ID_CONTEXT: &str = "IMN 2026 transaction id";
const TX_HASH_CONTEXT: &str = "IMN 2026 transaction hash";
const SIG_HASH_CONTEXT: &str = "IMN 2026 signature hash";

/// Script version 0: the script is the 32-byte Blake3 hash of an Ed25519 public key.
pub const SCRIPT_VERSION_PUBKEY_HASH: u8 = AddressType::PubKeyHash as u8;

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
    #[error("Unsupported script version: {0}")]
    UnsupportedScriptVersion(u8),
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

/// Versioned locking script of an output.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScriptPublicKey {
    pub version: u8,
    pub script: Vec<u8>,
}

impl ScriptPublicKey {
    /// The script that pays to `address`.
    pub fn pay_to_address(address: &Address) -> Self {
        Self {
            version: address.address_type as u8,
            script: address.hash.0.to_vec(),
        }
    }
}

/// Transaction output creating a new UTXO.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxOutput {
    pub value_atoms: u64,
    pub script_public_key: ScriptPublicKey,
}

/// UTXO-based Internet Money transaction. A transaction without inputs is a coinbase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub version: u16,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub lock_time: u64,
    pub subnetwork_id: [u8; 20],
    pub gas: u64,
    pub payload: Vec<u8>,
    /// Optional script of the node that served this payment. When set, the ledger pays it half
    /// of the transaction fee; the rest goes to the miner. Covered by the signature.
    pub service: Option<ScriptPublicKey>,
}

impl Transaction {
    /// Builds a coinbase transaction. The payload starts with the block's DAA score.
    pub fn coinbase(daa_score: u64, outputs: Vec<TxOutput>, extra_data: &[u8]) -> Self {
        let mut payload = daa_score.to_be_bytes().to_vec();
        payload.extend_from_slice(extra_data);
        Self {
            version: 1,
            inputs: Vec::new(),
            outputs,
            lock_time: 0,
            subnetwork_id: [0u8; 20],
            gas: 0,
            payload,
            service: None,
        }
    }

    pub fn is_coinbase(&self) -> bool {
        self.inputs.is_empty()
    }

    fn encode_with(&self, out: &mut Vec<u8>, include_signatures: bool) {
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&(self.inputs.len() as u32).to_be_bytes());
        for input in &self.inputs {
            input.previous_outpoint.encode(out);
            if include_signatures {
                put_bytes(out, &input.signature_script);
            } else {
                put_bytes(out, &[]);
            }
            out.extend_from_slice(&input.sequence.to_be_bytes());
        }
        put_list(out, &self.outputs);
        out.extend_from_slice(&self.lock_time.to_be_bytes());
        out.extend_from_slice(&self.subnetwork_id);
        out.extend_from_slice(&self.gas.to_be_bytes());
        put_bytes(out, &self.payload);
        match &self.service {
            None => out.push(0),
            Some(script) => {
                out.push(1);
                out.push(script.version);
                put_bytes(out, &script.script);
            }
        }
    }

    /// The share of `fee` paid to the service script: half, rounded down, when one is named.
    pub fn service_share(&self, fee: u64) -> u64 {
        match self.service {
            Some(_) => fee / 2,
            None => 0,
        }
    }

    /// Where the ledger puts the service share: one past the transaction's own outputs.
    pub fn service_outpoint(&self) -> Outpoint {
        Outpoint { transaction_id: self.id(), index: self.outputs.len() as u32 }
    }

    fn unsigned_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_with(&mut out, false);
        out
    }

    /// The transaction ID referenced by outpoints. It excludes signature scripts,
    /// so re-signing a transaction cannot change its ID.
    pub fn id(&self) -> Hash {
        tagged_hash(TX_ID_CONTEXT, &[&self.unsigned_bytes()])
    }

    /// Hash of the full transaction including signatures; committed to by the block merkle root.
    pub fn hash(&self) -> Hash {
        tagged_hash(TX_HASH_CONTEXT, &[&self.to_bytes()])
    }

    /// Computes the signing hash for an input. It commits to the network, so a signature
    /// made on one network is not valid on another.
    pub fn sig_hash(&self, network: Network, input_index: usize) -> Hash {
        tagged_hash(
            SIG_HASH_CONTEXT,
            &[&[network.id()], &self.unsigned_bytes(), &(input_index as u32).to_be_bytes()],
        )
    }

    /// Signs an input using the specified private key.
    pub fn sign_input(
        &mut self,
        network: Network,
        input_index: usize,
        signing_key: &SigningKey,
    ) -> Result<(), TransactionError> {
        if input_index >= self.inputs.len() {
            return Err(TransactionError::EmptyInputs);
        }

        let sighash = self.sig_hash(network, input_index);
        let signature = signing_key.sign(sighash.as_bytes());
        let verifying_key = signing_key.verifying_key();

        // Format signature script: [32-byte verifying key] ++ [64-byte signature] = 96 bytes
        let mut script = Vec::with_capacity(96);
        script.extend_from_slice(verifying_key.as_bytes());
        script.extend_from_slice(&signature.to_bytes());

        self.inputs[input_index].signature_script = script;
        Ok(())
    }

    /// Verifies the signature of an input against the script of the UTXO it spends.
    pub fn verify_input(
        &self,
        network: Network,
        input_index: usize,
        spent_script: &ScriptPublicKey,
    ) -> Result<(), TransactionError> {
        if spent_script.version != SCRIPT_VERSION_PUBKEY_HASH {
            return Err(TransactionError::UnsupportedScriptVersion(spent_script.version));
        }

        let input = self.inputs.get(input_index).ok_or(TransactionError::EmptyInputs)?;
        if input.signature_script.len() != 96 {
            return Err(TransactionError::InvalidSignatureScript(input.signature_script.len()));
        }

        let pubkey_bytes: [u8; 32] = input.signature_script[0..32].try_into().unwrap();
        let sig_bytes: [u8; 64] = input.signature_script[32..96].try_into().unwrap();

        // 1. Verify that public key hashes to the expected address hash
        let computed_addr_hash = blake3::hash(&pubkey_bytes);
        if computed_addr_hash.as_bytes()[..] != spent_script.script[..] {
            return Err(TransactionError::PublicKeyAddressMismatch);
        }

        // 2. Verify signature on sig_hash
        let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes)
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))?;
        let signature = Signature::from_bytes(&sig_bytes);

        let sighash = self.sig_hash(network, input_index);
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
        service: Option<&Address>,
    ) -> Result<Self, String> {
        let verifying_key = signing_key.verifying_key();
        let sender_addr = Address::from_public_key(network, AddressType::PubKeyHash, verifying_key.as_bytes());

        let total_required = amount_atoms
            .checked_add(fee_atoms)
            .ok_or_else(|| "Amount plus fee overflows".to_string())?;
        let mut accumulated: u64 = 0;
        let mut selected_utxos = Vec::new();

        for (outpoint, output) in available_utxos {
            accumulated = accumulated
                .checked_add(output.value_atoms)
                .ok_or_else(|| "Selected inputs overflow".to_string())?;
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
            script_public_key: ScriptPublicKey::pay_to_address(recipient_addr),
        }];

        if change_atoms > 0 {
            outputs.push(TxOutput {
                value_atoms: change_atoms,
                script_public_key: ScriptPublicKey::pay_to_address(&sender_addr),
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
            service: service.map(ScriptPublicKey::pay_to_address),
        };

        for i in 0..tx.inputs.len() {
            tx.sign_input(network, i, signing_key)
                .map_err(|e| format!("Failed to sign input {}: {}", i, e))?;
        }

        Ok(tx)
    }
}

impl Encode for Outpoint {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.transaction_id.0);
        out.extend_from_slice(&self.index.to_be_bytes());
    }
}

impl Decode for Outpoint {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            transaction_id: reader.hash()?,
            index: reader.u32()?,
        })
    }
}

impl Encode for TxInput {
    fn encode(&self, out: &mut Vec<u8>) {
        self.previous_outpoint.encode(out);
        put_bytes(out, &self.signature_script);
        out.extend_from_slice(&self.sequence.to_be_bytes());
    }
}

impl Decode for TxInput {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            previous_outpoint: Outpoint::decode(reader)?,
            signature_script: reader.bytes(MAX_TX_BYTES)?,
            sequence: reader.u64()?,
        })
    }
}

impl Encode for TxOutput {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.value_atoms.to_be_bytes());
        out.push(self.script_public_key.version);
        put_bytes(out, &self.script_public_key.script);
    }
}

impl Decode for TxOutput {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            value_atoms: reader.u64()?,
            script_public_key: ScriptPublicKey {
                version: reader.u8()?,
                script: reader.bytes(MAX_TX_BYTES)?,
            },
        })
    }
}

impl Encode for Transaction {
    fn encode(&self, out: &mut Vec<u8>) {
        self.encode_with(out, true);
    }
}

impl Decode for Transaction {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            version: reader.u16()?,
            inputs: reader.list(MAX_TX_BYTES)?,
            outputs: reader.list(MAX_TX_BYTES)?,
            lock_time: reader.u64()?,
            subnetwork_id: reader.take(20)?.try_into().unwrap(),
            gas: reader.u64()?,
            payload: reader.bytes(MAX_TX_BYTES)?,
            service: match reader.u8()? {
                0 => None,
                1 => Some(ScriptPublicKey { version: reader.u8()?, script: reader.bytes(MAX_TX_BYTES)? }),
                _ => return Err(DecodeError::Invalid("service flag")),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    fn signed_payment() -> (Transaction, Address) {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let sender_addr = Address::from_public_key(
            Network::Testnet,
            AddressType::PubKeyHash,
            signing_key.verifying_key().as_bytes(),
        );

        let outpoint = Outpoint {
            transaction_id: Hash([7u8; 32]),
            index: 0,
        };
        let utxo = TxOutput {
            value_atoms: 10_000_000,
            script_public_key: ScriptPublicKey::pay_to_address(&sender_addr),
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
            None,
        )
        .expect("build_payment failed");
        (tx, sender_addr)
    }

    #[test]
    fn test_transaction_signing_and_verification() {
        let (tx, sender_addr) = signed_payment();

        assert_eq!(tx.inputs.len(), 1);
        assert_eq!(tx.outputs.len(), 2); // 6_000_000 to recipient, 3_999_000 change
        assert_eq!(tx.outputs[0].value_atoms, 6_000_000);
        assert_eq!(tx.outputs[1].value_atoms, 3_999_000);

        // Verify input signature
        let verify_res = tx.verify_input(Network::Testnet, 0, &ScriptPublicKey::pay_to_address(&sender_addr));
        assert!(verify_res.is_ok(), "Signature verification failed: {:?}", verify_res);
    }

    #[test]
    fn signature_does_not_replay_on_another_network() {
        let (tx, sender_addr) = signed_payment();
        let script = ScriptPublicKey::pay_to_address(&sender_addr);

        assert!(tx.verify_input(Network::Testnet, 0, &script).is_ok());
        assert!(matches!(
            tx.verify_input(Network::Mainnet, 0, &script),
            Err(TransactionError::InvalidSignature(_))
        ));
    }

    #[test]
    fn tampering_with_outputs_invalidates_signature() {
        let (mut tx, sender_addr) = signed_payment();
        tx.outputs[0].value_atoms += 1;
        let script = ScriptPublicKey::pay_to_address(&sender_addr);
        assert!(tx.verify_input(Network::Testnet, 0, &script).is_err());
    }

    #[test]
    fn id_ignores_signatures_but_hash_commits_to_them() {
        let (tx, _) = signed_payment();
        let mut resigned = tx.clone();
        resigned.inputs[0].signature_script[95] ^= 1;

        assert_eq!(tx.id(), resigned.id());
        assert_ne!(tx.hash(), resigned.hash());
        assert_ne!(tx.id(), tx.hash());
    }

    #[test]
    fn transaction_round_trips_through_canonical_encoding() {
        let (tx, _) = signed_payment();
        let bytes = tx.to_bytes();
        assert_eq!(Transaction::from_bytes(&bytes).unwrap(), tx);
        assert!(Transaction::from_bytes(&bytes[..bytes.len() - 1]).is_err());

        let coinbase = Transaction::coinbase(42, Vec::new(), b"extra");
        assert!(coinbase.is_coinbase());
        assert_eq!(Transaction::from_bytes(&coinbase.to_bytes()).unwrap(), coinbase);
    }

    #[test]
    fn canonical_encoding_matches_fixed_vector() {
        let tx = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_outpoint: Outpoint { transaction_id: Hash([0xaa; 32]), index: 2 },
                signature_script: vec![0xbb; 3],
                sequence: 5,
            }],
            outputs: vec![TxOutput {
                value_atoms: 0x0102,
                script_public_key: ScriptPublicKey { version: 0, script: vec![0xcc; 2] },
            }],
            lock_time: 0,
            subnetwork_id: [0u8; 20],
            gas: 0,
            payload: vec![0xdd],
            service: Some(ScriptPublicKey { version: 0, script: vec![0xee; 2] }),
        };
        let expected = [
            "0001",                 // version
            "00000001",             // input count
            &"aa".repeat(32),       // previous transaction id
            "00000002",             // previous output index
            "00000003bbbbbb",       // signature script
            "0000000000000005",     // sequence
            "00000001",             // output count
            "0000000000000102",     // value
            "00",                   // script version
            "00000002cccc",         // script
            "0000000000000000",     // lock time
            &"00".repeat(20),       // subnetwork id
            "0000000000000000",     // gas
            "00000001dd",           // payload
            "01",                   // service script present
            "00",                   // service script version
            "00000002eeee",         // service script
        ]
        .concat();
        assert_eq!(hex::encode(tx.to_bytes()), expected);
    }

    #[test]
    fn service_script_is_signed_and_takes_half_the_fee() {
        let (tx, sender_addr) = signed_payment();
        assert_eq!(tx.service_share(1_001), 0);

        let key = SigningKey::from_bytes(&[5u8; 32]);
        let me = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, key.verifying_key().as_bytes());
        let node = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &[9u8; 32]);
        let utxo = TxOutput { value_atoms: 50_000, script_public_key: ScriptPublicKey::pay_to_address(&me) };
        let outpoint = Outpoint { transaction_id: Hash([7u8; 32]), index: 0 };
        let mut with_service =
            Transaction::build_payment(&key, Network::Testnet, &sender_addr, 10_000, 1_001, vec![(outpoint, utxo)], Some(&node))
                .unwrap();

        assert_eq!(with_service.service_share(1_001), 500);
        assert_eq!(with_service.service_outpoint().index, 2);
        assert_eq!(Transaction::from_bytes(&with_service.to_bytes()).unwrap(), with_service);

        // Swapping the service script after signing invalidates the signature
        let script = ScriptPublicKey::pay_to_address(&me);
        assert!(with_service.verify_input(Network::Testnet, 0, &script).is_ok());
        with_service.service = Some(ScriptPublicKey::pay_to_address(&sender_addr));
        assert!(with_service.verify_input(Network::Testnet, 0, &script).is_err());
    }
}
