//! Escrow: coins held for a buyer and a seller, with an arbiter for disputes.
//!
//! An escrow address names three keys and a deadline. Each party's signature can move the
//! coins only to the place that party is allowed to send them, and every node checks the
//! destination, not just the signature:
//!
//! - the **buyer** can release the coins to the seller;
//! - the **seller** can refund them to the buyer;
//! - the **arbiter** can settle a dispute by paying the buyer, the seller, or both, but never
//!   anyone else;
//! - after the **deadline**, one side may collect alone: the seller (auto-release, as on most
//!   marketplaces) or the buyer (auto-refund), as the escrow's terms say.
//!
//! The escrowed amount may go only to the allowed destination or to the fee, and at most
//! `MAX_ESCROW_FEE_ATOMS` of it to the fee. Anything the transaction pays elsewhere must come
//! from its other inputs, so extra coins can be added to pay a larger fee but escrowed coins can
//! never be paid to anyone else. Any token the escrow holds must go to the allowed destination
//! too. Only one escrow coin may be spent per transaction, so one payment can never be counted
//! towards two escrows.
//!
//! The signature script of a spend is `script (105 bytes) ++ role (1) ++ signature (64)`.

use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use crate::serialize::tagged_hash;
use crate::transaction::{Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// Script version of an output locked to an escrow script.
pub const SCRIPT_VERSION_ESCROW: u8 = AddressType::Escrow as u8;
/// The most an escrow spend may leave behind as its fee.
pub const MAX_ESCROW_FEE_ATOMS: u64 = 1_000_000;

const SCRIPT_HASH_CONTEXT: &str = "IMN 2026 escrow script";
const SCRIPT_BYTES: usize = 32 * 3 + 8 + 1;
const SPEND_BYTES: usize = SCRIPT_BYTES + 1 + 64;

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidEscrow(reason.to_string())
}

/// Who is moving the coins out of an escrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscrowRole {
    /// The buyer, releasing the coins to the seller.
    Buyer = 0,
    /// The seller, refunding the coins to the buyer.
    Seller = 1,
    /// The arbiter, paying the buyer, the seller, or both.
    Arbiter = 2,
    /// After the deadline: the party the terms name, collecting alone.
    Deadline = 3,
}

impl EscrowRole {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Buyer),
            1 => Some(Self::Seller),
            2 => Some(Self::Arbiter),
            3 => Some(Self::Deadline),
            _ => None,
        }
    }
}

/// The terms of an escrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscrowScript {
    pub buyer: [u8; 32],
    pub seller: [u8; 32],
    pub arbiter: [u8; 32],
    /// From blocks with this blue score on, the deadline party may collect alone. One blue
    /// score is about five seconds, so 17,280 is about a day.
    pub deadline: u64,
    /// Who collects after the deadline: the seller when true, the buyer when false.
    pub seller_collects_after_deadline: bool,
}

impl EscrowScript {
    pub fn new(
        buyer: [u8; 32],
        seller: [u8; 32],
        arbiter: [u8; 32],
        deadline: u64,
        seller_collects_after_deadline: bool,
    ) -> Result<Self, TransactionError> {
        let script = Self { buyer, seller, arbiter, deadline, seller_collects_after_deadline };
        script.check()?;
        Ok(script)
    }

    fn check(&self) -> Result<(), TransactionError> {
        let keys = [self.buyer, self.seller, self.arbiter];
        if keys.iter().any(|key| VerifyingKey::from_bytes(key).is_err()) {
            return Err(invalid("the buyer, seller and arbiter must be valid Ed25519 public keys"));
        }
        if self.buyer == self.seller || self.buyer == self.arbiter || self.seller == self.arbiter {
            return Err(invalid("the buyer, seller and arbiter must be three different keys"));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SCRIPT_BYTES);
        bytes.extend_from_slice(&self.buyer);
        bytes.extend_from_slice(&self.seller);
        bytes.extend_from_slice(&self.arbiter);
        bytes.extend_from_slice(&self.deadline.to_be_bytes());
        bytes.push(self.seller_collects_after_deadline as u8);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        if bytes.len() != SCRIPT_BYTES {
            return Err(invalid("an escrow script is 105 bytes"));
        }
        let key = |from: usize| -> [u8; 32] { bytes[from..from + 32].try_into().expect("32 bytes") };
        let seller_collects = match bytes[104] {
            0 => false,
            1 => true,
            _ => return Err(invalid("malformed deadline flag")),
        };
        Self::new(key(0), key(32), key(64), u64::from_be_bytes(bytes[96..104].try_into().expect("8 bytes")), seller_collects)
    }

