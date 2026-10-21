//! Offers: a trading layer in the protocol itself.
//!
//! A maker locks coins (IMN or a token) in an offer output whose script spells out the terms:
//! what is given, what is wanted in return, at what price, and who is paid. Anyone may take
//! the offer, in full or in part, without the maker's signature: the take is valid only if the
//! same transaction pays the maker at least the price for what it takes, and, for a partial take,
//! puts the rest back in an identical offer. The maker can cancel at any time with their key.
//!
//! The terms are stored in the output itself, not hashed into an address, so every node can
//! list the open offers as an order book. A take and the payment are one transaction, so
//! either both happen or neither does, and the chain is the only party in between.
//!
//! An offer's signature script is either a take, `[0, payment output (u16), rest output (u16)]`
//! with `0xffff` for "no rest", or a cancel, `[1] ++ signature (64)` by the maker. Within one
//! transaction no output may be named by two offers, so one payment never settles two takes.

use crate::address::{Address, AddressType, Network};
use crate::hash::Hash;
use crate::transaction::{Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use std::collections::BTreeSet;

/// Script version of an offer output. The script is the terms themselves.
pub const SCRIPT_VERSION_OFFER: u8 = 0x05;

/// Length of an offer's terms, which are its script.
pub const OFFER_TERMS_BYTES: usize = 32 + 33 + 33 + 8 + 8;
const TERMS_BYTES: usize = OFFER_TERMS_BYTES;
const TAKE: u8 = 0;
const CANCEL: u8 = 1;
const NO_REST: u16 = 0xffff;

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidOffer(reason.to_string())
}

/// IMN, or the token with this ID.
pub type Asset = Option<Hash>;

fn put_asset(out: &mut Vec<u8>, asset: &Asset) {
    match asset {
        None => out.extend_from_slice(&[0u8; 33]),
        Some(id) => {
            out.push(1);
            out.extend_from_slice(&id.0);
        }
    }
}

fn read_asset(bytes: &[u8]) -> Result<Asset, TransactionError> {
    match bytes[0] {
        0 if bytes[1..].iter().all(|b| *b == 0) => Ok(None),
        1 => Ok(Some(Hash(bytes[1..33].try_into().expect("32 bytes")))),
        _ => Err(invalid("malformed asset")),
    }
}

/// How much of `asset` an output carries: its IMN, or its amount of that token.
pub fn amount_of(output: &TxOutput, asset: &Asset) -> u64 {
    match asset {
        None => output.value_atoms,
        Some(id) => output.token.as_ref().filter(|t| t.id == *id && !t.mint_authority).map_or(0, |t| t.amount),
    }
}

/// The terms of an offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferTerms {
    /// Paid when the offer is taken, and the only key that can cancel it.
    pub maker: [u8; 32],
    /// What the offer gives: its coin holds this.
    pub give: Asset,
    /// What the maker wants in return.
    pub want: Asset,
    /// The price: taking `give_units` costs at least `give_units * price_num / price_den` units
    /// of `want`, rounded up.
    pub price_num: u64,
    pub price_den: u64,
}

impl OfferTerms {
    pub fn new(maker: [u8; 32], give: Asset, want: Asset, price_num: u64, price_den: u64) -> Result<Self, TransactionError> {
        let terms = Self { maker, give, want, price_num, price_den };
        terms.check()?;
        Ok(terms)
    }

