//! Swap offers: IMN for sale against a coin on another chain, listed in the order book.
//!
//! A maker locks IMN in a swap offer whose script spells out the terms: which chain and which
//! token they want (USDC on Base, say), at what price, the address on that chain where they are
//! paid, and the least they sell at once. The chain cannot see the other chain, so nobody can
//! take the offer by themselves; the maker fills it. A buyer locks the payment in the swap
//! contract on the other chain under a hash of their own secret, naming their IMN key. The
//! maker's software checks the lock and spends the offer into a hash lock for the buyer under
//! the same hash, refundable to the maker well before the buyer's lock can be refunded. The
//! buyer claims the IMN, which reveals the secret, and the maker uses it to claim the payment.
//!
//! The terms are stored in the output itself so every node can list the offers. The only way to
//! spend a swap offer is with the maker's signature, `[signature (64)]`: to fill it or cancel it.

use crate::address::{Address, AddressType, Network};
use crate::hashlock::HashLockScript;
use crate::offer::{fund, sign_own};
use crate::transaction::{
    Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// Script version of a swap offer output. The script is the terms themselves.
pub const SCRIPT_VERSION_SWAP_OFFER: u8 = 0x09;

/// Length of a swap offer's terms, which are its script.
pub const SWAP_OFFER_TERMS_BYTES: usize = 32 + 8 + 20 + 20 + 8 + 8 + 8;

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidOffer(reason.to_string())
}

/// The terms of a swap offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapOfferTerms {
    /// The only key that can fill or cancel the offer.
    pub maker: [u8; 32],
    /// The other chain's ID (8453 for Base).
    pub chain_id: u64,
    /// The token wanted on that chain: its contract address.
    pub token: [u8; 20],
    /// Where the maker is paid on that chain.
    pub pay_to: [u8; 20],
    /// The price: `atoms` of IMN cost `atoms * price_num / price_den` of the token's smallest
    /// units, rounded up.
    pub price_num: u64,
    pub price_den: u64,
    /// The least the maker sells in one fill, in atoms.
    pub min_atoms: u64,
}