    /// The hash an output is locked to.
    pub fn script_hash(&self) -> Hash {
        tagged_hash(SCRIPT_HASH_CONTEXT, &[&self.to_bytes()])
    }

    pub fn address(&self, network: Network) -> Address {
        Address::new(network, AddressType::Escrow, self.script_hash())
    }

    fn party_script(key: &[u8; 32], network: Network) -> ScriptPublicKey {
        ScriptPublicKey::pay_to_address(&Address::from_public_key(network, AddressType::PubKeyHash, key))
    }

    pub fn buyer_script(&self, network: Network) -> ScriptPublicKey {
        Self::party_script(&self.buyer, network)
    }

    pub fn seller_script(&self, network: Network) -> ScriptPublicKey {
        Self::party_script(&self.seller, network)
    }

    /// The key that signs for a role, and where that role may send the coins.
    fn role_terms(&self, role: EscrowRole, network: Network) -> ([u8; 32], Vec<ScriptPublicKey>) {
        match role {
            EscrowRole::Buyer => (self.buyer, vec![self.seller_script(network)]),
            EscrowRole::Seller => (self.seller, vec![self.buyer_script(network)]),
            EscrowRole::Arbiter => (self.arbiter, vec![self.buyer_script(network), self.seller_script(network)]),
            EscrowRole::Deadline if self.seller_collects_after_deadline => (self.seller, vec![self.seller_script(network)]),
            EscrowRole::Deadline => (self.buyer, vec![self.buyer_script(network)]),
        }
    }
}

impl Transaction {
    /// Reads an escrow spend from an input's signature script, without checking it.
    pub fn escrow_spend(&self, input_index: usize) -> Option<(EscrowScript, EscrowRole)> {
        let bytes = &self.inputs.get(input_index)?.signature_script;
        if bytes.len() != SPEND_BYTES {
            return None;
        }
        Some((EscrowScript::from_bytes(&bytes[..SCRIPT_BYTES]).ok()?, EscrowRole::from_u8(bytes[SCRIPT_BYTES])?))
    }

