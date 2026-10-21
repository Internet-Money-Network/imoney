//! Vaults: savings that can only leave slowly, with a backup key that can stop a withdrawal.
//!
//! A vault address locks coins to the hash of a script naming an everyday key, a backup key and
//! a delay. The everyday key cannot send the coins anywhere directly; it can only *start a
//! withdrawal*, which moves them into a pending output naming the destination and the blue score
//! from which the withdrawal completes. Until then the backup key can take them back. After it,
//! anyone may complete the withdrawal to the named destination, and only there. The backup key
//! can also move vault coins directly at any time; it is meant to be kept offline.
//!
//! So a thief who takes the everyday key gains nothing quickly: the owner sees the pending
//! withdrawal and uses the backup key to pull the money back before it completes.
//!
//! Signature scripts:
//! - vault coin, start: `vault script (72) ++ [0] ++ signature (64) ++ pending terms (73) ++ output (u16)`
//! - vault coin, recover: `vault script (72) ++ [1] ++ signature (64)` by the backup key
//! - pending coin, complete: `pending terms (73) ++ [0] ++ output (u16)`, no signature
//! - pending coin, recover: `pending terms (73) ++ [1] ++ signature (64)` by the backup key

use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use crate::serialize::tagged_hash;
use crate::transaction::{Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// Script version of a vault coin, and of a coin on its way out of a vault.
pub const SCRIPT_VERSION_VAULT: u8 = AddressType::Vault as u8;
pub const SCRIPT_VERSION_VAULT_PENDING: u8 = AddressType::VaultPending as u8;
/// The most a withdrawal step may leave behind as its fee.
pub const MAX_VAULT_FEE_ATOMS: u64 = 1_000_000;

const VAULT_CONTEXT: &str = "IMN 2026 vault script";
const PENDING_CONTEXT: &str = "IMN 2026 vault withdrawal";
const VAULT_BYTES: usize = 32 + 32 + 8;
const PENDING_BYTES: usize = 1 + 32 + 8 + 32;
const MAIN: u8 = 0;
const RECOVER: u8 = 1;

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidVault(reason.to_string())
}

fn verify(key: &[u8; 32], tx: &Transaction, network: Network, input_index: usize, signature: &[u8]) -> Result<(), TransactionError> {
    let signature: [u8; 64] = signature.try_into().map_err(|_| invalid("malformed signature"))?;
    VerifyingKey::from_bytes(key)
        .and_then(|key| key.verify_strict(tx.sig_hash(network, input_index).as_bytes(), &Signature::from_bytes(&signature)))
        .map_err(|e| TransactionError::InvalidSignature(e.to_string()))
}

/// The terms of a vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultScript {
    /// Starts withdrawals.
    pub everyday: [u8; 32],
    /// Stops withdrawals and can move the coins directly. Keep it offline.
    pub backup: [u8; 32],
    /// How long a withdrawal waits, in blue score: about 17,280 a day.
    pub delay: u64,
}

impl VaultScript {
    pub fn new(everyday: [u8; 32], backup: [u8; 32], delay: u64) -> Result<Self, TransactionError> {
        if VerifyingKey::from_bytes(&everyday).is_err() || VerifyingKey::from_bytes(&backup).is_err() {
            return Err(invalid("both keys must be valid Ed25519 public keys"));
        }
        if everyday == backup {
            return Err(invalid("the everyday and backup keys must differ"));
        }
        if delay == 0 {
            return Err(invalid("a vault needs a delay"));
        }
        Ok(Self { everyday, backup, delay })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        [self.everyday.as_slice(), self.backup.as_slice(), &self.delay.to_be_bytes()].concat()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        if bytes.len() != VAULT_BYTES {
            return Err(invalid("a vault script is 72 bytes"));
        }
        Self::new(bytes[..32].try_into().expect("32"), bytes[32..64].try_into().expect("32"), u64::from_be_bytes(bytes[64..].try_into().expect("8")))
    }

