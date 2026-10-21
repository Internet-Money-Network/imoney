//! Hash-and-time locks: the building block of an atomic swap.
//!
//! Coins sent to a hash-lock address can be spent in exactly two ways:
//!
//! - by the **recipient**, with their signature and a 32-byte secret whose SHA-256 hash is
//!   the one written in the lock; or
//! - by the **refunder**, with their signature, once the spending block's blue score has
//!   reached the lock's deadline.
//!
//! Two people who lock coins for each other under the same hash, on this chain and on
//! another, can trade without trusting each other or anyone else: claiming one side reveals
//! the secret that claims the other, and if either walks away both are refunded. SHA-256 is
//! used because Bitcoin-style chains and Ethereum-style chains can both check it cheaply.
//!
//! An output is locked to the hash of the script, as with a multi-signature address, so the
//! terms stay private until the coins are spent. The signature script of a spend is
//! `script (104 bytes) ++ branch (1) ++ signature (64)`, followed by the 32-byte secret when
//! the branch is a claim.

use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use crate::serialize::tagged_hash;
use crate::transaction::{Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// Script version of an output locked to a hash-lock script.
pub const SCRIPT_VERSION_HASHLOCK: u8 = AddressType::HashLock as u8;

const SCRIPT_HASH_CONTEXT: &str = "IMN 2026 hash lock script";
const SCRIPT_BYTES: usize = 32 + 32 + 32 + 8;
const BRANCH_CLAIM: u8 = 0;
const BRANCH_REFUND: u8 = 1;
const CLAIM_BYTES: usize = SCRIPT_BYTES + 1 + 64 + 32;
const REFUND_BYTES: usize = SCRIPT_BYTES + 1 + 64;

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidHashLock(reason.to_string())
}

/// SHA-256 of a swap secret: what goes into the lock on both chains.
pub fn secret_hash(secret: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(secret).into()
}

/// The terms of a hash lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashLockScript {
    /// Public key of whoever may claim with the secret.
    pub recipient: [u8; 32],
    /// Public key of whoever gets the coins back after the deadline.
    pub refunder: [u8; 32],
    /// SHA-256 of the secret.
    pub hash: [u8; 32],
    /// The refund becomes valid in blocks whose blue score is at least this. One blue score
    /// is about five seconds, so 720 is about an hour.
    pub refund_after: u64,
}

impl HashLockScript {
    pub fn new(recipient: [u8; 32], refunder: [u8; 32], hash: [u8; 32], refund_after: u64) -> Result<Self, TransactionError> {
        let script = Self { recipient, refunder, hash, refund_after };
        script.check()?;
        Ok(script)
    }

    fn check(&self) -> Result<(), TransactionError> {
        if VerifyingKey::from_bytes(&self.recipient).is_err() || VerifyingKey::from_bytes(&self.refunder).is_err() {
            return Err(invalid("the recipient and refunder must be valid Ed25519 public keys"));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SCRIPT_BYTES);
        bytes.extend_from_slice(&self.recipient);
        bytes.extend_from_slice(&self.refunder);
        bytes.extend_from_slice(&self.hash);
        bytes.extend_from_slice(&self.refund_after.to_be_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        if bytes.len() != SCRIPT_BYTES {
            return Err(invalid("a hash-lock script is 104 bytes"));
        }
        let part = |from: usize| -> [u8; 32] { bytes[from..from + 32].try_into().expect("32 bytes") };
        Self::new(part(0), part(32), part(64), u64::from_be_bytes(bytes[96..104].try_into().expect("8 bytes")))
    }

    /// The hash an output is locked to.
    pub fn script_hash(&self) -> Hash {
        tagged_hash(SCRIPT_HASH_CONTEXT, &[&self.to_bytes()])
    }

    pub fn address(&self, network: Network) -> Address {
        Address::new(network, AddressType::HashLock, self.script_hash())
    }
}

/// What a hash-lock input revealed about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashLockSpend {
    /// Claimed by the recipient, revealing the secret.
    Claim { script: HashLockScript, secret: [u8; 32] },
    /// Taken back by the refunder.
    Refund { script: HashLockScript },
}

