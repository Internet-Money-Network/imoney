//! Quantum-safe addresses: coins that need both an Ed25519 signature and a SQIsign signature to
//! move.
//!
//! The address (type `PostQuantumHash`) is the hash of two public keys: an ordinary Ed25519 key
//! and a SQIsign key (round 3, level I, through the `sqisign-rs` crate). Spending reveals both
//! keys and signs the ordinary signing hash with each. Both must be valid, so the coins stay safe
//! while either scheme holds: Ed25519 if SQIsign, which is young, turns out to be weak; SQIsign if
//! a quantum computer breaks Ed25519.
//!
//! The signature script of a spend is
//! `Ed25519 key (32) ++ SQIsign key (83) ++ Ed25519 signature (64) ++ SQIsign signature (200)`,
//! 379 bytes. Checking the SQIsign signature costs a few milliseconds, so it is done last: the
//! hash and the Ed25519 signature are checked first and refuse a forgery cheaply.
//!
//! SQIsign signatures are not strongly unforgeable: anyone can turn a valid one into another
//! valid one. That changes the transaction's hash but not its ID, which leaves signature scripts
//! out, so it cannot be used to hide or replace a payment.

use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use crate::serialize::tagged_hash;
use crate::transaction::{Transaction, TransactionError};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use sqisign_rs::Level1;

/// Script version 1: the script is the hash of an Ed25519 key and a SQIsign key.
pub const SCRIPT_VERSION_QUANTUM_SAFE: u8 = AddressType::PostQuantumHash as u8;
/// Bytes of a SQIsign (level I) public key.
pub const SQISIGN_PUBLIC_KEY_BYTES: usize = 83;
/// Bytes of a SQIsign (level I) signature.
pub const SQISIGN_SIGNATURE_BYTES: usize = 200;
/// Most quantum-safe coins one relayed payment may spend; nodes refuse more (relay policy).
pub const MAX_RELAYED_INPUTS: usize = 16;
/// Bytes of the signature script of a quantum-safe spend.
pub const QUANTUM_SAFE_WITNESS_BYTES: usize = 32 + SQISIGN_PUBLIC_KEY_BYTES + 64 + SQISIGN_SIGNATURE_BYTES;

const KEY_HASH_CONTEXT: &str = "IMN 2026 quantum-safe keys";
const ED25519_SEED_CONTEXT: &str = "IMN 2026 quantum-safe Ed25519 key";
const SQISIGN_SEED_CONTEXT: &str = "IMN 2026 quantum-safe SQIsign key";
const SQISIGN_NONCE_CONTEXT: &str = "IMN 2026 quantum-safe SQIsign signing";

/// The two public keys behind a quantum-safe address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuantumSafePublicKey {
    pub ed25519: [u8; 32],
    pub sqisign: [u8; SQISIGN_PUBLIC_KEY_BYTES],
}

impl QuantumSafePublicKey {
    /// The hash an output is locked to.
    pub fn key_hash(&self) -> Hash {
        tagged_hash(KEY_HASH_CONTEXT, &[&self.ed25519, &self.sqisign])
    }

    pub fn address(&self, network: Network) -> Address {
        Address::new(network, AddressType::PostQuantumHash, self.key_hash())
    }
}

/// A quantum-safe key pair: an Ed25519 key and a SQIsign key, both from one 32-byte seed.
pub struct QuantumSafeKey {
    ed25519: SigningKey,
    sqisign: sqisign_rs::SigningKey<Level1>,
    /// Kept to make each SQIsign signature's randomness from the key and the message, so
    /// signing needs no source of randomness (a browser wallet has none to rely on).
    seed: [u8; 32],
}

impl QuantumSafeKey {
    /// Both keys from one seed: the same seed always gives the same keys. A wallet derives the
    /// seed from its recovery phrase, so the phrase alone restores a quantum-safe address.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let ed25519 = SigningKey::from_bytes(&blake3::derive_key(ED25519_SEED_CONTEXT, seed));
        let mut rng = ChaCha20Rng::from_seed(blake3::derive_key(SQISIGN_SEED_CONTEXT, seed));
        let (_, sqisign) = sqisign_rs::generate::<Level1>(&mut rng);
        Self { ed25519, sqisign, seed: *seed }
    }

    pub fn public_key(&self) -> QuantumSafePublicKey {
        let sqisign: [u8; SQISIGN_PUBLIC_KEY_BYTES] =
            self.sqisign.public_key().to_bytes().as_slice().try_into().expect("a level I key is 83 bytes");
        QuantumSafePublicKey { ed25519: self.ed25519.verifying_key().to_bytes(), sqisign }
    }

    pub fn address(&self, network: Network) -> Address {
        self.public_key().address(network)
    }
}

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidSignature(format!("quantum-safe spend: {}", reason))
}