    pub fn locking_script(&self) -> ScriptPublicKey {
        ScriptPublicKey { version: SCRIPT_VERSION_VAULT, script: tagged_hash(VAULT_CONTEXT, &[&self.to_bytes()]).0.to_vec() }
    }

    pub fn address(&self, network: Network) -> Address {
        Address::new(network, AddressType::Vault, Hash(self.locking_script().script.try_into().expect("32 bytes")))
    }
}

/// A withdrawal on its way out of a vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingWithdrawal {
    /// Where the coins go when the withdrawal completes.
    pub destination: ScriptPublicKey,
    /// The blue score from which it may complete.
    pub unlock_at: u64,
    /// The vault's backup key, which can stop it until then.
    pub backup: [u8; 32],
}

impl PendingWithdrawal {
    pub fn to_bytes(&self) -> Vec<u8> {
        [&[self.destination.version][..], &self.destination.script, &self.unlock_at.to_be_bytes(), &self.backup].concat()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        if bytes.len() != PENDING_BYTES {
            return Err(invalid("a pending withdrawal is 73 bytes"));
        }
        Ok(Self {
            destination: ScriptPublicKey { version: bytes[0], script: bytes[1..33].to_vec() },
            unlock_at: u64::from_be_bytes(bytes[33..41].try_into().expect("8")),
            backup: bytes[41..].try_into().expect("32"),
        })
    }

    pub fn locking_script(&self) -> ScriptPublicKey {
        ScriptPublicKey { version: SCRIPT_VERSION_VAULT_PENDING, script: tagged_hash(PENDING_CONTEXT, &[&self.to_bytes()]).0.to_vec() }
    }
}

impl Transaction {
    /// Checks the signature, timing and destination of an input spending a vault coin or a
    /// pending withdrawal, in a block of the given blue score. `spent` is the coin it spends.
    pub(crate) fn verify_vault_input(&self, network: Network, input_index: usize, spent: &ScriptPublicKey, spending_blue_score: u64) -> Result<(), TransactionError> {
        let bytes = &self.inputs.get(input_index).ok_or(TransactionError::EmptyInputs)?.signature_script;
        if spent.version == SCRIPT_VERSION_VAULT {
            if bytes.len() < VAULT_BYTES + 65 {
                return Err(invalid("malformed signature script"));
            }
            let vault = VaultScript::from_bytes(&bytes[..VAULT_BYTES])?;
            if vault.locking_script() != *spent {
                return Err(TransactionError::PublicKeyAddressMismatch);
            }
            let signature = &bytes[VAULT_BYTES + 1..VAULT_BYTES + 65];
            match (bytes[VAULT_BYTES], bytes.len()) {
                (RECOVER, len) if len == VAULT_BYTES + 65 => verify(&vault.backup, self, network, input_index, signature),
                (MAIN, len) if len == VAULT_BYTES + 65 + PENDING_BYTES + 2 => {
                    verify(&vault.everyday, self, network, input_index, signature)?;
                    let pending = PendingWithdrawal::from_bytes(&bytes[VAULT_BYTES + 65..VAULT_BYTES + 65 + PENDING_BYTES])?;
                    if pending.backup != vault.backup {
                        return Err(invalid("a withdrawal can be stopped by the vault's own backup key"));
                    }
                    if pending.unlock_at < spending_blue_score.saturating_add(vault.delay) {
                        return Err(invalid("a withdrawal waits at least the vault's delay"));
                    }
                    Ok(())
                }
                _ => Err(invalid("malformed signature script")),
            }
        } else {
            if bytes.len() < PENDING_BYTES + 1 {
                return Err(invalid("malformed signature script"));
            }
            let pending = PendingWithdrawal::from_bytes(&bytes[..PENDING_BYTES])?;
            if pending.locking_script() != *spent {
                return Err(TransactionError::PublicKeyAddressMismatch);
            }
            match (bytes[PENDING_BYTES], bytes.len() - PENDING_BYTES - 1) {
                (RECOVER, 64) => verify(&pending.backup, self, network, input_index, &bytes[PENDING_BYTES + 1..]),
                (MAIN, 2) if spending_blue_score >= pending.unlock_at => Ok(()),
                (MAIN, 2) => Err(invalid("the withdrawal is still waiting")),
                _ => Err(invalid("malformed signature script")),
            }
        }
    }