    /// Checks the signature on an input that spends an escrow coin, in a block of the given
    /// blue score. Where the coins go is checked by `check_escrow_destinations`.
    pub(crate) fn verify_escrow_input(
        &self,
        network: Network,
        input_index: usize,
        script_hash: &[u8],
        spending_blue_score: u64,
    ) -> Result<(), TransactionError> {
        let (script, role) = self.escrow_spend(input_index).ok_or_else(|| invalid("malformed signature script"))?;
        if script.script_hash().0[..] != *script_hash {
            return Err(TransactionError::PublicKeyAddressMismatch);
        }
        if role == EscrowRole::Deadline && spending_blue_score < script.deadline {
            return Err(invalid("the deadline has not passed"));
        }
        let (key, _) = script.role_terms(role, network);
        let signature: [u8; 64] = self.inputs[input_index].signature_script[SCRIPT_BYTES + 1..].try_into().expect("64 bytes");
        VerifyingKey::from_bytes(&key)
            .and_then(|key| key.verify_strict(self.sig_hash(network, input_index).as_bytes(), &Signature::from_bytes(&signature)))
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))
    }

    /// Checks that the coins of every escrow this transaction spends go where the signer's role
    /// allows. `spent` holds the outputs the transaction spends, in input order.
    pub fn check_escrow_destinations(&self, network: Network, spent: &[&TxOutput]) -> Result<(), TransactionError> {
        let escrows: Vec<usize> =
            spent.iter().enumerate().filter(|(_, o)| o.script_public_key.version == SCRIPT_VERSION_ESCROW).map(|(i, _)| i).collect();
        let Some(&index) = escrows.first() else { return Ok(()) };
        if escrows.len() > 1 {
            return Err(invalid("a transaction spends at most one escrow coin"));
        }
        let (script, role) = self.escrow_spend(index).ok_or_else(|| invalid("malformed signature script"))?;
        let (_, allowed) = script.role_terms(role, network);
        let escrowed = spent[index];

        let to_allowed = self.outputs.iter().filter(|o| allowed.contains(&o.script_public_key));
        let atoms: u64 = to_allowed.clone().fold(0u64, |sum, o| sum.saturating_add(o.value_atoms));
        if atoms.saturating_add(MAX_ESCROW_FEE_ATOMS) < escrowed.value_atoms {
            return Err(invalid("the escrowed coins must go to the party this signer may pay"));
        }
        // Whatever is paid anywhere else must be covered by the transaction's other inputs
        let elsewhere: u64 = self
            .outputs
            .iter()
            .filter(|o| !allowed.contains(&o.script_public_key))
            .fold(0u64, |sum, o| sum.saturating_add(o.value_atoms));
        let other_inputs: u64 =
            spent.iter().enumerate().filter(|(i, _)| *i != index).fold(0u64, |sum, (_, o)| sum.saturating_add(o.value_atoms));
        if elsewhere > other_inputs {
            return Err(invalid("escrowed coins can only go to the party this signer may pay, or to the fee"));
        }
        if let Some(token) = &escrowed.token {
            let carried: u64 = to_allowed
                .filter_map(|o| o.token.as_ref())
                .filter(|t| t.id == token.id && t.mint_authority == token.mint_authority)
                .fold(0u64, |sum, t| sum.saturating_add(t.amount.max(t.mint_authority as u64)));
            if carried < token.amount.max(token.mint_authority as u64) {
                return Err(invalid("the escrowed tokens must go to the party this signer may pay"));
            }
        }
        Ok(())
    }

    /// Moves an escrow coin as `role`, signed with that role's key. Pays the whole coin, less
    /// the fee, to the one place the role allows; for the arbiter, pass `to_buyer_atoms` to
    /// split, with the rest going to the seller. A token the coin holds goes with the larger
    /// share, or to the seller on an even split.
    pub fn spend_escrow(
        script: &EscrowScript,
        network: Network,
        coin: (Outpoint, TxOutput),
        role: EscrowRole,
        key: &SigningKey,
        fee_atoms: u64,
        to_buyer_atoms: Option<u64>,
    ) -> Result<Self, String> {
        if fee_atoms > MAX_ESCROW_FEE_ATOMS {
            return Err(format!("An escrow spend may pay at most {} atoms in fee", MAX_ESCROW_FEE_ATOMS));
        }
        let (signer, allowed) = script.role_terms(role, network);
        if key.verifying_key().to_bytes() != signer {
            return Err("That key is not the one this role of the escrow names".to_string());
        }
        let (outpoint, escrowed) = coin;
        let total = escrowed.value_atoms.checked_sub(fee_atoms).ok_or("The fee exceeds the escrowed amount")?;

        let mut outputs = Vec::new();
        match (role, to_buyer_atoms) {
            (EscrowRole::Arbiter, Some(to_buyer)) => {
                let to_buyer = to_buyer.min(total);
                let to_seller = total - to_buyer;
                let token_to_buyer = to_buyer > to_seller;
                for (atoms, destination, takes_token) in
                    [(to_buyer, script.buyer_script(network), token_to_buyer), (to_seller, script.seller_script(network), !token_to_buyer)]
                {
                    let token = if takes_token { escrowed.token.clone() } else { None };
                    if atoms > 0 || token.is_some() {
                        outputs.push(TxOutput { value_atoms: atoms, script_public_key: destination, token });
                    }
                }
            }
            (_, Some(_)) => return Err("Only the arbiter can split an escrow".to_string()),
            (_, None) => {
                outputs.push(TxOutput { value_atoms: total, script_public_key: allowed[0].clone(), token: escrowed.token.clone() });
            }
        }
        if outputs.is_empty() {
            return Err("The fee takes everything in the escrow".to_string());
        }

        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput { previous_outpoint: outpoint, signature_script: Vec::new() }],
            outputs,
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        let signature = key.sign(tx.sig_hash(network, 0).as_bytes());
        let mut signature_script = script.to_bytes();
        signature_script.push(role as u8);
        signature_script.extend_from_slice(&signature.to_bytes());
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

    const NET: Network = Network::Testnet;

    fn setup(seller_collects: bool) -> (SigningKey, SigningKey, SigningKey, EscrowScript) {
        let (buyer, seller, arbiter) = (key(1), key(2), key(3));
        let script = EscrowScript::new(
            buyer.verifying_key().to_bytes(),
            seller.verifying_key().to_bytes(),
            arbiter.verifying_key().to_bytes(),
            1_000,
            seller_collects,
        )
        .unwrap();
        (buyer, seller, arbiter, script)
    }

    fn coin(script: &EscrowScript) -> (Outpoint, TxOutput) {
        (Outpoint { transaction_id: Hash([9u8; 32]), index: 0 }, TxOutput::new(10_000_000, ScriptPublicKey::pay_to_address(&script.address(NET))))
    }

    /// Signature and destination, as a node checks them, at `blue_score`.
    fn valid(tx: &Transaction, script: &EscrowScript, blue_score: u64) -> bool {
        let escrowed = coin(script).1;
        tx.verify_input_at(NET, 0, &escrowed.script_public_key, blue_score).is_ok() && tx.check_escrow_destinations(NET, &[&escrowed]).is_ok()
    }

    #[test]
    fn script_round_trips_and_parties_must_differ() {
        let (_, _, _, script) = setup(true);
        assert_eq!(EscrowScript::from_bytes(&script.to_bytes()).unwrap(), script);
        assert!(EscrowScript::new(script.buyer, script.buyer, script.arbiter, 5, true).is_err());
        assert_eq!(coin(&script).1.script_public_key.version, SCRIPT_VERSION_ESCROW);
    }

    #[test]
    fn each_party_can_pay_only_where_its_role_allows() {
        let (buyer, seller, arbiter, script) = setup(true);
        let release = Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Buyer, &buyer, 1_000, None).unwrap();
        assert_eq!(release.outputs[0].script_public_key, script.seller_script(NET));
        assert!(valid(&release, &script, 0));
        let refund = Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Seller, &seller, 1_000, None).unwrap();
        assert_eq!(refund.outputs[0].script_public_key, script.buyer_script(NET));
        assert!(valid(&refund, &script, 0));
        let split = Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Arbiter, &arbiter, 1_000, Some(4_000_000)).unwrap();
        assert_eq!(split.outputs.len(), 2);
        assert!(valid(&split, &script, 0));

        // The wrong key for a role
        assert!(Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Buyer, &seller, 1_000, None).is_err());
        // Only the arbiter splits
        assert!(Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Seller, &seller, 1_000, Some(5)).is_err());
    }

    #[test]
    fn a_signer_cannot_redirect_the_coins() {
        let (buyer, _, arbiter, script) = setup(true);
        let thief = ScriptPublicKey::pay_to_address(&Address::from_public_key(NET, AddressType::PubKeyHash, &[7u8; 32]));
        for (role, signer) in [(EscrowRole::Buyer, &buyer), (EscrowRole::Arbiter, &arbiter)] {
            let mut tx = Transaction::spend_escrow(&script, NET, coin(&script), role, signer, 1_000, None).unwrap();
            // Re-pointed after the fact: the signature no longer matches
            tx.outputs[0].script_public_key = thief.clone();
            assert!(!valid(&tx, &script, 0));
            // Re-pointed and re-signed by the same party: the destination rule refuses it
            let signature = signer.sign(tx.sig_hash(NET, 0).as_bytes());
            tx.inputs[0].signature_script[SCRIPT_BYTES + 1..].copy_from_slice(&signature.to_bytes());
            assert!(tx.verify_input_at(NET, 0, &coin(&script).1.script_public_key, 0).is_ok());
            assert!(tx.check_escrow_destinations(NET, &[&coin(&script).1]).is_err());
        }
        // Even when the escrow holds less than the fee cap
        let small = (coin(&script).0, TxOutput::new(50_000, coin(&script).1.script_public_key));
        let mut sneaky = Transaction::spend_escrow(&script, NET, small.clone(), EscrowRole::Arbiter, &arbiter, 1_000, None).unwrap();
        sneaky.outputs[0].script_public_key = thief.clone();
        assert!(sneaky.check_escrow_destinations(NET, &[&small.1]).is_err());
        // Paying a third party from the signer's own extra coin is fine
        let mut topped_up = Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Buyer, &buyer, 1_000, None).unwrap();
        topped_up.outputs.push(TxOutput::new(30_000, thief.clone()));
        let extra = TxOutput::new(40_000, thief.clone());
        assert!(topped_up.check_escrow_destinations(NET, &[&coin(&script).1, &extra]).is_ok());
        // Nor burn most of it as a fee
        let mut greedy = Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Buyer, &buyer, 1_000, None).unwrap();
        greedy.outputs[0].value_atoms = 1_000;
        assert!(greedy.check_escrow_destinations(NET, &[&coin(&script).1]).is_err());
        assert!(Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Buyer, &buyer, MAX_ESCROW_FEE_ATOMS + 1, None).is_err());
    }

    #[test]
    fn after_the_deadline_the_named_party_collects_alone() {
        let (buyer, seller, _, auto_release) = setup(true);
        let collect = Transaction::spend_escrow(&auto_release, NET, coin(&auto_release), EscrowRole::Deadline, &seller, 1_000, None).unwrap();
        assert_eq!(collect.outputs[0].script_public_key, auto_release.seller_script(NET));
        assert!(!valid(&collect, &auto_release, 999));
        assert!(valid(&collect, &auto_release, 1_000));
        assert!(Transaction::spend_escrow(&auto_release, NET, coin(&auto_release), EscrowRole::Deadline, &buyer, 1_000, None).is_err());

        let (buyer, _, _, auto_refund) = setup(false);
        let reclaim = Transaction::spend_escrow(&auto_refund, NET, coin(&auto_refund), EscrowRole::Deadline, &buyer, 1_000, None).unwrap();
        assert_eq!(reclaim.outputs[0].script_public_key, auto_refund.buyer_script(NET));
        assert!(!valid(&reclaim, &auto_refund, 10));
        assert!(valid(&reclaim, &auto_refund, 2_000));
        // Without a block height, nothing time-locked has matured
        assert!(reclaim.verify_input(NET, 0, &coin(&auto_refund).1.script_public_key).is_err());
    }

    #[test]
    fn one_escrow_per_transaction() {
        let (buyer, _, _, script) = setup(true);
        let mut tx = Transaction::spend_escrow(&script, NET, coin(&script), EscrowRole::Buyer, &buyer, 1_000, None).unwrap();
        tx.inputs.push(tx.inputs[0].clone());
        let escrowed = coin(&script).1;
        assert!(tx.check_escrow_destinations(NET, &[&escrowed, &escrowed]).is_err());
    }

    #[test]
    fn escrowed_tokens_must_reach_the_allowed_party() {
        let (buyer, _, _, script) = setup(true);
        let (outpoint, mut escrowed) = coin(&script);
        escrowed.token = Some(crate::TokenAmount { id: Hash([4u8; 32]), amount: 50, mint_authority: false });
        let release = Transaction::spend_escrow(&script, NET, (outpoint, escrowed.clone()), EscrowRole::Buyer, &buyer, 1_000, None).unwrap();
        assert!(release.check_escrow_destinations(NET, &[&escrowed]).is_ok());
        let mut short = release.clone();
        short.outputs[0].token.as_mut().unwrap().amount = 49;
        assert!(short.check_escrow_destinations(NET, &[&escrowed]).is_err());
    }
}
