//! Subscriptions: a customer sets money aside that a merchant may collect from, at most a set
//! amount once per period, and the customer can take back whenever they like.
//!
//! A subscription address locks IMN to the hash of a script naming the customer, the merchant,
//! the amount per period, the period, and when the next collection is due. Two things can happen
//! to the coins:
//!
//! - **The merchant collects**, with their signature, once the due time has come. At most the
//!   amount per period, plus a fee of at most `MAX_SUBSCRIPTION_FEE_ATOMS`, may leave; the rest
//!   must go back into a subscription with the same terms and the next due time one period
//!   later. A merchant who misses a period can collect it later, one period at a time.
//! - **The customer cancels**, with their signature, at any time, and the coins go wherever the
//!   customer sends them.
//!
//! Subscriptions hold IMN only. The signature script of a spend is
//! `script (120 bytes) ++ role (1) ++ signature (64)`, and a collect names the output that
//! carries the rest: `++ rest output (u16)`, or `0xffff` when the last of the money is collected.

use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use crate::serialize::tagged_hash;
use crate::transaction::{Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// Script version of an output locked to a subscription.
pub const SCRIPT_VERSION_SUBSCRIPTION: u8 = AddressType::Subscription as u8;
/// The most a collection may leave behind as its fee, beyond the amount per period.
pub const MAX_SUBSCRIPTION_FEE_ATOMS: u64 = 1_000_000;

const SCRIPT_HASH_CONTEXT: &str = "IMN 2026 subscription script";
const SCRIPT_BYTES: usize = 32 + 32 + 8 + 8 + 8 + 32;
const COLLECT: u8 = 0;
const CANCEL: u8 = 1;
const NO_REST: u16 = 0xffff;

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidSubscription(reason.to_string())
}

/// The terms of a subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionScript {
    pub customer: [u8; 32],
    pub merchant: [u8; 32],
    /// The most the merchant may collect each period, in atoms.
    pub amount_per_period: u64,
    /// A period in blue score: about 17,280 a day, about 518,400 for thirty days.
    pub period: u64,
    /// The blue score from which the next collection may be made.
    pub next_due: u64,
    /// A reference the merchant chooses, such as the customer's account number, so two
    /// subscriptions with the same terms have different addresses.
    pub reference: [u8; 32],
}

impl SubscriptionScript {
    pub fn new(
        customer: [u8; 32],
        merchant: [u8; 32],
        amount_per_period: u64,
        period: u64,
        next_due: u64,
        reference: [u8; 32],
    ) -> Result<Self, TransactionError> {
        let script = Self { customer, merchant, amount_per_period, period, next_due, reference };
        script.check()?;
        Ok(script)
    }

    fn check(&self) -> Result<(), TransactionError> {
        if VerifyingKey::from_bytes(&self.customer).is_err() || VerifyingKey::from_bytes(&self.merchant).is_err() {
            return Err(invalid("the customer and merchant must be valid Ed25519 public keys"));
        }
        if self.customer == self.merchant {
            return Err(invalid("the customer and merchant must be different keys"));
        }
        if self.amount_per_period == 0 || self.period == 0 {
            return Err(invalid("the amount and the period must be positive"));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SCRIPT_BYTES);
        bytes.extend_from_slice(&self.customer);
        bytes.extend_from_slice(&self.merchant);
        bytes.extend_from_slice(&self.amount_per_period.to_be_bytes());
        bytes.extend_from_slice(&self.period.to_be_bytes());
        bytes.extend_from_slice(&self.next_due.to_be_bytes());
        bytes.extend_from_slice(&self.reference);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        if bytes.len() != SCRIPT_BYTES {
            return Err(invalid("a subscription script is 120 bytes"));
        }
        let key = |from: usize| -> [u8; 32] { bytes[from..from + 32].try_into().expect("32 bytes") };
        let number = |from: usize| u64::from_be_bytes(bytes[from..from + 8].try_into().expect("8 bytes"));
        Self::new(key(0), key(32), number(64), number(72), number(80), key(88))
    }