impl Transaction {
    /// Reads a hash-lock spend from an input's signature script, without checking it.
    pub fn hashlock_spend(&self, input_index: usize) -> Option<HashLockSpend> {
        let bytes = &self.inputs.get(input_index)?.signature_script;
        if bytes.len() != CLAIM_BYTES && bytes.len() != REFUND_BYTES {
            return None;
        }
        let script = HashLockScript::from_bytes(&bytes[..SCRIPT_BYTES]).ok()?;
        match (bytes[SCRIPT_BYTES], bytes.len()) {
            (BRANCH_CLAIM, CLAIM_BYTES) => {
                Some(HashLockSpend::Claim { script, secret: bytes[CLAIM_BYTES - 32..].try_into().expect("32 bytes") })
            }
            (BRANCH_REFUND, REFUND_BYTES) => Some(HashLockSpend::Refund { script }),
            _ => None,
        }
    }

    /// The secret a claim revealed, for the other party of a swap to read off the chain.
    pub fn revealed_secret(&self, input_index: usize) -> Option<[u8; 32]> {
        match self.hashlock_spend(input_index)? {
            HashLockSpend::Claim { secret, .. } => Some(secret),
            HashLockSpend::Refund { .. } => None,
        }
    }

    /// Checks an input that spends a hash-lock output in a block of the given blue score.
    pub(crate) fn verify_hashlock_input(
        &self,
        network: Network,
        input_index: usize,
        script_hash: &[u8],
        spending_blue_score: u64,
    ) -> Result<(), TransactionError> {
        let spend = self.hashlock_spend(input_index).ok_or_else(|| invalid("malformed signature script"))?;
        let (script, key) = match &spend {
            HashLockSpend::Claim { script, secret } => {
                if secret_hash(secret) != script.hash {
                    return Err(invalid("the secret does not match the lock"));
                }
                // Claim and refund never overlap. Otherwise a swap buyer could wait until both
                // deadlines pass, take back their payment on the other chain, and still claim
                // here if the seller had not refunded in time.
                if spending_blue_score >= script.refund_after {
                    return Err(invalid("the claim period is over; only a refund is possible"));
                }
                (script, script.recipient)
            }
            HashLockSpend::Refund { script } => {
                if spending_blue_score < script.refund_after {
                    return Err(invalid("the refund is not valid yet"));
                }
                (script, script.refunder)
            }
        };
        if script.script_hash().0[..] != *script_hash {
            return Err(TransactionError::PublicKeyAddressMismatch);
        }
        let signature_bytes: [u8; 64] = self.inputs[input_index].signature_script[SCRIPT_BYTES + 1..SCRIPT_BYTES + 65]
            .try_into()
            .expect("64 bytes");
        VerifyingKey::from_bytes(&key)
            .and_then(|key| key.verify_strict(self.sig_hash(network, input_index).as_bytes(), &Signature::from_bytes(&signature_bytes)))
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))
    }

    /// Spends one hash-lock coin to `to`, less the fee: a claim when `secret` is given, a
    /// refund otherwise. `key` is the recipient's key for a claim and the refunder's for a
    /// refund. A refund built before the deadline is valid but will be refused until then.
    ///
    /// A token the coin carries moves with it; the fee comes from the coin's IMN, so a lock
    /// that holds a token must also hold at least the fee in IMN.
    pub fn spend_hashlock(
        script: &HashLockScript,
        network: Network,
        coin: (Outpoint, TxOutput),
        to: &Address,
        fee_atoms: u64,
        key: &SigningKey,
        secret: Option<[u8; 32]>,
    ) -> Result<Self, String> {
        let (outpoint, locked) = coin;
        let amount = locked.value_atoms.checked_sub(fee_atoms).ok_or("The fee exceeds the IMN in the lock")?;
        if amount == 0 && locked.token.is_none() {
            return Err("The fee takes everything in the lock".to_string());
        }
        let expected = if secret.is_some() { script.recipient } else { script.refunder };
        if key.verifying_key().to_bytes() != expected {
            return Err("That key is not the one this branch of the lock names".to_string());
        }
        if secret.is_some_and(|secret| secret_hash(&secret) != script.hash) {
            return Err("The secret does not match the lock".to_string());
        }
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new() }],
            outputs: vec![TxOutput { value_atoms: amount, script_public_key: ScriptPublicKey::pay_to_address(to), token: locked.token }],
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        let signature = key.sign(tx.sig_hash(network, 0).as_bytes());
        let mut signature_script = script.to_bytes();
        signature_script.push(if secret.is_some() { BRANCH_CLAIM } else { BRANCH_REFUND });
        signature_script.extend_from_slice(&signature.to_bytes());
        if let Some(secret) = secret {
            signature_script.extend_from_slice(&secret);
        }
        tx.inputs[0].signature_script = signature_script;
        Ok(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn setup() -> (SigningKey, SigningKey, [u8; 32], HashLockScript, Address) {
        let (recipient, refunder, secret) = (key(1), key(2), [7u8; 32]);
        let script =
            HashLockScript::new(recipient.verifying_key().to_bytes(), refunder.verifying_key().to_bytes(), secret_hash(&secret), 100).unwrap();
        let to = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, &[9u8; 32]);
        (recipient, refunder, secret, script, to)
    }

    fn coin() -> (Outpoint, TxOutput) {
        (Outpoint { transaction_id: Hash([3u8; 32]), index: 0 }, TxOutput::new(1_000_000, ScriptPublicKey::burn()))
    }

    fn locked(script: &HashLockScript) -> ScriptPublicKey {
        ScriptPublicKey::pay_to_address(&script.address(Network::Testnet))
    }

    #[test]
    fn script_round_trips_and_has_one_address() {
        let (_, _, _, script, _) = setup();
        assert_eq!(HashLockScript::from_bytes(&script.to_bytes()).unwrap(), script);
        assert_eq!(script.address(Network::Testnet).to_string(), script.clone().address(Network::Testnet).to_string());
        assert!(HashLockScript::from_bytes(&script.to_bytes()[1..]).is_err());
        assert_eq!(locked(&script).version, SCRIPT_VERSION_HASHLOCK);
    }

    #[test]
    fn the_recipient_claims_with_the_secret_until_the_refund_time() {
        let (recipient, _, secret, script, to) = setup();
        let tx = Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &recipient, Some(secret)).unwrap();
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 0).is_ok());
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 99).is_ok());
        // From the refund time on, only the refunder can move the coins
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 100).is_err());
        assert_eq!(tx.revealed_secret(0), Some(secret));
        assert_eq!(tx.outputs[0].value_atoms, 999_000);
        // The same claim is not valid on another network
        assert!(tx.verify_input_at(Network::Mainnet, 0, &locked(&script), 0).is_err());
    }

    #[test]
    fn a_claim_needs_the_right_secret_and_the_recipients_key() {
        let (recipient, refunder, secret, script, to) = setup();
        assert!(Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &recipient, Some([8u8; 32])).is_err());
        assert!(Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &refunder, Some(secret)).is_err());

        // Forged by hand: the right secret, signed by the refunder
        let mut tx = Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &recipient, Some(secret)).unwrap();
        let forged = refunder.sign(tx.sig_hash(Network::Testnet, 0).as_bytes());
        tx.inputs[0].signature_script[SCRIPT_BYTES + 1..SCRIPT_BYTES + 65].copy_from_slice(&forged.to_bytes());
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 1_000).is_err());

        // A wrong secret of the right length
        let mut tx = Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &recipient, Some(secret)).unwrap();
        let end = tx.inputs[0].signature_script.len();
        tx.inputs[0].signature_script[end - 1] ^= 1;
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 1_000).is_err());
    }

    #[test]
    fn the_refunder_takes_the_coins_back_only_after_the_deadline() {
        let (recipient, refunder, _, script, to) = setup();
        let tx = Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &refunder, None).unwrap();
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 99).is_err());
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 100).is_ok());
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 5_000).is_ok());
        assert_eq!(tx.revealed_secret(0), None);
        // Without a block height to judge by, a refund is never accepted
        assert!(tx.verify_input(Network::Testnet, 0, &locked(&script)).is_err());
        // The recipient cannot use the refund branch
        assert!(Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &recipient, None).is_err());
    }

    #[test]
    fn a_spend_must_present_the_script_the_coin_is_locked_to() {
        let (recipient, refunder, secret, script, to) = setup();
        // Same parties and secret, a deadline that has already passed
        let easier = HashLockScript::new(script.recipient, script.refunder, script.hash, 0).unwrap();
        let tx = Transaction::spend_hashlock(&easier, Network::Testnet, coin(), &to, 1_000, &refunder, None).unwrap();
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 50).is_err());
        // Changing where the money goes after signing breaks the signature
        let mut tx = Transaction::spend_hashlock(&script, Network::Testnet, coin(), &to, 1_000, &recipient, Some(secret)).unwrap();
        tx.outputs[0].value_atoms += 1;
        assert!(tx.verify_input_at(Network::Testnet, 0, &locked(&script), 0).is_err());
    }
}