    /// Checks where a withdrawal's coins go: starting one moves everything into the pending
    /// output it names, and completing one pays everything to its destination. A capped fee
    /// may come out each time. `spent` holds the outputs spent, in input order.
    /// Returns the outputs the withdrawals claim.
    pub fn check_vault_withdrawals(&self, spent: &[&TxOutput]) -> Result<Vec<u16>, TransactionError> {
        let mut named: Vec<u16> = Vec::new();
        for (index, coin) in spent.iter().enumerate() {
            let version = coin.script_public_key.version;
            if version != SCRIPT_VERSION_VAULT && version != SCRIPT_VERSION_VAULT_PENDING {
                continue;
            }
            let bytes = &self.inputs.get(index).ok_or(TransactionError::EmptyInputs)?.signature_script;
            let malformed = || invalid("malformed signature script");
            let index_at = |from: usize| -> Result<u16, TransactionError> {
                Ok(u16::from_be_bytes(bytes.get(from..from + 2).ok_or_else(malformed)?.try_into().expect("2 bytes")))
            };
            let (destination, at) = if version == SCRIPT_VERSION_VAULT {
                if bytes.get(VAULT_BYTES) != Some(&MAIN) {
                    continue; // Recovery by the backup key: anywhere
                }
                let terms = bytes.get(VAULT_BYTES + 65..VAULT_BYTES + 65 + PENDING_BYTES).ok_or_else(malformed)?;
                (PendingWithdrawal::from_bytes(terms)?.locking_script(), index_at(VAULT_BYTES + 65 + PENDING_BYTES)?)
            } else {
                if bytes.get(PENDING_BYTES) != Some(&MAIN) {
                    continue;
                }
                let terms = bytes.get(..PENDING_BYTES).ok_or_else(malformed)?;
                (PendingWithdrawal::from_bytes(terms)?.destination, index_at(PENDING_BYTES + 1)?)
            };
            if named.contains(&at) {
                return Err(invalid("an output may carry only one vault withdrawal"));
            }
            named.push(at);
            let output = self.outputs.get(at as usize).ok_or_else(|| invalid("the withdrawal names an output that does not exist"))?;
            if output.script_public_key != destination
                || output.value_atoms.saturating_add(MAX_VAULT_FEE_ATOMS) < coin.value_atoms
                || output.token != coin.token
            {
                return Err(invalid("a withdrawal's coins go only where it says"));
            }
        }
        Ok(named)
    }