    /// The same subscription one period later: where the rest goes after a collection.
    pub fn next(&self) -> Result<Self, TransactionError> {
        let next_due = self.next_due.checked_add(self.period).ok_or_else(|| invalid("the due time overflows"))?;
        Ok(Self { next_due, ..self.clone() })
    }

    pub fn script_hash(&self) -> Hash {
        tagged_hash(SCRIPT_HASH_CONTEXT, &[&self.to_bytes()])
    }

    pub fn address(&self, network: Network) -> Address {
        Address::new(network, AddressType::Subscription, self.script_hash())
    }

    /// The script an output is locked with to hold this subscription.
    pub fn locking_script(&self) -> ScriptPublicKey {
        ScriptPublicKey { version: SCRIPT_VERSION_SUBSCRIPTION, script: self.script_hash().0.to_vec() }
    }

    pub fn merchant_script(&self, network: Network) -> ScriptPublicKey {
        ScriptPublicKey::pay_to_address(&Address::from_public_key(network, AddressType::PubKeyHash, &self.merchant))
    }
}

/// How an input spends a subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionSpend {
    Collect { rest: Option<u16> },
    Cancel,
}

impl Transaction {
    /// Reads a subscription spend from an input's signature script, without checking it.
    pub fn subscription_spend(&self, input_index: usize) -> Option<(SubscriptionScript, SubscriptionSpend)> {
        let bytes = &self.inputs.get(input_index)?.signature_script;
        if bytes.len() < SCRIPT_BYTES + 65 {
            return None;
        }
        let script = SubscriptionScript::from_bytes(&bytes[..SCRIPT_BYTES]).ok()?;
        let tail = &bytes[SCRIPT_BYTES + 65..];
        let spend = match (bytes[SCRIPT_BYTES], tail.len()) {
            (COLLECT, 2) => {
                let rest = u16::from_be_bytes([tail[0], tail[1]]);
                SubscriptionSpend::Collect { rest: (rest != NO_REST).then_some(rest) }
            }
            (CANCEL, 0) => SubscriptionSpend::Cancel,
            _ => return None,
        };
        Some((script, spend))
    }