impl Transaction {
    /// Signs an input that spends a quantum-safe coin, with both keys.
    pub fn sign_input_quantum_safe(&mut self, network: Network, input_index: usize, key: &QuantumSafeKey) -> Result<(), TransactionError> {
        if input_index >= self.inputs.len() {
            return Err(TransactionError::EmptyInputs);
        }
        let sighash = self.sig_hash(network, input_index);
        let ed_signature = key.ed25519.sign(sighash.as_bytes());
        let mut rng = ChaCha20Rng::from_seed(blake3::derive_key(SQISIGN_NONCE_CONTEXT, &[&key.seed[..], sighash.as_bytes()].concat()));
        let sq_signature = key.sqisign.sign(sighash.as_bytes(), &mut rng).map_err(|_| invalid("SQIsign could not sign"))?;

        let public = key.public_key();
        let mut script = Vec::with_capacity(QUANTUM_SAFE_WITNESS_BYTES);
        script.extend_from_slice(&public.ed25519);
        script.extend_from_slice(&public.sqisign);
        script.extend_from_slice(&ed_signature.to_bytes());
        script.extend_from_slice(&sq_signature.to_bytes());
        self.inputs[input_index].signature_script = script;
        Ok(())
    }

    /// Pays `amount_atoms` to `recipient` from a quantum-safe address's coins, change back to
    /// it, signing every input with both keys. At most `MAX_RELAYED_INPUTS` coins are used:
    /// nodes relay no more, since each costs a SQIsign check.
    #[allow(clippy::too_many_arguments)]
    pub fn build_quantum_safe_payment(
        key: &QuantumSafeKey,
        network: Network,
        recipient: &Address,
        amount_atoms: u64,
        fee_atoms: u64,
        available_utxos: Vec<(crate::transaction::Outpoint, crate::transaction::TxOutput)>,
        service: Option<&Address>,
        invoice_id: Option<&str>,
    ) -> Result<Self, String> {
        let mut tx = Self::plan_quantum_safe_payment(
            &key.address(network),
            recipient,
            amount_atoms,
            fee_atoms,
            available_utxos,
            service,
            invoice_id,
        )?;
        tx.sign_all_quantum_safe(network, key)?;
        Ok(tx)
    }

    /// The payment `build_quantum_safe_payment` makes, not yet signed: what it spends and pays.
    /// With `signed_size` it tells the fee before any (slow) signing.
    #[allow(clippy::too_many_arguments)]
    pub fn plan_quantum_safe_payment(
        from: &Address,
        recipient: &Address,
        amount_atoms: u64,
        fee_atoms: u64,
        available_utxos: Vec<(crate::transaction::Outpoint, crate::transaction::TxOutput)>,
        service: Option<&Address>,
        invoice_id: Option<&str>,
    ) -> Result<Self, String> {
        let tx = Self::unsigned_payment(from, recipient, amount_atoms, fee_atoms, available_utxos, service, invoice_id)?;
        if tx.inputs.len() > MAX_RELAYED_INPUTS {
            return Err(format!(
                "This payment needs {} coins from the quantum-safe address, more than the {} one payment may use. Send a smaller amount first.",
                tx.inputs.len(),
                MAX_RELAYED_INPUTS
            ));
        }
        Ok(tx)
    }

    /// Signs every input with the quantum-safe key.
    pub fn sign_all_quantum_safe(&mut self, network: Network, key: &QuantumSafeKey) -> Result<(), String> {
        for i in 0..self.inputs.len() {
            self.sign_input_quantum_safe(network, i, key).map_err(|e| format!("Failed to sign input {}: {}", i, e))?;
        }
        Ok(())
    }

    /// The size this transaction will have once every input is signed for a quantum-safe address.
    /// Their signature scripts have a fixed length, so the fee is known before signing.
    pub fn quantum_safe_signed_size(&self) -> usize {
        let unsigned: usize = self.inputs.iter().map(|i| i.signature_script.len()).sum();
        crate::serialize::Encode::to_bytes(self).len() - unsigned + self.inputs.len() * QUANTUM_SAFE_WITNESS_BYTES
    }