    fn check(&self) -> Result<(), TransactionError> {
        if VerifyingKey::from_bytes(&self.maker).is_err() {
            return Err(invalid("the maker must be a valid Ed25519 public key"));
        }
        if self.give == self.want {
            return Err(invalid("an offer exchanges two different assets"));
        }
        if self.price_num == 0 || self.price_den == 0 {
            return Err(invalid("the price must be positive"));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(TERMS_BYTES);
        bytes.extend_from_slice(&self.maker);
        put_asset(&mut bytes, &self.give);
        put_asset(&mut bytes, &self.want);
        bytes.extend_from_slice(&self.price_num.to_be_bytes());
        bytes.extend_from_slice(&self.price_den.to_be_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TransactionError> {
        if bytes.len() != TERMS_BYTES {
            return Err(invalid("offer terms are 114 bytes"));
        }
        let number = |from: usize| u64::from_be_bytes(bytes[from..from + 8].try_into().expect("8 bytes"));
        Self::new(bytes[..32].try_into().expect("32 bytes"), read_asset(&bytes[32..65])?, read_asset(&bytes[65..98])?, number(98), number(106))
    }

    /// The locking script of an offer with these terms.
    pub fn script(&self) -> ScriptPublicKey {
        ScriptPublicKey { version: SCRIPT_VERSION_OFFER, script: self.to_bytes() }
    }

    /// Where the maker is paid.
    pub fn maker_script(&self, network: Network) -> ScriptPublicKey {
        ScriptPublicKey::pay_to_address(&Address::from_public_key(network, AddressType::PubKeyHash, &self.maker))
    }

    /// The least that taking `give_units` costs, or `None` if it does not fit in 64 bits.
    pub fn cost(&self, give_units: u64) -> Option<u64> {
        let cost = (give_units as u128 * self.price_num as u128).div_ceil(self.price_den as u128);
        u64::try_from(cost).ok()
    }

    /// The offer's terms, if this output is an offer.
    pub fn of(output: &TxOutput) -> Option<Self> {
        (output.script_public_key.version == SCRIPT_VERSION_OFFER).then(|| Self::from_bytes(&output.script_public_key.script).ok())?
    }
}

/// How an input spends an offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferSpend {
    Take { payment: u16, rest: Option<u16> },
    Cancel,
}

impl Transaction {
    /// Reads how an input spends an offer, without checking it.
    pub fn offer_spend(&self, input_index: usize) -> Option<OfferSpend> {
        let bytes = &self.inputs.get(input_index)?.signature_script;
        match (bytes.first()?, bytes.len()) {
            (&TAKE, 5) => {
                let payment = u16::from_be_bytes([bytes[1], bytes[2]]);
                let rest = u16::from_be_bytes([bytes[3], bytes[4]]);
                Some(OfferSpend::Take { payment, rest: (rest != NO_REST).then_some(rest) })
            }
            (&CANCEL, 65) => Some(OfferSpend::Cancel),
            _ => None,
        }
    }

    /// Checks an input that spends an offer: a cancel must be signed by the maker; a take needs
    /// no signature, and what it must pay is checked by `check_offer_takes`.
    pub(crate) fn verify_offer_input(&self, network: Network, input_index: usize, terms_bytes: &[u8]) -> Result<(), TransactionError> {
        let terms = OfferTerms::from_bytes(terms_bytes)?;
        match self.offer_spend(input_index).ok_or_else(|| invalid("malformed signature script"))? {
            OfferSpend::Take { .. } => Ok(()),
            OfferSpend::Cancel => {
                let signature: [u8; 64] = self.inputs[input_index].signature_script[1..].try_into().expect("64 bytes");
                VerifyingKey::from_bytes(&terms.maker)
                    .and_then(|key| key.verify_strict(self.sig_hash(network, input_index).as_bytes(), &Signature::from_bytes(&signature)))
                    .map_err(|e| TransactionError::InvalidSignature(e.to_string()))
            }
        }
    }

    /// Checks every offer this transaction takes: the maker is paid at least the price for what
    /// is taken, and a partial take puts the rest back in an identical offer. `spent` holds the
    /// outputs the transaction spends, in input order.
    /// Returns the outputs the takes claim (payments and rests), which no other contract in the
    /// transaction may count.
    pub fn check_offer_takes(&self, network: Network, spent: &[&TxOutput]) -> Result<BTreeSet<u16>, TransactionError> {
        let mut named: BTreeSet<u16> = BTreeSet::new();
        for (index, coin) in spent.iter().enumerate() {
            if coin.script_public_key.version != SCRIPT_VERSION_OFFER {
                continue;
            }
            let Some(OfferSpend::Take { payment, rest }) = self.offer_spend(index) else { continue };
            let terms = OfferTerms::from_bytes(&coin.script_public_key.script)?;
            let offered = amount_of(coin, &terms.give);
            if offered == 0 {
                return Err(invalid("the offer's coin does not hold what it offers"));
            }
            // An IMN offer holding a token could hand the token over for nothing
            if terms.give.is_none() && coin.token.is_some() {
                return Err(invalid("an IMN offer may not hold a token"));
            }

            for output in std::iter::once(payment).chain(rest) {
                if !named.insert(output) {
                    return Err(invalid("an output may settle only one offer"));
                }
            }
            let output = |i: u16| self.outputs.get(i as usize).ok_or_else(|| invalid("the take names an output that does not exist"));

            let remaining = match rest {
                None => 0,
                Some(i) => {
                    let rest = output(i)?;
                    if rest.script_public_key != coin.script_public_key {
                        return Err(invalid("the rest of a partly taken offer must stay in the same offer"));
                    }
                    if terms.give.is_some() && rest.value_atoms < coin.value_atoms {
                        return Err(invalid("the rest of a token offer keeps the IMN the offer held"));
                    }
                    let left = amount_of(rest, &terms.give);
                    if left == 0 || left >= offered {
                        return Err(invalid("a partial take takes some, and leaves some"));
                    }
                    left
                }
            };
            let taken = offered - remaining;
            let cost = terms.cost(taken).ok_or_else(|| invalid("price overflows"))?;
            let paid = output(payment)?;
            if paid.script_public_key != terms.maker_script(network) || amount_of(paid, &terms.want) < cost {
                return Err(invalid("the maker must be paid the price for what is taken"));
            }
            // A fully taken token offer gives the IMN it held back to the maker, on top of the price
            if terms.give.is_some() && rest.is_none() {
                let owed = if terms.want.is_none() { cost.checked_add(coin.value_atoms) } else { Some(coin.value_atoms) };
                if owed.is_none_or(|owed| paid.value_atoms < owed) {
                    return Err(invalid("a fully taken token offer returns its IMN to the maker"));
                }
            }
        }
        Ok(named)
    }

    /// Creates an offer: `give_amount` of `give` locked for anyone to take at the price, paying
    /// `want` to the maker. `coins` are the maker's, with and without tokens.
    #[allow(clippy::too_many_arguments)]
    pub fn build_offer(
        key: &SigningKey,
        network: Network,
        give: Asset,
        give_amount: u64,
        want: Asset,
        price_num: u64,
        price_den: u64,
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
    ) -> Result<Self, String> {
        let terms = OfferTerms::new(key.verifying_key().to_bytes(), give, want, price_num, price_den).map_err(|e| e.to_string())?;
        if give_amount == 0 {
            return Err("Offer a positive amount".to_string());
        }
        let me = Address::from_public_key(network, AddressType::PubKeyHash, key.verifying_key().as_bytes());
        let mut offer = TxOutput::new(0, terms.script());
        match give {
            None => offer.value_atoms = give_amount,
            Some(id) => offer.token = Some(crate::TokenAmount { id, amount: give_amount, mint_authority: false }),
        }
        let tx = fund(vec![offer], Vec::new(), &me, fee_atoms, coins)?;
        sign_own(tx, network, key, &[])
    }

    /// Takes `take_amount` of an offer, paying its price from the taker's coins. What is taken
    /// goes to the taker's address; a partial take puts the rest back in the same offer.
    pub fn take_offer(
        key: &SigningKey,
        network: Network,
        offer: (Outpoint, TxOutput),
        take_amount: u64,
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
    ) -> Result<Self, String> {
        let (offer_outpoint, coin) = offer;
        let terms = OfferTerms::of(&coin).ok_or("That coin is not an offer")?;
        let offered = amount_of(&coin, &terms.give);
        if take_amount == 0 || take_amount > offered {
            return Err(format!("Take between 1 and {} units", offered));
        }
        let cost = terms.cost(take_amount).ok_or("The price is too large")?;
        let me = Address::from_public_key(network, AddressType::PubKeyHash, key.verifying_key().as_bytes());
        let mine = ScriptPublicKey::pay_to_address(&me);

        // 0: the maker's payment; 1: the rest of the offer, if any; then what the taker receives
        let mut outputs = vec![asset_output(&terms.want, cost, terms.maker_script(network))];
        let rest = (take_amount < offered).then(|| {
            let mut rest = coin.clone();
            match terms.give {
                None => rest.value_atoms = offered - take_amount,
                Some(_) => rest.token.as_mut().expect("token offer").amount = offered - take_amount,
            }
            rest
        });
        let rest_index = rest.as_ref().map(|_| 1u16);
        outputs.extend(rest);
        if terms.give.is_some() && rest_index.is_none() {
            // A fully taken token offer returns the IMN it held to the maker
            outputs[0].value_atoms = outputs[0].value_atoms.checked_add(coin.value_atoms).ok_or("Amount too large")?;
        }
        outputs.push(asset_output(&terms.give, take_amount, mine.clone()));

        let witness = [vec![TAKE], 0u16.to_be_bytes().to_vec(), rest_index.unwrap_or(NO_REST).to_be_bytes().to_vec()].concat();
        let offer_input = TxInput { previous_outpoint: offer_outpoint, signature_script: witness };
        let tx = fund(outputs, vec![(offer_input, coin)], &me, fee_atoms, coins)?;
        sign_own(tx, network, key, &[0])
    }

    /// Cancels an offer and returns what it holds to the maker. The fee comes from the maker's
    /// other coins when the offer holds no IMN to pay it.
    pub fn cancel_offer(
        key: &SigningKey,
        network: Network,
        offer: (Outpoint, TxOutput),
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
    ) -> Result<Self, String> {
        let (offer_outpoint, coin) = offer;
        let terms = OfferTerms::of(&coin).ok_or("That coin is not an offer")?;
        if terms.maker != key.verifying_key().to_bytes() {
            return Err("Only the maker can cancel an offer".to_string());
        }
        let me = Address::from_public_key(network, AddressType::PubKeyHash, key.verifying_key().as_bytes());
        let mut back = coin.clone();
        back.script_public_key = ScriptPublicKey::pay_to_address(&me);
        // Pay the fee from the offer's own IMN when it holds enough, else from the maker's coins
        let covers_fee = if back.token.is_some() { back.value_atoms >= fee_atoms } else { back.value_atoms > fee_atoms };
        let others = if covers_fee {
            back.value_atoms -= fee_atoms;
            Vec::new()
        } else {
            coins
        };
        let carried = TxOutput { value_atoms: back.value_atoms, ..coin.clone() };
        let fee_from_others = if covers_fee { 0 } else { fee_atoms };
        let tx = fund(vec![back], vec![(TxInput { previous_outpoint: offer_outpoint, signature_script: Vec::new() }, carried)], &me, fee_from_others, others)?;
        let mut tx = sign_own(tx, network, key, &[0])?;
        let signature = key.sign(tx.sig_hash(network, 0).as_bytes());
        tx.inputs[0].signature_script = [vec![CANCEL], signature.to_bytes().to_vec()].concat();
        Ok(tx)
    }
}

fn asset_output(asset: &Asset, amount: u64, script: ScriptPublicKey) -> TxOutput {
    match asset {
        None => TxOutput::new(amount, script),
        Some(id) => TxOutput::token(*id, amount, script),
    }
}

/// Adds the payer's coins so the transaction covers `outputs` and the fee. `spending` are
/// inputs already chosen, with the outputs they spend, whose IMN and tokens count towards the
/// outputs; the payer's coins make up the rest, and any surplus returns to the payer.
pub(crate) fn fund(
    mut outputs: Vec<TxOutput>,
    spending: Vec<(TxInput, TxOutput)>,
    payer: &Address,
    fee_atoms: u64,
    coins: Vec<(Outpoint, TxOutput)>,
) -> Result<Transaction, String> {
    use std::collections::BTreeMap;
    let change_script = ScriptPublicKey::pay_to_address(payer);
    let mut inputs: Vec<TxInput> = Vec::new();
    // Amounts come from a node's coin listing: never let a bad one wrap around
    const TOO_LARGE: &str = "Amount too large";
    let mut imn_in: u64 = 0;
    let mut tokens_in: BTreeMap<Hash, u64> = BTreeMap::new();
    for (input, spent) in spending {
        imn_in = imn_in.checked_add(spent.value_atoms).ok_or(TOO_LARGE)?;
        if let Some(token) = spent.token.filter(|t| !t.mint_authority) {
            let total = tokens_in.entry(token.id).or_default();
            *total = total.checked_add(token.amount).ok_or(TOO_LARGE)?;
        }
        inputs.push(input);
    }

    // Tokens the outputs carry beyond what the chosen inputs bring
    let mut tokens_out: BTreeMap<Hash, u64> = BTreeMap::new();
    for token in outputs.iter().filter_map(|o| o.token.as_ref()).filter(|t| !t.mint_authority) {
        let total = tokens_out.entry(token.id).or_default();
        *total = total.checked_add(token.amount).ok_or(TOO_LARGE)?;
    }
    let mut change = Vec::new();
    for (id, out) in tokens_out {
        let mut have = tokens_in.get(&id).copied().unwrap_or(0);
        for (outpoint, output) in coins.iter().filter(|(_, o)| o.token.as_ref().is_some_and(|t| t.id == id && !t.mint_authority)) {
            if have >= out {
                break;
            }
            have = have.checked_add(output.token.as_ref().map_or(0, |t| t.amount)).ok_or(TOO_LARGE)?;
            imn_in = imn_in.checked_add(output.value_atoms).ok_or(TOO_LARGE)?;
            inputs.push(TxInput { previous_outpoint: outpoint.clone(), signature_script: Vec::new() });
        }
        if have < out {
            return Err(format!("Not enough of token {}…: have {}, need {}", &id.to_hex()[..12], have, out));
        }
        if have > out {
            change.push(TxOutput::token(id, have - out, change_script.clone()));
        }
    }

    // IMN for the outputs and the fee
    let imn_out = outputs.iter().try_fold(fee_atoms, |sum, o| sum.checked_add(o.value_atoms)).ok_or("Amount too large")?;
    let mut plain: Vec<&(Outpoint, TxOutput)> = coins.iter().filter(|(_, o)| o.token.is_none()).collect();
    plain.sort_by_key(|(_, o)| std::cmp::Reverse(o.value_atoms));
    for (outpoint, output) in plain {
        if imn_in >= imn_out {
            break;
        }
        imn_in = imn_in.checked_add(output.value_atoms).ok_or(TOO_LARGE)?;
        inputs.push(TxInput { previous_outpoint: outpoint.clone(), signature_script: Vec::new() });
    }
    if imn_in < imn_out {
        return Err(format!("Not enough IMN: need {} atoms including the fee", imn_out));
    }
    outputs.extend(change);
    if imn_in > imn_out {
        outputs.push(TxOutput::new(imn_in - imn_out, change_script));
    }
    Ok(Transaction { version: 1, inputs, outputs, payload: Vec::new(), service: None, issue: None })
}

/// Signs every input except those listed, which spend offers and carry no key signature.
pub(crate) fn sign_own(mut tx: Transaction, network: Network, key: &SigningKey, skip: &[usize]) -> Result<Transaction, String> {
    for index in 0..tx.inputs.len() {
        if !skip.contains(&index) {
            tx.sign_input(network, index, key).map_err(|e| e.to_string())?;
        }
    }
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenAmount;

    const NET: Network = Network::Testnet;
    const BEAN: Hash = Hash([0xbe; 32]);

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn script_of(seed: u8) -> ScriptPublicKey {
        ScriptPublicKey::pay_to_address(&Address::from_public_key(NET, AddressType::PubKeyHash, key(seed).verifying_key().as_bytes()))
    }

    fn imn_coin(seed: u8, n: u8, atoms: u64) -> (Outpoint, TxOutput) {
        (Outpoint { transaction_id: Hash([n; 32]), index: 0 }, TxOutput::new(atoms, script_of(seed)))
    }

    fn bean_coin(seed: u8, n: u8, amount: u64) -> (Outpoint, TxOutput) {
        let mut output = TxOutput::new(0, script_of(seed));
        output.token = Some(TokenAmount { id: BEAN, amount, mint_authority: false });
        (Outpoint { transaction_id: Hash([n; 32]), index: 0 }, output)
    }

    /// The first output of `tx` as a coin: where `build_offer` puts the offer.
    fn first_output(tx: &Transaction) -> (Outpoint, TxOutput) {
        (Outpoint { transaction_id: tx.id(), index: 0 }, tx.outputs[0].clone())
    }

    /// Everything a node checks about a transaction, given the coins it may spend.
    fn valid(tx: &Transaction, known: &[(Outpoint, TxOutput)]) -> Result<(), String> {
        let spent: Vec<&TxOutput> = tx
            .inputs
            .iter()
            .map(|i| known.iter().find(|(o, _)| *o == i.previous_outpoint).map(|(_, out)| out).ok_or("unknown input".to_string()))
            .collect::<Result<_, _>>()?;
        for (index, output) in spent.iter().enumerate() {
            tx.verify_input_at(NET, index, &output.script_public_key, 0).map_err(|e| format!("input {}: {}", index, e))?;
        }
        tx.check_tokens(&spent).map_err(|e| e.to_string())?;
        tx.check_offer_takes(NET, &spent).map_err(|e| e.to_string())?;
        let imn_in: u64 = spent.iter().map(|o| o.value_atoms).sum();
        let imn_out: u64 = tx.outputs.iter().map(|o| o.value_atoms).sum();
        if imn_out > imn_in {
            return Err(format!("IMN out {} exceeds in {}", imn_out, imn_in));
        }
        Ok(())
    }

    /// Maker 1 offers 1,000 BEAN at 1,000 atoms each; taker 2 has IMN.
    fn bean_for_imn() -> (Transaction, Vec<(Outpoint, TxOutput)>) {
        let maker_coins = vec![bean_coin(1, 1, 1_000), imn_coin(1, 2, 100_000)];
        let offer = Transaction::build_offer(&key(1), NET, Some(BEAN), 1_000, None, 1_000, 1, 10_000, maker_coins.clone()).unwrap();
        valid(&offer, &maker_coins).unwrap();
        let mut known = maker_coins;
        known.push(first_output(&offer));
        known.push(imn_coin(2, 3, 5_000_000));
        (offer, known)
    }

    #[test]
    fn terms_round_trip_and_prices_round_up() {
        let terms = OfferTerms::new(key(1).verifying_key().to_bytes(), Some(BEAN), None, 3, 2).unwrap();
        assert_eq!(OfferTerms::from_bytes(&terms.to_bytes()).unwrap(), terms);
        assert_eq!(terms.cost(3), Some(5)); // 4.5 rounds up
        assert!(OfferTerms::new(terms.maker, None, None, 1, 1).is_err());
        assert!(OfferTerms::new(terms.maker, Some(BEAN), None, 0, 1).is_err());
    }

    #[test]
    fn anyone_can_take_a_whole_offer_by_paying_the_maker() {
        let (offer, known) = bean_for_imn();
        let take = Transaction::take_offer(&key(2), NET, first_output(&offer), 1_000, 10_000, vec![known[3].clone()]).unwrap();
        valid(&take, &known).unwrap();
        assert_eq!(take.outputs[0].script_public_key, script_of(1));
        assert_eq!(take.outputs[0].value_atoms, 1_000_000);
        assert!(take.outputs.iter().any(|o| o.script_public_key == script_of(2) && amount_of(o, &Some(BEAN)) == 1_000));
    }

    #[test]
    fn a_partial_take_leaves_the_rest_in_the_same_offer() {
        let (offer, known) = bean_for_imn();
        let take = Transaction::take_offer(&key(2), NET, first_output(&offer), 300, 10_000, vec![known[3].clone()]).unwrap();
        valid(&take, &known).unwrap();
        assert_eq!(take.outputs[1].script_public_key, offer.outputs[0].script_public_key);
        assert_eq!(amount_of(&take.outputs[1], &Some(BEAN)), 700);
        assert_eq!(take.outputs[0].value_atoms, 300_000);
    }

    #[test]
    fn a_take_that_underpays_or_redirects_the_rest_is_refused() {
        let (offer, known) = bean_for_imn();
        let honest = Transaction::take_offer(&key(2), NET, first_output(&offer), 300, 10_000, vec![known[3].clone()]).unwrap();
        let resign = |mut tx: Transaction| {
            for i in 1..tx.inputs.len() {
                tx.sign_input(NET, i, &key(2)).unwrap();
            }
            tx
        };

        let mut underpaid = honest.clone();
        underpaid.outputs[0].value_atoms -= 1;
        assert!(valid(&resign(underpaid), &known).is_err());

        let mut wrong_payee = honest.clone();
        wrong_payee.outputs[0].script_public_key = script_of(2);
        assert!(valid(&resign(wrong_payee), &known).is_err());

        // The rest moved to the taker instead of staying on offer
        let mut stolen_rest = honest.clone();
        stolen_rest.outputs[1].script_public_key = script_of(2);
        assert!(valid(&resign(stolen_rest), &known).is_err());

        // Claiming a smaller take than was really made: the rest shrinks, the payment does not grow
        let mut shrunk = honest.clone();
        shrunk.outputs[1].token.as_mut().unwrap().amount = 100;
        shrunk.outputs[2].token.as_mut().unwrap().amount = 900;
        assert!(valid(&resign(shrunk), &known).is_err());
    }

    #[test]
    fn one_payment_cannot_settle_two_offers() {
        let maker_coins = vec![bean_coin(1, 1, 500), bean_coin(1, 2, 500), imn_coin(1, 3, 100_000), imn_coin(1, 4, 100_000)];
        let first =
            Transaction::build_offer(&key(1), NET, Some(BEAN), 500, None, 1_000, 1, 10_000, vec![maker_coins[0].clone(), maker_coins[2].clone()]).unwrap();
        let second =
            Transaction::build_offer(&key(1), NET, Some(BEAN), 500, None, 1_000, 1, 10_000, vec![maker_coins[1].clone(), maker_coins[3].clone()]).unwrap();
        let mut known = maker_coins;
        known.extend([first_output(&first), first_output(&second), imn_coin(2, 9, 5_000_000)]);
        // Take the first properly, then add the second offer naming the same payment output
        let mut take = Transaction::take_offer(&key(2), NET, first_output(&first), 500, 10_000, vec![known[6].clone()]).unwrap();
        take.inputs.push(TxInput { previous_outpoint: first_output(&second).0, signature_script: vec![TAKE, 0, 0, 0xff, 0xff] });
        take.outputs.push(TxOutput::token(BEAN, 500, script_of(2)));
        for i in 1..take.inputs.len() - 1 {
            take.sign_input(NET, i, &key(2)).unwrap();
        }
        assert!(valid(&take, &known).unwrap_err().contains("only one offer"));
    }

    #[test]
    fn imn_can_be_offered_for_a_token_too() {
        // Maker 1 sells 2 IMN for BEAN at 1 BEAN per 1,000,000 atoms
        let maker_coins = vec![imn_coin(1, 1, 3_000_000)];
        let offer = Transaction::build_offer(&key(1), NET, None, 2_000_000, Some(BEAN), 1, 1_000_000, 10_000, maker_coins.clone()).unwrap();
        valid(&offer, &maker_coins).unwrap();
        let mut known = maker_coins;
        known.extend([first_output(&offer), bean_coin(2, 5, 10), imn_coin(2, 6, 50_000)]);
        let take =
            Transaction::take_offer(&key(2), NET, first_output(&offer), 500_000, 10_000, vec![known[2].clone(), known[3].clone()]).unwrap();
        valid(&take, &known).unwrap();
        assert_eq!(amount_of(&take.outputs[0], &Some(BEAN)), 1); // 0.5 BEAN rounds up to 1
        assert_eq!(take.outputs[1].value_atoms, 1_500_000);
    }

    #[test]
    fn only_the_maker_can_cancel() {
        let (offer, known) = bean_for_imn();
        let maker_imn = vec![imn_coin(1, 7, 50_000)];
        let mut known = known;
        known.extend(maker_imn.clone());
        let cancel = Transaction::cancel_offer(&key(1), NET, first_output(&offer), 10_000, maker_imn.clone()).unwrap();
        valid(&cancel, &known).unwrap();
        assert!(cancel.outputs.iter().any(|o| o.script_public_key == script_of(1) && amount_of(o, &Some(BEAN)) == 1_000));
        assert!(Transaction::cancel_offer(&key(2), NET, first_output(&offer), 10_000, maker_imn).is_err());
        // A forged cancel signed by someone else
        let mut forged = cancel.clone();
        let signature = key(2).sign(forged.sig_hash(NET, 0).as_bytes());
        forged.inputs[0].signature_script = [vec![CANCEL], signature.to_bytes().to_vec()].concat();
        assert!(valid(&forged, &known).is_err());
    }
}