    /// Checks the signature on an input that spends a subscription coin, in a block of the given
    /// blue score. What a collection may take is checked by `check_subscription_collections`.
    pub(crate) fn verify_subscription_input(
        &self,
        network: Network,
        input_index: usize,
        script_hash: &[u8],
        spending_blue_score: u64,
    ) -> Result<(), TransactionError> {
        let (script, spend) = self.subscription_spend(input_index).ok_or_else(|| invalid("malformed signature script"))?;
        if script.script_hash().0[..] != *script_hash {
            return Err(TransactionError::PublicKeyAddressMismatch);
        }
        let key = match spend {
            SubscriptionSpend::Collect { .. } => {
                if spending_blue_score < script.next_due {
                    return Err(invalid("the next collection is not due yet"));
                }
                script.merchant
            }
            SubscriptionSpend::Cancel => script.customer,
        };
        let bytes = &self.inputs[input_index].signature_script;
        let signature: [u8; 64] = bytes[SCRIPT_BYTES + 1..SCRIPT_BYTES + 65].try_into().expect("64 bytes");
        VerifyingKey::from_bytes(&key)
            .and_then(|key| key.verify_strict(self.sig_hash(network, input_index).as_bytes(), &Signature::from_bytes(&signature)))
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))
    }

    /// Checks every collection this transaction makes: at most the amount per period (and a
    /// capped fee) leaves, and the rest goes back into the subscription, due one period later.
    /// Returns the outputs the collections claim (their rests).
    pub fn check_subscription_collections(&self, spent: &[&TxOutput]) -> Result<Vec<u16>, TransactionError> {
        let mut named: Vec<u16> = Vec::new();
        for (index, coin) in spent.iter().enumerate() {
            if coin.script_public_key.version != SCRIPT_VERSION_SUBSCRIPTION {
                continue;
            }
            if coin.token.is_some() {
                return Err(invalid("subscriptions hold IMN only"));
            }
            let Some((script, SubscriptionSpend::Collect { rest })) = self.subscription_spend(index) else { continue };
            let allowance = script.amount_per_period.saturating_add(MAX_SUBSCRIPTION_FEE_ATOMS);
            match rest {
                None => {
                    if coin.value_atoms > allowance {
                        return Err(invalid("more than one period's amount is left: it must stay in the subscription"));
                    }
                }
                Some(i) => {
                    if named.contains(&i) {
                        return Err(invalid("an output may hold the rest of only one subscription"));
                    }
                    named.push(i);
                    let output = self.outputs.get(i as usize).ok_or_else(|| invalid("the collection names an output that does not exist"))?;
                    let next = script.next()?;
                    if output.script_public_key != next.locking_script() {
                        return Err(invalid("the rest must stay in the subscription, due one period later"));
                    }
                    if output.token.is_some() || output.value_atoms.saturating_add(allowance) < coin.value_atoms {
                        return Err(invalid("a collection takes at most one period's amount"));
                    }
                }
            }
        }
        Ok(named)
    }

    /// Collects one period's amount from a subscription coin, as the merchant. The rest goes
    /// back into the subscription, due one period later. The fee comes out of the subscription.
    pub fn collect_subscription(
        script: &SubscriptionScript,
        network: Network,
        coin: (Outpoint, TxOutput),
        merchant: &SigningKey,
        fee_atoms: u64,
    ) -> Result<Self, String> {
        if merchant.verifying_key().to_bytes() != script.merchant {
            return Err("That is not the merchant's key".to_string());
        }
        if fee_atoms > MAX_SUBSCRIPTION_FEE_ATOMS {
            return Err(format!("A collection may pay at most {} atoms in fee", MAX_SUBSCRIPTION_FEE_ATOMS));
        }
        let (outpoint, held) = coin;
        let take = held.value_atoms.min(script.amount_per_period);
        let to_merchant = take.checked_sub(fee_atoms).filter(|a| *a > 0).ok_or("The fee takes the whole collection")?;
        let left = held.value_atoms - take;
        let mut outputs = vec![TxOutput::new(to_merchant, script.merchant_script(network))];
        if left > 0 {
            let next = script.next().map_err(|e| e.to_string())?;
            outputs.push(TxOutput::new(left, next.locking_script()));
        }
        let rest = if left > 0 { 1u16 } else { NO_REST };
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new() }],
            outputs,
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        let signature = merchant.sign(tx.sig_hash(network, 0).as_bytes());
        tx.inputs[0].signature_script = [script.to_bytes(), vec![COLLECT], signature.to_bytes().to_vec(), rest.to_be_bytes().to_vec()].concat();
        Ok(tx)
    }

    /// Cancels a subscription as the customer: everything it holds, less the fee, goes to `to`.
    pub fn cancel_subscription(
        script: &SubscriptionScript,
        network: Network,
        coin: (Outpoint, TxOutput),
        customer: &SigningKey,
        to: &Address,
        fee_atoms: u64,
    ) -> Result<Self, String> {
        if customer.verifying_key().to_bytes() != script.customer {
            return Err("That is not the customer's key".to_string());
        }
        let (outpoint, held) = coin;
        let back = held.value_atoms.checked_sub(fee_atoms).filter(|a| *a > 0).ok_or("The fee takes everything")?;
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new() }],
            outputs: vec![TxOutput::new(back, ScriptPublicKey::pay_to_address(to))],
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        let signature = customer.sign(tx.sig_hash(network, 0).as_bytes());
        tx.inputs[0].signature_script = [script.to_bytes(), vec![CANCEL], signature.to_bytes().to_vec()].concat();
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

    /// Customer 1 lets merchant 2 collect 1 IMN every 100 blocks, first due at 1,000.
    fn setup() -> (SigningKey, SigningKey, SubscriptionScript, (Outpoint, TxOutput)) {
        let (customer, merchant) = (key(1), key(2));
        let script = SubscriptionScript::new(
            customer.verifying_key().to_bytes(),
            merchant.verifying_key().to_bytes(),
            100_000_000,
            100,
            1_000,
            [7u8; 32],
        )
        .unwrap();
        let coin = (Outpoint { transaction_id: Hash([9u8; 32]), index: 0 }, TxOutput::new(350_000_000, script.locking_script()));
        (customer, merchant, script, coin)
    }

    fn valid(tx: &Transaction, coin: &TxOutput, blue_score: u64) -> Result<(), String> {
        tx.verify_input_at(NET, 0, &coin.script_public_key, blue_score).map_err(|e| e.to_string())?;
        tx.check_subscription_collections(&[coin]).map(|_| ()).map_err(|e| e.to_string())
    }

    #[test]
    fn the_merchant_collects_one_period_at_a_time() {
        let (_, merchant, script, coin) = setup();
        let collect = Transaction::collect_subscription(&script, NET, coin.clone(), &merchant, 10_000).unwrap();
        assert!(valid(&collect, &coin.1, 999).unwrap_err().contains("not due"));
        valid(&collect, &coin.1, 1_000).unwrap();
        assert_eq!(collect.outputs[0].value_atoms, 99_990_000);
        assert_eq!(collect.outputs[1].value_atoms, 250_000_000);
        assert_eq!(collect.outputs[1].script_public_key, script.next().unwrap().locking_script());

        // The next collection from the rest is due one period later
        let next = script.next().unwrap();
        let rest = (Outpoint { transaction_id: collect.id(), index: 1 }, collect.outputs[1].clone());
        let again = Transaction::collect_subscription(&next, NET, rest.clone(), &merchant, 10_000).unwrap();
        assert!(valid(&again, &rest.1, 1_050).is_err());
        valid(&again, &rest.1, 1_100).unwrap();
    }

    #[test]
    fn the_merchant_cannot_take_more_or_move_the_due_date() {
        let (_, merchant, script, coin) = setup();
        let honest = Transaction::collect_subscription(&script, NET, coin.clone(), &merchant, 10_000).unwrap();
        let resign = |mut tx: Transaction| {
            let signature = merchant.sign(tx.sig_hash(NET, 0).as_bytes());
            tx.inputs[0].signature_script[SCRIPT_BYTES + 1..SCRIPT_BYTES + 65].copy_from_slice(&signature.to_bytes());
            tx
        };
        // Two periods' worth at once
        let mut greedy = honest.clone();
        greedy.outputs[0].value_atoms += 100_000_000;
        greedy.outputs[1].value_atoms -= 100_000_000;
        assert!(valid(&resign(greedy), &coin.1, 1_000).is_err());
        // The rest put back with no next due date, so it could be collected again at once
        let mut same_due = honest.clone();
        same_due.outputs[1].script_public_key = script.locking_script();
        assert!(valid(&resign(same_due), &coin.1, 1_000).is_err());
        // Claiming it was the last of the money
        let mut last = honest.clone();
        last.inputs[0].signature_script.truncate(SCRIPT_BYTES + 65);
        last.inputs[0].signature_script.extend_from_slice(&NO_REST.to_be_bytes());
        last.outputs.truncate(1);
        last.outputs[0].value_atoms = 349_990_000;
        assert!(valid(&resign(last), &coin.1, 1_000).is_err());
        // The customer's key cannot collect
        assert!(Transaction::collect_subscription(&script, NET, coin, &key(1), 10_000).is_err());
    }

    #[test]
    fn the_last_period_empties_it_and_the_customer_can_cancel_any_time() {
        let (customer, merchant, script, _) = setup();
        let small = (Outpoint { transaction_id: Hash([8u8; 32]), index: 0 }, TxOutput::new(60_000_000, script.locking_script()));
        let last = Transaction::collect_subscription(&script, NET, small.clone(), &merchant, 10_000).unwrap();
        assert_eq!(last.outputs.len(), 1);
        valid(&last, &small.1, 5_000).unwrap();

        let (_, _, _, coin) = setup();
        let home = Address::from_public_key(NET, AddressType::PubKeyHash, customer.verifying_key().as_bytes());
        let cancel = Transaction::cancel_subscription(&script, NET, coin.clone(), &customer, &home, 10_000).unwrap();
        valid(&cancel, &coin.1, 0).unwrap();
        assert!(Transaction::cancel_subscription(&script, NET, coin, &merchant, &home, 10_000).is_err());
    }
}
