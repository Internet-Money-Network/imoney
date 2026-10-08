//! Multi-signature addresses: coins that need `threshold` of a fixed set of Ed25519 keys to move.
//!
//! The address (type `ScriptHash`) is the hash of a small script naming the threshold and the
//! keys. Spending reveals that script and gives `threshold` signatures over the ordinary
//! signing hash. Signatures do not cover each other, so the key holders can sign in any order,
//! on different machines, and the pieces are put together at the end.

use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use crate::serialize::tagged_hash;
use crate::transaction::{
    is_valid_invoice_id, Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput, INVOICE_TAG,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Script version 2: the script is the 32-byte hash of a multi-signature script.
pub const SCRIPT_VERSION_MULTISIG: u8 = AddressType::ScriptHash as u8;
/// Most keys one multi-signature address may name.
pub const MAX_MULTISIG_KEYS: usize = 16;

const SCRIPT_HASH_CONTEXT: &str = "IMN 2026 multisig script";
/// Bytes of one signature in a signature script: the key's position, then the signature.
const SIGNATURE_ENTRY_BYTES: usize = 1 + 64;

/// One key holder's signature for one input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialSignature {
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
}

/// The rule behind a multi-signature address: any `threshold` of `keys` may spend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultisigScript {
    threshold: u8,
    /// Ed25519 public keys in ascending byte order, so a set of keys has exactly one address.
    keys: Vec<[u8; 32]>,
}

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidMultisig(reason.to_string())
}

impl MultisigScript {
    /// The script for `threshold` of `keys`. The order the keys are given in does not matter.
    pub fn new(threshold: u8, keys: &[[u8; 32]]) -> Result<Self, TransactionError> {
        let mut keys = keys.to_vec();
        keys.sort_unstable();
        let script = Self { threshold, keys };
        script.check()?;
        Ok(script)
    }

    fn check(&self) -> Result<(), TransactionError> {
        if self.keys.is_empty() || self.keys.len() > MAX_MULTISIG_KEYS {
            return Err(invalid("a multi-signature address names 1 to 16 keys"));
        }
        if self.threshold == 0 || self.threshold as usize > self.keys.len() {
            return Err(invalid("the threshold must be between 1 and the number of keys"));
        }
        if self.keys.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(invalid("keys must be distinct and in ascending order"));
        }
        if self.keys.iter().any(|key| VerifyingKey::from_bytes(key).is_err()) {
            return Err(invalid("not every key is a valid Ed25519 public key"));
        }
        Ok(())
    }

    pub fn threshold(&self) -> u8 {
        self.threshold
    }

    pub fn keys(&self) -> &[[u8; 32]] {
        &self.keys
    }

    /// The script as revealed when spending: threshold, key count, then the keys.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + 32 * self.keys.len());
        out.push(self.threshold);
        out.push(self.keys.len() as u8);
        for key in &self.keys {
            out.extend_from_slice(key);
        }
        out
    }

    /// Parses a script, accepting only the one canonical form.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        let (&threshold, rest) = bytes.split_first().ok_or_else(|| invalid("empty script"))?;
        let (&count, key_bytes) = rest.split_first().ok_or_else(|| invalid("truncated script"))?;
        if key_bytes.len() != count as usize * 32 {
            return Err(invalid("script length does not match its key count"));
        }
        let keys = key_bytes.chunks_exact(32).map(|chunk| chunk.try_into().expect("32 bytes")).collect();
        let script = Self { threshold, keys };
        script.check()?;
        Ok(script)
    }

    /// The hash an output is locked to.
    pub fn script_hash(&self) -> Hash {
        tagged_hash(SCRIPT_HASH_CONTEXT, &[&self.to_bytes()])
    }

    pub fn address(&self, network: Network) -> Address {
        Address::new(network, AddressType::ScriptHash, self.script_hash())
    }

    /// Bytes of the signature script of one input spending from this address.
    pub fn signature_script_len(&self) -> usize {
        2 + 32 * self.keys.len() + SIGNATURE_ENTRY_BYTES * self.threshold as usize
    }
}