impl SwapOfferTerms {
    pub fn new(
        maker: [u8; 32],
        chain_id: u64,
        token: [u8; 20],
        pay_to: [u8; 20],
        price_num: u64,
        price_den: u64,
        min_atoms: u64,
    ) -> Result<Self, TransactionError> {
        if VerifyingKey::from_bytes(&maker).is_err() {
            return Err(invalid("the maker must be a valid Ed25519 public key"));
        }
        if chain_id == 0 || token == [0; 20] || pay_to == [0; 20] {
            return Err(invalid(
                "name the chain, the token and the address to be paid at",
            ));
        }
        if price_num == 0 || price_den == 0 {
            return Err(invalid("the price must be positive"));
        }
        Ok(Self {
            maker,
            chain_id,
            token,
            pay_to,
            price_num,
            price_den,
            min_atoms,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SWAP_OFFER_TERMS_BYTES);
        bytes.extend_from_slice(&self.maker);
        bytes.extend_from_slice(&self.chain_id.to_be_bytes());
        bytes.extend_from_slice(&self.token);
        bytes.extend_from_slice(&self.pay_to);
        bytes.extend_from_slice(&self.price_num.to_be_bytes());
        bytes.extend_from_slice(&self.price_den.to_be_bytes());
        bytes.extend_from_slice(&self.min_atoms.to_be_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        if bytes.len() != SWAP_OFFER_TERMS_BYTES {
            return Err(invalid("swap offer terms are 104 bytes"));
        }
        let number =
            |from: usize| u64::from_be_bytes(bytes[from..from + 8].try_into().expect("8 bytes"));
        Self::new(
            bytes[..32].try_into().expect("32 bytes"),
            number(32),
            bytes[40..60].try_into().expect("20 bytes"),
            bytes[60..80].try_into().expect("20 bytes"),
            number(80),
            number(88),
            number(96),
        )
    }

    /// The locking script of a swap offer with these terms.
    pub fn script(&self) -> ScriptPublicKey {
        ScriptPublicKey {
            version: SCRIPT_VERSION_SWAP_OFFER,
            script: self.to_bytes(),
        }
    }

    /// What `atoms` of IMN cost in the token's smallest units, or `None` if it does not fit.
    pub fn cost(&self, atoms: u64) -> Option<u64> {
        u64::try_from((atoms as u128 * self.price_num as u128).div_ceil(self.price_den as u128))
            .ok()
    }

    /// How many atoms a payment of `units` buys: the most whose cost it covers.
    pub fn atoms_for(&self, units: u64) -> u64 {
        u64::try_from(units as u128 * self.price_den as u128 / self.price_num as u128)
            .unwrap_or(u64::MAX)
    }

    /// The offer's terms, if this output is a swap offer.
    pub fn of(output: &TxOutput) -> Option<Self> {
        (output.script_public_key.version == SCRIPT_VERSION_SWAP_OFFER)
            .then(|| Self::from_bytes(&output.script_public_key.script).ok())?
    }
}

impl Transaction {
    /// Checks an input that spends a swap offer: it must carry the maker's signature.
    pub(crate) fn verify_swap_offer_input(
        &self,
        network: Network,
        input_index: usize,
        terms_bytes: &[u8],
    ) -> Result<(), TransactionError> {
        let terms = SwapOfferTerms::from_bytes(terms_bytes)?;
        let signature: [u8; 64] = self.inputs[input_index]
            .signature_script
            .as_slice()
            .try_into()
            .map_err(|_| {
                TransactionError::InvalidSignatureScript(
                    self.inputs[input_index].signature_script.len(),
                )
            })?;
        VerifyingKey::from_bytes(&terms.maker)
            .and_then(|key| {
                key.verify_strict(
                    self.sig_hash(network, input_index).as_bytes(),
                    &Signature::from_bytes(&signature),
                )
            })
            .map_err(|e| TransactionError::InvalidSignature(e.to_string()))
    }

    /// Lists `atoms` of the maker's IMN for sale on these terms.
    pub fn build_swap_offer(
        key: &SigningKey,
        network: Network,
        terms: &SwapOfferTerms,
        atoms: u64,
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
    ) -> Result<Self, String> {
        if terms.maker != key.verifying_key().to_bytes() {
            return Err("The offer's terms name another key".to_string());
        }
        if atoms == 0 || atoms < terms.min_atoms {
            return Err("Offer at least the smallest amount you sell at once".to_string());
        }
        let me = Address::from_public_key(
            network,
            AddressType::PubKeyHash,
            key.verifying_key().as_bytes(),
        );
        let tx = fund(
            vec![TxOutput::new(atoms, terms.script())],
            Vec::new(),
            &me,
            fee_atoms,
            coins,
        )?;
        sign_own(tx, network, key, &[])
    }

    /// Fills `atoms` of a swap offer for a buyer who has locked the payment on the other chain:
    /// locks them for the buyer under `hash`, refundable to the maker from blue score
    /// `refund_after`, and puts the rest back in the same offer. The fee comes from the offer.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_swap_offer(
        key: &SigningKey,
        network: Network,
        offer: (Outpoint, TxOutput),
        atoms: u64,
        buyer: [u8; 32],
        hash: [u8; 32],
        refund_after: u64,
        fee_atoms: u64,
    ) -> Result<(Self, HashLockScript), String> {
        let (outpoint, coin) = offer;
        let terms = SwapOfferTerms::of(&coin).ok_or("That coin is not a swap offer")?;
        if terms.maker != key.verifying_key().to_bytes() {
            return Err("Only the maker can fill a swap offer".to_string());
        }
        let rest = coin
            .value_atoms
            .checked_sub(atoms)
            .and_then(|r| r.checked_sub(fee_atoms))
            .ok_or("The offer does not hold that much")?;
        if atoms == 0 {
            return Err("Fill a positive amount".to_string());
        }
        let lock = HashLockScript::new(buyer, terms.maker, hash, refund_after)
            .map_err(|e| e.to_string())?;
        let mut outputs = vec![TxOutput::new(
            atoms,
            ScriptPublicKey::pay_to_address(&lock.address(network)),
        )];
        if rest > 0 {
            outputs.push(TxOutput::new(rest, terms.script()));
        }
        Ok((
            Self::sign_swap_offer_spend(key, network, outpoint, outputs),
            lock,
        ))
    }

    /// Cancels a swap offer, returning its IMN less the fee to the maker.
    pub fn cancel_swap_offer(
        key: &SigningKey,
        network: Network,
        offer: (Outpoint, TxOutput),
        fee_atoms: u64,
    ) -> Result<Self, String> {
        let (outpoint, coin) = offer;
        let terms = SwapOfferTerms::of(&coin).ok_or("That coin is not a swap offer")?;
        if terms.maker != key.verifying_key().to_bytes() {
            return Err("Only the maker can cancel a swap offer".to_string());
        }
        let back = coin
            .value_atoms
            .checked_sub(fee_atoms)
            .filter(|v| *v > 0)
            .ok_or("The fee takes everything in the offer")?;
        let me = Address::from_public_key(
            network,
            AddressType::PubKeyHash,
            key.verifying_key().as_bytes(),
        );
        Ok(Self::sign_swap_offer_spend(
            key,
            network,
            outpoint,
            vec![TxOutput::new(back, ScriptPublicKey::pay_to_address(&me))],
        ))
    }

    fn sign_swap_offer_spend(
        key: &SigningKey,
        network: Network,
        outpoint: Outpoint,
        outputs: Vec<TxOutput>,
    ) -> Self {
        let mut tx = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_outpoint: outpoint,
                signature_script: Vec::new(),
            }],
            outputs,
            payload: Vec::new(),
            service: None,
            issue: None,
        };
        tx.inputs[0].signature_script = key
            .sign(tx.sig_hash(network, 0).as_bytes())
            .to_bytes()
            .to_vec();
        tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash;

    const NET: Network = Network::Testnet;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    // 1 IMN (1e8 atoms) for 0.25 USDC (250,000 units)
    fn terms(maker: &SigningKey) -> SwapOfferTerms {
        SwapOfferTerms::new(
            maker.verifying_key().to_bytes(),
            8453,
            [0x83; 20],
            [0x11; 20],
            250_000,
            100_000_000,
            10_000_000,
        )
        .unwrap()
    }

    fn offer(terms: &SwapOfferTerms, atoms: u64) -> (Outpoint, TxOutput) {
        (
            Outpoint {
                transaction_id: Hash([5u8; 32]),
                index: 0,
            },
            TxOutput::new(atoms, terms.script()),
        )
    }

    #[test]
    fn terms_round_trip_and_price() {
        let t = terms(&key(1));
        assert_eq!(SwapOfferTerms::from_bytes(&t.to_bytes()).unwrap(), t);
        assert_eq!(t.to_bytes().len(), SWAP_OFFER_TERMS_BYTES);
        assert_eq!(t.cost(100_000_000), Some(250_000));
        assert_eq!(t.cost(1), Some(1)); // rounded up
        assert_eq!(t.atoms_for(250_000), 100_000_000);
        assert!(t.cost(t.atoms_for(123_457)).unwrap() <= 123_457);
        assert!(SwapOfferTerms::new(t.maker, 0, [1; 20], [1; 20], 1, 1, 0).is_err());
        assert!(SwapOfferTerms::new(t.maker, 1, [1; 20], [1; 20], 0, 1, 0).is_err());
    }

    #[test]
    fn maker_fills_into_a_hash_lock_and_keeps_the_rest_listed() {
        let maker = key(1);
        let t = terms(&maker);
        let coin = offer(&t, 500_000_000);
        let (tx, lock) = Transaction::fill_swap_offer(
            &maker,
            NET,
            coin.clone(),
            100_000_000,
            key(2).verifying_key().to_bytes(),
            [9; 32],
            5_000,
            20_000,
        )
        .unwrap();
        tx.verify_input_at(NET, 0, &coin.1.script_public_key, 0)
            .unwrap();
        assert_eq!(tx.outputs[0].value_atoms, 100_000_000);
        assert_eq!(
            tx.outputs[0].script_public_key,
            ScriptPublicKey::pay_to_address(&lock.address(NET))
        );
        assert_eq!(lock.refunder, t.maker);
        assert_eq!(
            tx.outputs[1],
            TxOutput::new(500_000_000 - 100_000_000 - 20_000, t.script())
        );
    }

    #[test]
    fn only_the_maker_can_spend_it() {
        let maker = key(1);
        let t = terms(&maker);
        let coin = offer(&t, 500_000_000);
        assert!(Transaction::fill_swap_offer(
            &key(2),
            NET,
            coin.clone(),
            1,
            [0; 32],
            [0; 32],
            1,
            0
        )
        .is_err());
        // A spend signed by someone else, or with no signature, is invalid
        let mut tx = Transaction::cancel_swap_offer(&maker, NET, coin.clone(), 20_000).unwrap();
        tx.verify_input_at(NET, 0, &coin.1.script_public_key, 0)
            .unwrap();
        tx.inputs[0].signature_script = key(2)
            .sign(tx.sig_hash(NET, 0).as_bytes())
            .to_bytes()
            .to_vec();
        assert!(tx
            .verify_input_at(NET, 0, &coin.1.script_public_key, 0)
            .is_err());
        tx.inputs[0].signature_script = Vec::new();
        assert!(tx
            .verify_input_at(NET, 0, &coin.1.script_public_key, 0)
            .is_err());
        // Changing the outputs after signing breaks the signature
        let mut tx = Transaction::cancel_swap_offer(&maker, NET, coin.clone(), 20_000).unwrap();
        tx.outputs[0].value_atoms -= 1;
        assert!(tx
            .verify_input_at(NET, 0, &coin.1.script_public_key, 0)
            .is_err());
    }
}