    /// Checks an input that spends a quantum-safe coin: both keys hash to the address, and both
    /// signatures are valid. The cheap checks come first.
    pub(crate) fn verify_quantum_safe_input(&self, network: Network, input_index: usize, key_hash: &[u8]) -> Result<(), TransactionError> {
        let input = self.inputs.get(input_index).ok_or(TransactionError::EmptyInputs)?;
        let bytes = &input.signature_script;
        if bytes.len() != QUANTUM_SAFE_WITNESS_BYTES {
            return Err(TransactionError::InvalidSignatureScript(bytes.len()));
        }
        let (ed_key, rest) = bytes.split_at(32);
        let (sq_key, rest) = rest.split_at(SQISIGN_PUBLIC_KEY_BYTES);
        let (ed_signature, sq_signature) = rest.split_at(64);

        let public = QuantumSafePublicKey { ed25519: ed_key.try_into().expect("32 bytes"), sqisign: sq_key.try_into().expect("83 bytes") };
        if public.key_hash().0[..] != *key_hash {
            return Err(TransactionError::PublicKeyAddressMismatch);
        }

        let sighash = self.sig_hash(network, input_index);
        VerifyingKey::from_bytes(&public.ed25519)
            .and_then(|key| key.verify_strict(sighash.as_bytes(), &Signature::from_bytes(ed_signature.try_into().expect("64 bytes"))))
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))?;

        let sq_key = sqisign_rs::PublicKey::<Level1>::from_bytes(&public.sqisign).map_err(|_| invalid("malformed SQIsign key"))?;
        let sq_signature = sqisign_rs::Signature::<Level1>::from_bytes(sq_signature).map_err(|_| invalid("malformed SQIsign signature"))?;
        sq_key.verify(sighash.as_bytes(), &sq_signature).map_err(|_| invalid("the SQIsign signature is not valid"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{Outpoint, ScriptPublicKey, TxInput, TxOutput};

    const NET: Network = Network::Testnet;

    fn spend(key: &QuantumSafeKey) -> (Transaction, ScriptPublicKey) {
        let locked = ScriptPublicKey::pay_to_address(&key.address(NET));
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_outpoint: Outpoint { transaction_id: Hash([3u8; 32]), index: 0 },
                signature_script: Vec::new(),
            }],
            outputs: vec![TxOutput::new(1_000_000, ScriptPublicKey { version: 0, script: vec![9u8; 32] })],
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        tx.sign_input_quantum_safe(NET, 0, key).unwrap();
        (tx, locked)
    }

    #[test]
    fn the_signed_size_is_known_before_signing() {
        let key = QuantumSafeKey::from_seed(&[1u8; 32]);
        let (signed, _) = spend(&key);
        let mut unsigned = signed.clone();
        unsigned.inputs[0].signature_script.clear();
        assert_eq!(unsigned.quantum_safe_signed_size(), crate::serialize::Encode::to_bytes(&signed).len());
    }

    #[test]
    fn keys_come_from_the_seed_and_addresses_are_type_one() {
        let a = QuantumSafeKey::from_seed(&[1u8; 32]);
        let again = QuantumSafeKey::from_seed(&[1u8; 32]);
        let other = QuantumSafeKey::from_seed(&[2u8; 32]);
        assert_eq!(a.public_key(), again.public_key());
        assert_ne!(a.public_key(), other.public_key());
        let address = a.address(NET);
        assert_eq!(address.address_type, AddressType::PostQuantumHash);
        assert_eq!(Address::decode(&address.to_string()).unwrap(), address);
    }

    #[test]
    fn a_spend_needs_both_signatures() {
        let key = QuantumSafeKey::from_seed(&[1u8; 32]);
        let (tx, locked) = spend(&key);
        assert_eq!(tx.inputs[0].signature_script.len(), QUANTUM_SAFE_WITNESS_BYTES);
        assert!(tx.verify_input(NET, 0, &locked).is_ok());

        // Either signature altered: refused
        let ed_at = 32 + SQISIGN_PUBLIC_KEY_BYTES;
        for at in [ed_at + 5, ed_at + 64 + 40] {
            let mut bad = tx.clone();
            bad.inputs[0].signature_script[at] ^= 1;
            assert!(bad.verify_input(NET, 0, &locked).is_err(), "byte {} altered", at);
        }
        // Someone else's keys, or the transaction changed after signing: refused
        let (theirs, _) = spend(&QuantumSafeKey::from_seed(&[2u8; 32]));
        assert_eq!(theirs.verify_input(NET, 0, &locked), Err(TransactionError::PublicKeyAddressMismatch));
        let mut changed = tx.clone();
        changed.outputs[0].value_atoms += 1;
        assert!(changed.verify_input(NET, 0, &locked).is_err());
        // Another network: refused
        assert!(tx.verify_input(Network::Mainnet, 0, &ScriptPublicKey::pay_to_address(&key.address(Network::Mainnet))).is_err());
        // A truncated script: refused without any expensive check
        let mut short = tx.clone();
        short.inputs[0].signature_script.truncate(96);
        assert!(matches!(short.verify_input(NET, 0, &locked), Err(TransactionError::InvalidSignatureScript(96))));
    }

    /// An Ed25519 signature alone, as for an ordinary address, cannot spend a quantum-safe coin.
    #[test]
    fn an_ed25519_signature_alone_is_not_enough() {
        let key = QuantumSafeKey::from_seed(&[1u8; 32]);
        let (mut tx, locked) = spend(&key);
        let ed = SigningKey::from_bytes(&blake3::derive_key(ED25519_SEED_CONTEXT, &[1u8; 32]));
        tx.sign_input(NET, 0, &ed).unwrap();
        assert!(tx.verify_input(NET, 0, &locked).is_err());
    }
}