impl Transaction {
    /// One key holder's signature for an input of a multi-signature spend.
    pub fn multisig_sign(&self, network: Network, input_index: usize, signing_key: &SigningKey) -> PartialSignature {
        let sighash = self.sig_hash(network, input_index);
        PartialSignature {
            public_key: signing_key.verifying_key().to_bytes(),
            signature: signing_key.sign(sighash.as_bytes()).to_bytes(),
        }
    }

    /// Puts collected signatures on an input. Needs at least the script's threshold of
    /// signatures from its keys; extra ones are left out.
    pub fn set_multisig_signatures(
        &mut self,
        input_index: usize,
        script: &MultisigScript,
        signatures: &[PartialSignature],
    ) -> Result<(), TransactionError> {
        let input = self.inputs.get_mut(input_index).ok_or(TransactionError::EmptyInputs)?;
        let mut signature_script = script.to_bytes();
        let mut used = 0;
        // In key order, as the verifier requires
        for (position, key) in script.keys.iter().enumerate() {
            if used == script.threshold {
                break;
            }
            if let Some(partial) = signatures.iter().find(|partial| &partial.public_key == key) {
                signature_script.push(position as u8);
                signature_script.extend_from_slice(&partial.signature);
                used += 1;
            }
        }
        if used < script.threshold {
            return Err(invalid("fewer signatures than the address requires"));
        }
        input.signature_script = signature_script;
        Ok(())
    }

    /// Checks an input that spends a multi-signature output locked to `script_hash`.
    pub(crate) fn verify_multisig_input(
        &self,
        network: Network,
        input_index: usize,
        script_hash: &[u8],
    ) -> Result<(), TransactionError> {
        let signature_script = &self.inputs.get(input_index).ok_or(TransactionError::EmptyInputs)?.signature_script;
        let key_count = *signature_script.get(1).ok_or_else(|| invalid("truncated signature script"))? as usize;
        let script_len = 2 + 32 * key_count;
        if signature_script.len() < script_len {
            return Err(invalid("truncated signature script"));
        }
        let (script_bytes, signature_bytes) = signature_script.split_at(script_len);
        let script = MultisigScript::from_bytes(script_bytes)?;
        if script.script_hash().0[..] != *script_hash {
            return Err(TransactionError::PublicKeyAddressMismatch);
        }
        // Exactly the threshold: nothing may be appended to a valid signature script
        if signature_bytes.len() != SIGNATURE_ENTRY_BYTES * script.threshold as usize {
            return Err(invalid("wrong number of signatures"));
        }

        let sighash = self.sig_hash(network, input_index);
        let mut previous: Option<u8> = None;
        for entry in signature_bytes.chunks_exact(SIGNATURE_ENTRY_BYTES) {
            let position = entry[0];
            // Strictly ascending positions: no key signs twice
            if previous.is_some_and(|last| position <= last) {
                return Err(invalid("signatures must be in key order, one per key"));
            }
            previous = Some(position);
            let key = script.keys.get(position as usize).ok_or_else(|| invalid("signature names a key that is not in the script"))?;
            let verifying_key = VerifyingKey::from_bytes(key).map_err(|e| TransactionError::InvalidSignature(e.to_string()))?;
            let signature = Signature::from_bytes(entry[1..].try_into().expect("64 bytes"));
            verifying_key
                .verify(sighash.as_bytes(), &signature)
                .map_err(|e| TransactionError::InvalidSignature(e.to_string()))?;
        }
        Ok(())
    }