    /// Starts a withdrawal of a vault coin to `to`, with the everyday key. It completes after the
    /// vault's delay, counted from `now` (the current blue score; the node refuses it if `now`
    /// is behind the block it lands in).
    #[allow(clippy::too_many_arguments)]
    pub fn start_vault_withdrawal(
        vault: &VaultScript,
        network: Network,
        coin: (Outpoint, TxOutput),
        everyday: &SigningKey,
        to: &Address,
        now: u64,
        fee_atoms: u64,
    ) -> Result<(Self, PendingWithdrawal), String> {
        if everyday.verifying_key().to_bytes() != vault.everyday {
            return Err("That is not the vault's everyday key".to_string());
        }
        // A few blocks of margin, so the transaction stays valid if it lands a little later
        let pending = PendingWithdrawal { destination: ScriptPublicKey::pay_to_address(to), unlock_at: now.saturating_add(vault.delay).saturating_add(10), backup: vault.backup };
        let (outpoint, held) = coin;
        let value = held.value_atoms.checked_sub(fee_atoms).ok_or("The fee is more than the vault coin holds")?;
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new() }],
            outputs: vec![TxOutput { value_atoms: value, script_public_key: pending.locking_script(), token: held.token }],
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        let signature = everyday.sign(tx.sig_hash(network, 0).as_bytes());
        tx.inputs[0].signature_script = [vault.to_bytes(), vec![MAIN], signature.to_bytes().to_vec(), pending.to_bytes(), 0u16.to_be_bytes().to_vec()].concat();
        Ok((tx, pending))
    }

    /// Completes a withdrawal once it has waited: anyone may send this; the coins can only go
    /// to the destination it names.
    pub fn complete_vault_withdrawal(pending: &PendingWithdrawal, coin: (Outpoint, TxOutput), fee_atoms: u64) -> Result<Self, String> {
        let (outpoint, held) = coin;
        let value = held.value_atoms.checked_sub(fee_atoms).ok_or("The fee is more than the withdrawal holds")?;
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new() }],
            outputs: vec![TxOutput { value_atoms: value, script_public_key: pending.destination.clone(), token: held.token }],
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        tx.inputs[0].signature_script = [pending.to_bytes(), vec![MAIN], 0u16.to_be_bytes().to_vec()].concat();
        Ok(tx)
    }

    /// Moves a vault coin, or stops a pending withdrawal, with the backup key: everything goes
    /// to `to`, less the fee. `pending` is given when stopping a withdrawal.
    pub fn recover_vault(
        vault: &VaultScript,
        pending: Option<&PendingWithdrawal>,
        network: Network,
        coin: (Outpoint, TxOutput),
        backup: &SigningKey,
        to: &Address,
        fee_atoms: u64,
    ) -> Result<Self, String> {
        if backup.verifying_key().to_bytes() != vault.backup {
            return Err("That is not the vault's backup key".to_string());
        }
        let (outpoint, held) = coin;
        let value = held.value_atoms.checked_sub(fee_atoms).ok_or("The fee is more than the coin holds")?;
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new() }],
            outputs: vec![TxOutput { value_atoms: value, script_public_key: ScriptPublicKey::pay_to_address(to), token: held.token }],
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        let signature = backup.sign(tx.sig_hash(network, 0).as_bytes()).to_bytes().to_vec();
        tx.inputs[0].signature_script = match pending {
            None => [vault.to_bytes(), vec![RECOVER], signature].concat(),
            Some(pending) => [pending.to_bytes(), vec![RECOVER], signature].concat(),
        };
        Ok(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET: Network = Network::Testnet;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn setup() -> (SigningKey, SigningKey, VaultScript, (Outpoint, TxOutput), Address) {
        let (everyday, backup) = (key(1), key(2));
        let vault = VaultScript::new(everyday.verifying_key().to_bytes(), backup.verifying_key().to_bytes(), 100).unwrap();
        let coin = (Outpoint { transaction_id: Hash([9u8; 32]), index: 0 }, TxOutput::new(5_000_000, vault.locking_script()));
        let shop = Address::from_public_key(NET, AddressType::PubKeyHash, &[7u8; 32]);
        (everyday, backup, vault, coin, shop)
    }

    fn valid(tx: &Transaction, spent: &TxOutput, blue_score: u64) -> Result<(), String> {
        tx.verify_input_at(NET, 0, &spent.script_public_key, blue_score).map_err(|e| e.to_string())?;
        tx.check_vault_withdrawals(&[spent]).map(|_| ()).map_err(|e| e.to_string())
    }

    #[test]
    fn a_withdrawal_waits_then_reaches_only_its_destination() {
        let (everyday, _, vault, coin, shop) = setup();
        let (start, pending) = Transaction::start_vault_withdrawal(&vault, NET, coin.clone(), &everyday, &shop, 50, 1_000).unwrap();
        valid(&start, &coin.1, 50).unwrap();
        // Landing so late that the wait would be shorter than the delay is refused
        assert!(valid(&start, &coin.1, 100).is_err());

        let pending_coin = (Outpoint { transaction_id: start.id(), index: 0 }, start.outputs[0].clone());
        let complete = Transaction::complete_vault_withdrawal(&pending, pending_coin.clone(), 1_000).unwrap();
        assert!(valid(&complete, &pending_coin.1, pending.unlock_at - 1).unwrap_err().contains("waiting"));
        valid(&complete, &pending_coin.1, pending.unlock_at).unwrap();
        assert_eq!(complete.outputs[0].script_public_key, ScriptPublicKey::pay_to_address(&shop));

        // Completing to anywhere else is refused, even with no signature to forge
        let mut diverted = complete.clone();
        diverted.outputs[0].script_public_key = ScriptPublicKey::pay_to_address(&Address::from_public_key(NET, AddressType::PubKeyHash, &[6u8; 32]));
        assert!(valid(&diverted, &pending_coin.1, pending.unlock_at).is_err());
    }

    #[test]
    fn the_everyday_key_cannot_send_directly_or_shorten_the_wait() {
        let (everyday, _, vault, coin, shop) = setup();
        let (start, _) = Transaction::start_vault_withdrawal(&vault, NET, coin.clone(), &everyday, &shop, 50, 1_000).unwrap();
        let resign = |mut tx: Transaction| {
            let signature = everyday.sign(tx.sig_hash(NET, 0).as_bytes());
            tx.inputs[0].signature_script[VAULT_BYTES + 1..VAULT_BYTES + 65].copy_from_slice(&signature.to_bytes());
            tx
        };
        // Straight to the shop, skipping the wait
        let mut direct = start.clone();
        direct.outputs[0].script_public_key = ScriptPublicKey::pay_to_address(&shop);
        assert!(valid(&resign(direct), &coin.1, 50).is_err());
        // A pending output that unlocks at once
        let quick = PendingWithdrawal { destination: ScriptPublicKey::pay_to_address(&shop), unlock_at: 51, backup: vault.backup };
        let mut short = start.clone();
        short.outputs[0].script_public_key = quick.locking_script();
        let end = short.inputs[0].signature_script.len();
        short.inputs[0].signature_script[VAULT_BYTES + 65..end - 2].copy_from_slice(&quick.to_bytes());
        assert!(valid(&resign(short), &coin.1, 50).is_err());
        // A pending output someone else's backup key controls
        let mut other = start.clone();
        let theirs = PendingWithdrawal { destination: ScriptPublicKey::pay_to_address(&shop), unlock_at: 500, backup: key(5).verifying_key().to_bytes() };
        other.outputs[0].script_public_key = theirs.locking_script();
        let end = other.inputs[0].signature_script.len();
        other.inputs[0].signature_script[VAULT_BYTES + 65..end - 2].copy_from_slice(&theirs.to_bytes());
        assert!(valid(&resign(other), &coin.1, 50).is_err());
    }

    #[test]
    fn the_backup_key_stops_a_withdrawal_or_moves_the_vault() {
        let (everyday, backup, vault, coin, shop) = setup();
        let safe = Address::from_public_key(NET, AddressType::PubKeyHash, backup.verifying_key().as_bytes());
        let (start, pending) = Transaction::start_vault_withdrawal(&vault, NET, coin.clone(), &everyday, &shop, 50, 1_000).unwrap();
        let pending_coin = (Outpoint { transaction_id: start.id(), index: 0 }, start.outputs[0].clone());
        let stop = Transaction::recover_vault(&vault, Some(&pending), NET, pending_coin.clone(), &backup, &safe, 1_000).unwrap();
        valid(&stop, &pending_coin.1, 60).unwrap();
        assert!(Transaction::recover_vault(&vault, Some(&pending), NET, pending_coin, &everyday, &safe, 1_000).is_err());

        let direct = Transaction::recover_vault(&vault, None, NET, coin.clone(), &backup, &safe, 1_000).unwrap();
        valid(&direct, &coin.1, 0).unwrap();
    }
}