    /// Builds an unsigned payment from a multi-signature address, with change returned to it.
    /// Each key holder then calls `multisig_sign` for every input, and `set_multisig_signatures`
    /// puts the results on.
    #[allow(clippy::too_many_arguments)]
    pub fn build_multisig_payment(
        script: &MultisigScript,
        network: Network,
        recipient: &Address,
        amount_atoms: u64,
        fee_atoms: u64,
        available_utxos: Vec<(Outpoint, TxOutput)>,
        invoice_id: Option<&str>,
        service: Option<&Address>,
    ) -> Result<Transaction, String> {
        let payload = match invoice_id {
            Some(id) if is_valid_invoice_id(id) => [INVOICE_TAG, id.as_bytes()].concat(),
            Some(_) => return Err("Invoice ID must be 1-64 characters of A-Z a-z 0-9 - _ .".to_string()),
            None => Vec::new(),
        };
        let required = amount_atoms.checked_add(fee_atoms).ok_or_else(|| "Amount plus fee overflows".to_string())?;

        let mut gathered: u64 = 0;
        let mut inputs = Vec::new();
        for (outpoint, output) in available_utxos {
            gathered = gathered.checked_add(output.value_atoms).ok_or_else(|| "Selected inputs overflow".to_string())?;
            inputs.push(TxInput { previous_outpoint: outpoint, signature_script: Vec::new(), sequence: 0 });
            if gathered >= required {
                break;
            }
        }
        if gathered < required {
            return Err(format!("Insufficient funds: have {} atoms, require {} atoms", gathered, required));
        }

        let mut outputs = vec![TxOutput { value_atoms: amount_atoms, script_public_key: ScriptPublicKey::pay_to_address(recipient) }];
        if gathered > required {
            outputs.push(TxOutput {
                value_atoms: gathered - required,
                script_public_key: ScriptPublicKey::pay_to_address(&script.address(network)),
            });
        }
        Ok(Transaction {
            version: 1,
            inputs,
            outputs,
            lock_time: 0,
            subnetwork_id: [0u8; 20],
            gas: 0,
            payload,
            service: service.map(ScriptPublicKey::pay_to_address),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn public(seed: u8) -> [u8; 32] {
        key(seed).verifying_key().to_bytes()
    }

    /// A two-of-three script and a transaction spending one coin locked to it.
    fn two_of_three() -> (MultisigScript, Transaction) {
        let script = MultisigScript::new(2, &[public(1), public(2), public(3)]).unwrap();
        let coin = (
            Outpoint { transaction_id: Hash([7u8; 32]), index: 0 },
            TxOutput { value_atoms: 100_000, script_public_key: ScriptPublicKey::pay_to_address(&script.address(Network::Testnet)) },
        );
        let recipient = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &public(9));
        let tx = Transaction::build_multisig_payment(&script, Network::Testnet, &recipient, 60_000, 1_000, vec![coin], Some("INV-7"), None)
            .unwrap();
        (script, tx)
    }

    fn verify(tx: &Transaction, script: &MultisigScript) -> Result<(), TransactionError> {
        tx.verify_input(Network::Testnet, 0, &ScriptPublicKey::pay_to_address(&script.address(Network::Testnet)))
    }

    #[test]
    fn the_address_does_not_depend_on_the_order_keys_are_given_in() {
        let a = MultisigScript::new(2, &[public(1), public(2), public(3)]).unwrap();
        let b = MultisigScript::new(2, &[public(3), public(1), public(2)]).unwrap();
        assert_eq!(a.address(Network::Testnet), b.address(Network::Testnet));
        // A different threshold over the same keys is a different address
        let c = MultisigScript::new(3, &[public(1), public(2), public(3)]).unwrap();
        assert_ne!(a.address(Network::Testnet), c.address(Network::Testnet));
        assert_eq!(MultisigScript::from_bytes(&a.to_bytes()).unwrap(), a);
        assert!(a.address(Network::Testnet).to_string().starts_with("imntest:"));
    }

    #[test]
    fn bad_scripts_are_refused() {
        assert!(MultisigScript::new(0, &[public(1)]).is_err());
        assert!(MultisigScript::new(3, &[public(1), public(2)]).is_err());
        assert!(MultisigScript::new(1, &[public(1), public(1)]).is_err());
        assert!(MultisigScript::new(1, &[]).is_err());
        let seventeen: Vec<[u8; 32]> = (1..=17).map(public).collect();
        assert!(MultisigScript::new(2, &seventeen).is_err());

        // Keys out of order are not the canonical form
        let good = MultisigScript::new(1, &[public(1), public(2)]).unwrap().to_bytes();
        let mut swapped = good[..2].to_vec();
        swapped.extend_from_slice(&good[34..66]);
        swapped.extend_from_slice(&good[2..34]);
        assert!(MultisigScript::from_bytes(&swapped).is_err());
        assert!(MultisigScript::from_bytes(&good[..good.len() - 1]).is_err());
    }

    #[test]
    fn any_two_of_three_keys_can_spend() {
        for signers in [[1u8, 2], [1, 3], [2, 3], [3, 1]] {
            let (script, mut tx) = two_of_three();
            let signatures: Vec<PartialSignature> =
                signers.iter().map(|seed| tx.multisig_sign(Network::Testnet, 0, &key(*seed))).collect();
            tx.set_multisig_signatures(0, &script, &signatures).unwrap();
            assert_eq!(tx.inputs[0].signature_script.len(), script.signature_script_len());
            assert_eq!(verify(&tx, &script), Ok(()));
        }
    }

    #[test]
    fn one_signature_or_an_outsider_cannot_spend() {
        let (script, mut tx) = two_of_three();
        let one = vec![tx.multisig_sign(Network::Testnet, 0, &key(1))];
        assert!(tx.set_multisig_signatures(0, &script, &one).is_err());

        // A key that is not in the script does not count
        let with_outsider = vec![tx.multisig_sign(Network::Testnet, 0, &key(1)), tx.multisig_sign(Network::Testnet, 0, &key(4))];
        assert!(tx.set_multisig_signatures(0, &script, &with_outsider).is_err());

        // Hand-built script with the same key signing twice
        let signature = tx.multisig_sign(Network::Testnet, 0, &key(1)).signature;
        let mut forged = script.to_bytes();
        for _ in 0..2 {
            forged.push(0);
            forged.extend_from_slice(&signature);
        }
        tx.inputs[0].signature_script = forged;
        assert!(verify(&tx, &script).is_err());
    }

    #[test]
    fn signatures_are_bound_to_the_transaction_the_network_and_the_script() {
        let (script, mut tx) = two_of_three();
        let signatures: Vec<PartialSignature> = [1u8, 2].iter().map(|seed| tx.multisig_sign(Network::Testnet, 0, &key(*seed))).collect();
        tx.set_multisig_signatures(0, &script, &signatures).unwrap();
        assert_eq!(verify(&tx, &script), Ok(()));

        // Not valid on the other network
        let locked = ScriptPublicKey::pay_to_address(&script.address(Network::Testnet));
        assert!(tx.verify_input(Network::Mainnet, 0, &locked).is_err());

        // Not valid for a changed payment
        let mut altered = tx.clone();
        altered.outputs[0].value_atoms += 1;
        assert!(verify(&altered, &script).is_err());

        // Not valid with anything appended
        let mut padded = tx.clone();
        padded.inputs[0].signature_script.push(0);
        assert!(verify(&padded, &script).is_err());

        // A different script cannot claim the coin, even one whose keys can sign
        let other = MultisigScript::new(1, &[public(1), public(2)]).unwrap();
        let mut stolen = tx.clone();
        let signature = stolen.multisig_sign(Network::Testnet, 0, &key(1));
        stolen.set_multisig_signatures(0, &other, &[signature]).unwrap();
        assert_eq!(verify(&stolen, &script), Err(TransactionError::PublicKeyAddressMismatch));
    }
}
