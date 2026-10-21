//! Native tokens.
//!
//! Any output may carry an amount of one token besides its IMN. Fees are always paid in IMN.
//!
//! - **Issuing.** A transaction that names `TokenInfo` in its `issue` field creates a token. Its
//!   ID is derived from the transaction's first input, which can only ever be spent once, so
//!   IDs are unique and cannot be claimed in advance. The name and ticker are labels; they
//!   need not be unique.
//! - **Transferring.** In every other case each token is conserved exactly: the amounts a
//!   transaction spends and creates must be equal. A wallet that forgets a token is refused,
//!   rather than silently destroying it. To destroy tokens, send them to `ScriptPublicKey::burn()`.
//! - **Minting.** An output may carry a token's *mint authority* instead of an amount. A
//!   transaction that spends one may create any amount of that token, and may pass the authority
//!   on, split it, or drop it. A token issued without an authority output, or whose last
//!   authority is dropped, has a supply that can never grow.

use crate::address::{Address, Network};
use crate::hash::Hash;
use crate::serialize::{put_bytes, tagged_hash, Decode, DecodeError, Encode, Reader};
use crate::transaction::{is_valid_invoice_id, Outpoint, ScriptPublicKey, Transaction, TransactionError, TxInput, TxOutput};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_TOKEN_NAME_BYTES: usize = 32;
pub const MAX_TOKEN_TICKER_CHARS: usize = 8;
/// Amounts are 64-bit whole numbers, so more decimals than this leave too little room above.
pub const MAX_TOKEN_DECIMALS: u8 = 12;
pub const MAX_TOKEN_URL_BYTES: usize = 120;
/// Script version of an output no key can ever spend. Tokens sent there are destroyed.
pub const SCRIPT_VERSION_BURN: u8 = 0xff;

const TOKEN_ID_CONTEXT: &str = "IMN 2026 token id";

fn invalid(reason: &str) -> TransactionError {
    TransactionError::InvalidToken(reason.to_string())
}

/// A token amount carried by an output.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenAmount {
    pub id: Hash,
    /// In the token's smallest unit.
    pub amount: u64,
    /// True for an output that carries the right to mint more of the token. Such an output
    /// carries no amount.
    pub mint_authority: bool,
}

/// What a token is called, set once when it is issued.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    /// 1 to 32 bytes of UTF-8.
    pub name: String,
    /// 1 to 8 characters, A-Z and 0-9.
    pub ticker: String,
    /// Where the decimal point sits when amounts are shown: 0 to 12.
    pub decimals: u8,
    /// Optional link to more about the token, up to 120 bytes of printable ASCII.
    pub url: String,
}

impl TokenInfo {
    pub fn check(&self) -> Result<(), TransactionError> {
        if self.name.is_empty() || self.name.len() > MAX_TOKEN_NAME_BYTES || self.name.chars().any(char::is_control) {
            return Err(invalid("a token name is 1 to 32 bytes with no control characters"));
        }
        let ticker_ok = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
        if self.ticker.is_empty() || self.ticker.len() > MAX_TOKEN_TICKER_CHARS || !self.ticker.chars().all(ticker_ok) {
            return Err(invalid("a ticker is 1 to 8 characters of A-Z and 0-9"));
        }
        if self.decimals > MAX_TOKEN_DECIMALS {
            return Err(invalid("a token has at most 12 decimals"));
        }
        if self.url.len() > MAX_TOKEN_URL_BYTES || !self.url.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Err(invalid("a token URL is up to 120 printable ASCII characters"));
        }
        Ok(())
    }
}

impl Encode for TokenAmount {
    fn encode(&self, out: &mut Vec<u8>) {
        self.id.encode(out);
        out.extend_from_slice(&self.amount.to_be_bytes());
        out.push(self.mint_authority as u8);
    }
}

impl Decode for TokenAmount {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let id = Hash::decode(reader)?;
        let amount = reader.u64()?;
        let mint_authority = match reader.u8()? {
            0 => false,
            1 => true,
            _ => return Err(DecodeError::Invalid("mint authority flag")),
        };
        Ok(Self { id, amount, mint_authority })
    }
}

impl Encode for TokenInfo {
    fn encode(&self, out: &mut Vec<u8>) {
        put_bytes(out, self.name.as_bytes());
        put_bytes(out, self.ticker.as_bytes());
        out.push(self.decimals);
        put_bytes(out, self.url.as_bytes());
    }
}

impl Decode for TokenInfo {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let text = |bytes: Vec<u8>| String::from_utf8(bytes).map_err(|_| DecodeError::Invalid("token text is not UTF-8"));
        Ok(Self {
            name: text(reader.bytes(MAX_TOKEN_NAME_BYTES)?)?,
            ticker: text(reader.bytes(MAX_TOKEN_TICKER_CHARS)?)?,
            decimals: reader.u8()?,
            url: text(reader.bytes(MAX_TOKEN_URL_BYTES)?)?,
        })
    }
}

/// The ID of the token a transaction issues, from the outpoint its first input spends.
pub fn token_id_for(first_input: &Outpoint) -> Hash {
    tagged_hash(TOKEN_ID_CONTEXT, &[&first_input.transaction_id.0, &first_input.index.to_be_bytes()])
}

impl ScriptPublicKey {
    /// A script no key can spend. Coins and tokens sent to it are destroyed.
    pub fn burn() -> Self {
        Self { version: SCRIPT_VERSION_BURN, script: Vec::new() }
    }
}

impl TxOutput {
    /// An output carrying only IMN.
    pub fn new(value_atoms: u64, script_public_key: ScriptPublicKey) -> Self {
        Self { value_atoms, script_public_key, token: None }
    }

    /// An output carrying `amount` of a token and no IMN.
    pub fn token(id: Hash, amount: u64, script_public_key: ScriptPublicKey) -> Self {
        Self { value_atoms: 0, script_public_key, token: Some(TokenAmount { id, amount, mint_authority: false }) }
    }

    /// An output carrying the right to mint more of a token.
    pub fn mint_authority(id: Hash, script_public_key: ScriptPublicKey) -> Self {
        Self { value_atoms: 0, script_public_key, token: Some(TokenAmount { id, amount: 0, mint_authority: true }) }
    }
}

impl Transaction {
    /// The ID of the token this transaction issues, if it issues one.
    pub fn issued_token(&self) -> Option<Hash> {
        self.issue.as_ref()?;
        Some(token_id_for(&self.inputs.first()?.previous_outpoint))
    }

    /// Checks the token rules against the outputs this transaction spends, given in input order.
    pub fn check_tokens(&self, spent: &[&TxOutput]) -> Result<(), TransactionError> {
        let new_id = self.issued_token();
        if let Some(info) = &self.issue {
            info.check()?;
        }

        let mut spent_amounts: BTreeMap<Hash, u64> = BTreeMap::new();
        // Authorities spent, counted: an authority moves or is given up, but is never copied
        let mut spent_authorities: BTreeMap<Hash, u64> = BTreeMap::new();
        // Tokens whose holder may mint here: an authority spent from an address its owner signs
        // for. One held by a contract (vault, escrow, offer, ...) may only be passed on as the
        // contract allows; whoever the contract lets spend it gains no right to mint.
        let mut authorities: BTreeSet<Hash> = BTreeSet::new();
        for output in spent {
            if let Some(token) = &output.token {
                if token.mint_authority {
                    *spent_authorities.entry(token.id).or_default() += 1;
                    let version = output.script_public_key.version;
                    if version == crate::transaction::SCRIPT_VERSION_PUBKEY_HASH
                        || version == crate::multisig::SCRIPT_VERSION_MULTISIG
                    {
                        authorities.insert(token.id);
                    }
                } else {
                    let total = spent_amounts.entry(token.id).or_default();
                    *total = total.checked_add(token.amount).ok_or_else(|| invalid("token input total overflows"))?;
                }
            }
        }

        let mut created_amounts: BTreeMap<Hash, u64> = BTreeMap::new();
        let mut created_authorities: BTreeMap<Hash, u64> = BTreeMap::new();
        for output in &self.outputs {
            let Some(token) = &output.token else { continue };
            if token.mint_authority {
                if token.amount != 0 {
                    return Err(invalid("a mint authority carries no amount"));
                }
                let created = created_authorities.entry(token.id).or_default();
                *created += 1;
                // Issuing creates one authority; otherwise no more leave than came in
                let allowed = spent_authorities.get(&token.id).copied().unwrap_or(0) + u64::from(new_id == Some(token.id));
                if *created > allowed {
                    return Err(invalid("a mint authority can only come from issuing or from another mint authority, one for one"));
                }
            } else {
                if token.amount == 0 {
                    return Err(invalid("a token output carries a positive amount"));
                }
                let total = created_amounts.entry(token.id).or_default();
                *total = total.checked_add(token.amount).ok_or_else(|| invalid("token output total overflows"))?;
            }
        }

        if let Some(id) = new_id {
            if !self.outputs.iter().any(|o| o.token.as_ref().is_some_and(|t| t.id == id)) {
                return Err(invalid("issuing a token creates at least one output of it"));
            }
        }
        // Without the right to mint, every token is conserved exactly
        let ids: BTreeSet<&Hash> = spent_amounts.keys().chain(created_amounts.keys()).collect();
        for id in ids {
            if new_id == Some(*id) || authorities.contains(id) {
                continue;
            }
            if spent_amounts.get(id) != created_amounts.get(id) {
                return Err(invalid("token amounts spent and created must be equal"));
            }
        }
        Ok(())
    }

    /// Creates a token: `supply` units to the issuer's address, plus the right to mint more if
    /// `mintable`. Pays the fee from `key`'s IMN coins. The new token's ID is
    /// `tx.issued_token()`.
    pub fn build_token_issue(
        key: &SigningKey,
        network: Network,
        info: TokenInfo,
        supply: u64,
        mintable: bool,
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
    ) -> Result<Self, String> {
        info.check().map_err(|e| e.to_string())?;
        if supply == 0 && !mintable {
            return Err("A token needs a supply, the right to mint, or both".to_string());
        }
        let me = Address::from_public_key(network, crate::AddressType::PubKeyHash, key.verifying_key().as_bytes());
        let script = ScriptPublicKey::pay_to_address(&me);
        let (inputs, change) = pay_fee_from(coins, fee_atoms)?;
        let id = token_id_for(&inputs[0].previous_outpoint);

        let mut outputs = Vec::new();
        if supply > 0 {
            outputs.push(TxOutput::token(id, supply, script.clone()));
        }
        if mintable {
            outputs.push(TxOutput::mint_authority(id, script.clone()));
        }
        if change > 0 {
            outputs.push(TxOutput::new(change, script));
        }
        sign_all(Transaction { version: 1, inputs, outputs, payload: Vec::new(), service: None, issue: Some(info) }, network, key)
    }

    /// Sends `amount` of a token to `to`, with the token's change and the IMN change back to
    /// the sender. `coins` are the sender's coins, with and without tokens; the fee comes from
    /// their IMN. An invoice ID ties the payment to an order, as with IMN.
    #[allow(clippy::too_many_arguments)]
    pub fn build_token_transfer(
        key: &SigningKey,
        network: Network,
        to: &Address,
        id: Hash,
        amount: u64,
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
        invoice_id: Option<&str>,
    ) -> Result<Self, String> {
        Self::build_token_transfer_with_atoms(key, network, to, id, amount, 0, fee_atoms, coins, invoice_id)
    }

    /// Like `build_token_transfer`, and sends `attach_atoms` of IMN in the same output as the
    /// tokens. A hash lock that holds tokens needs this: whoever spends it pays the fee from
    /// the IMN in the lock.
    #[allow(clippy::too_many_arguments)]
    pub fn build_token_transfer_with_atoms(
        key: &SigningKey,
        network: Network,
        to: &Address,
        id: Hash,
        amount: u64,
        attach_atoms: u64,
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
        invoice_id: Option<&str>,
    ) -> Result<Self, String> {
        if amount == 0 {
            return Err("Send a positive amount".to_string());
        }
        let me = Address::from_public_key(network, crate::AddressType::PubKeyHash, key.verifying_key().as_bytes());
        let script = ScriptPublicKey::pay_to_address(&me);

        // The token coins first, smallest first, until the amount is covered
        let mut token_coins: Vec<(Outpoint, TxOutput)> = coins
            .iter()
            .filter(|(_, o)| o.token.as_ref().is_some_and(|t| t.id == id && !t.mint_authority))
            .cloned()
            .collect();
        token_coins.sort_by_key(|(_, o)| o.token.as_ref().map_or(0, |t| t.amount));
        let mut gathered: u64 = 0;
        let mut inputs = Vec::new();
        let mut carried_atoms: u64 = 0;
        for (outpoint, output) in token_coins {
            if gathered >= amount {
                break;
            }
            gathered = gathered.saturating_add(output.token.as_ref().map_or(0, |t| t.amount));
            carried_atoms = carried_atoms.saturating_add(output.value_atoms);
            inputs.push(TxInput { previous_outpoint: outpoint, signature_script: Vec::new() });
        }
        if gathered < amount {
            return Err(format!("Not enough of this token: have {}, need {}", gathered, amount));
        }

        // Then IMN for the fee and anything attached, beyond the IMN the token coins carry
        let imn_needed = fee_atoms.checked_add(attach_atoms).ok_or("Amount too large")?;
        let short = imn_needed.saturating_sub(carried_atoms);
        let (fee_inputs, imn_change) = if short > 0 { pay_fee_from(coins, short)? } else { (Vec::new(), 0) };
        let imn_change = imn_change + carried_atoms.saturating_sub(imn_needed);
        inputs.extend(fee_inputs);

        let mut first = TxOutput::token(id, amount, ScriptPublicKey::pay_to_address(to));
        first.value_atoms = attach_atoms;
        let mut outputs = vec![first];
        if gathered > amount {
            outputs.push(TxOutput::token(id, gathered - amount, script.clone()));
        }
        if imn_change > 0 {
            outputs.push(TxOutput::new(imn_change, script));
        }
        let payload = invoice_payload(invoice_id)?;
        sign_all(Transaction { version: 1, inputs, outputs, payload, service: None, issue: None }, network, key)
    }

    /// Mints `amount` more of a token to the sender, spending a mint authority the sender holds.
    /// The authority is kept unless `give_up_authority`, which fixes the supply for good.
    pub fn build_token_mint(
        key: &SigningKey,
        network: Network,
        id: Hash,
        amount: u64,
        give_up_authority: bool,
        fee_atoms: u64,
        coins: Vec<(Outpoint, TxOutput)>,
    ) -> Result<Self, String> {
        let me = Address::from_public_key(network, crate::AddressType::PubKeyHash, key.verifying_key().as_bytes());
        let script = ScriptPublicKey::pay_to_address(&me);
        let (authority_outpoint, authority) = coins
            .iter()
            .find(|(_, o)| o.token.as_ref().is_some_and(|t| t.id == id && t.mint_authority))
            .cloned()
            .ok_or("This key holds no mint authority for that token")?;
        let fee_needed = fee_atoms.saturating_sub(authority.value_atoms);
        let (fee_inputs, change) = if fee_needed > 0 { pay_fee_from(coins, fee_needed)? } else { (Vec::new(), 0) };
        let change = change + authority.value_atoms.saturating_sub(fee_atoms);

        let mut inputs = vec![TxInput { previous_outpoint: authority_outpoint, signature_script: Vec::new() }];
        inputs.extend(fee_inputs);
        let mut outputs = Vec::new();
        if amount > 0 {
            outputs.push(TxOutput::token(id, amount, script.clone()));
        }
        if !give_up_authority {
            outputs.push(TxOutput::mint_authority(id, script.clone()));
        }
        if change > 0 {
            outputs.push(TxOutput::new(change, script));
        }
        if outputs.is_empty() {
            outputs.push(TxOutput::new(0, ScriptPublicKey::burn()));
        }
        sign_all(Transaction { version: 1, inputs, outputs, payload: Vec::new(), service: None, issue: None }, network, key)
    }
}

/// Picks IMN-only coins, largest first, to cover `fee_atoms`. Returns the inputs and the change.
fn pay_fee_from(coins: Vec<(Outpoint, TxOutput)>, fee_atoms: u64) -> Result<(Vec<TxInput>, u64), String> {
    let mut plain: Vec<(Outpoint, TxOutput)> = coins.into_iter().filter(|(_, o)| o.token.is_none()).collect();
    plain.sort_by_key(|(_, o)| std::cmp::Reverse(o.value_atoms));
    let mut inputs = Vec::new();
    let mut gathered: u64 = 0;
    for (outpoint, output) in plain {
        if gathered >= fee_atoms && !inputs.is_empty() {
            break;
        }
        gathered = gathered.saturating_add(output.value_atoms);
        inputs.push(TxInput { previous_outpoint: outpoint, signature_script: Vec::new() });
    }
    if inputs.is_empty() || gathered < fee_atoms {
        return Err(format!("Not enough IMN for the {}-atom fee", fee_atoms));
    }
    Ok((inputs, gathered - fee_atoms))
}

fn invoice_payload(invoice_id: Option<&str>) -> Result<Vec<u8>, String> {
    match invoice_id {
        Some(id) if is_valid_invoice_id(id) => Ok([crate::transaction::INVOICE_TAG, id.as_bytes()].concat()),
        Some(_) => Err("Invoice ID must be 1-64 characters of A-Z a-z 0-9 - _ .".to_string()),
        None => Ok(Vec::new()),
    }
}

fn sign_all(mut tx: Transaction, network: Network, key: &SigningKey) -> Result<Transaction, String> {
    for index in 0..tx.inputs.len() {
        tx.sign_input(network, index, key).map_err(|e| e.to_string())?;
    }
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AddressType;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn address(seed: u8) -> Address {
        Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, key(seed).verifying_key().as_bytes())
    }

    fn info() -> TokenInfo {
        TokenInfo { name: "Coffee points".into(), ticker: "BEAN".into(), decimals: 2, url: "https://example.com".into() }
    }

    /// An IMN coin owned by `seed`, at a distinct outpoint.
    fn coin(seed: u8, n: u8, atoms: u64) -> (Outpoint, TxOutput) {
        (Outpoint { transaction_id: Hash([n; 32]), index: 0 }, TxOutput::new(atoms, ScriptPublicKey::pay_to_address(&address(seed))))
    }

    /// The outputs a transaction spends, looked up from the coins it was built from.
    fn spent<'a>(tx: &Transaction, coins: &'a [(Outpoint, TxOutput)]) -> Vec<&'a TxOutput> {
        tx.inputs.iter().map(|i| &coins.iter().find(|(o, _)| *o == i.previous_outpoint).unwrap().1).collect()
    }

    /// The outputs of `tx` as coins, as the ledger would record them.
    fn outputs_of(tx: &Transaction) -> Vec<(Outpoint, TxOutput)> {
        let id = tx.id();
        tx.outputs.iter().enumerate().map(|(i, o)| (Outpoint { transaction_id: id, index: i as u32 }, o.clone())).collect()
    }

    #[test]
    fn token_info_is_checked() {
        assert!(info().check().is_ok());
        for bad in [
            TokenInfo { name: String::new(), ..info() },
            TokenInfo { name: "x".repeat(33), ..info() },
            TokenInfo { ticker: "bean".into(), ..info() },
            TokenInfo { ticker: "TOOLONGTKR".into(), ..info() },
            TokenInfo { decimals: 13, ..info() },
            TokenInfo { url: "x".repeat(121), ..info() },
            TokenInfo { url: "line\nbreak".into(), ..info() },
        ] {
            assert!(bad.check().is_err(), "{:?}", bad);
        }
    }

    #[test]
    fn issuing_creates_a_token_whose_id_comes_from_the_first_input() {
        let coins = vec![coin(1, 1, 100_000)];
        let tx = Transaction::build_token_issue(&key(1), Network::Testnet, info(), 1_000, true, 10_000, coins.clone()).unwrap();
        let id = tx.issued_token().unwrap();
        assert_eq!(id, token_id_for(&coins[0].0));
        assert!(tx.check_tokens(&spent(&tx, &coins)).is_ok());
        assert_eq!(tx.outputs[0].token, Some(TokenAmount { id, amount: 1_000, mint_authority: false }));
        assert!(tx.outputs[1].token.as_ref().unwrap().mint_authority);
        // The encoding carries it all
        assert_eq!(Transaction::from_bytes(&tx.to_bytes()).unwrap(), tx);
    }

    #[test]
    fn transfers_conserve_every_token_exactly() {
        let coins = [coin(1, 1, 100_000), coin(1, 2, 100_000)];
        let issue = Transaction::build_token_issue(&key(1), Network::Testnet, info(), 1_000, false, 10_000, vec![coins[0].clone()]).unwrap();
        let id = issue.issued_token().unwrap();
        let mut wallet = outputs_of(&issue);
        wallet.push(coins[1].clone());

        let send = Transaction::build_token_transfer(&key(1), Network::Testnet, &address(2), id, 300, 10_000, wallet.clone(), Some("order-7")).unwrap();
        assert!(send.check_tokens(&spent(&send, &wallet)).is_ok());
        assert_eq!(send.outputs[0].token.as_ref().unwrap().amount, 300);
        assert_eq!(send.outputs[1].token.as_ref().unwrap().amount, 700);
        assert_eq!(send.invoice_id(), Some("order-7"));

        // Creating more than was spent, or less, is refused
        for change in [701, 699] {
            let mut forged = send.clone();
            forged.outputs[1].token.as_mut().unwrap().amount = change;
            assert!(forged.check_tokens(&spent(&forged, &wallet)).is_err(), "{}", change);
        }
        // So is dropping the token altogether, as a careless wallet might
        let mut dropped = send.clone();
        dropped.outputs.retain(|o| o.token.is_none());
        assert!(dropped.check_tokens(&spent(&dropped, &wallet)).is_err());
        // And claiming a mint authority out of nowhere
        let mut grabbed = send.clone();
        grabbed.outputs.push(TxOutput::mint_authority(id, ScriptPublicKey::pay_to_address(&address(1))));
        assert!(grabbed.check_tokens(&spent(&grabbed, &wallet)).is_err());
    }

    #[test]
    fn an_authority_mints_more_and_giving_it_up_fixes_the_supply() {
        let coins = [coin(1, 1, 100_000), coin(1, 2, 100_000)];
        let issue = Transaction::build_token_issue(&key(1), Network::Testnet, info(), 0, true, 10_000, vec![coins[0].clone()]).unwrap();
        let id = issue.issued_token().unwrap();
        let mut wallet = outputs_of(&issue);
        wallet.push(coins[1].clone());

        let mint = Transaction::build_token_mint(&key(1), Network::Testnet, id, 5_000, false, 10_000, wallet.clone()).unwrap();
        assert!(mint.check_tokens(&spent(&mint, &wallet)).is_ok());
        assert!(mint.outputs.iter().any(|o| o.token.as_ref().is_some_and(|t| t.mint_authority)));

        let last = Transaction::build_token_mint(&key(1), Network::Testnet, id, 1, true, 10_000, wallet.clone()).unwrap();
        assert!(last.check_tokens(&spent(&last, &wallet)).is_ok());
        assert!(!last.outputs.iter().any(|o| o.token.as_ref().is_some_and(|t| t.mint_authority)));

        // Without the authority there is nothing to mint with
        assert!(Transaction::build_token_mint(&key(1), Network::Testnet, id, 1, false, 10_000, vec![coins[1].clone()]).is_err());
        // An authority for one token mints nothing of another
        let other = Hash([0x42; 32]);
        let mut wrong = mint.clone();
        wrong.outputs[0].token.as_mut().unwrap().id = other;
        assert!(wrong.check_tokens(&spent(&wrong, &wallet)).is_err());
    }

    #[test]
    fn tokens_are_destroyed_only_by_sending_them_to_the_burn_script() {
        let coins = [coin(1, 1, 100_000), coin(1, 2, 100_000)];
        let issue = Transaction::build_token_issue(&key(1), Network::Testnet, info(), 1_000, false, 10_000, vec![coins[0].clone()]).unwrap();
        let id = issue.issued_token().unwrap();
        let mut wallet = outputs_of(&issue);
        wallet.push(coins[1].clone());
        let mut burn = Transaction::build_token_transfer(&key(1), Network::Testnet, &address(2), id, 1_000, 10_000, wallet.clone(), None).unwrap();
        burn.outputs[0].script_public_key = ScriptPublicKey::burn();
        assert!(burn.check_tokens(&spent(&burn, &wallet)).is_ok());
        assert_eq!(burn.outputs[0].script_public_key.version, SCRIPT_VERSION_BURN);
    }

    #[test]
    fn malformed_token_outputs_are_refused() {
        let coins = vec![coin(1, 1, 100_000)];
        let issue = Transaction::build_token_issue(&key(1), Network::Testnet, info(), 10, true, 10_000, coins.clone()).unwrap();
        let mut zero = issue.clone();
        zero.outputs[0].token.as_mut().unwrap().amount = 0;
        assert!(zero.check_tokens(&spent(&zero, &coins)).is_err());
        let mut heavy_authority = issue.clone();
        heavy_authority.outputs[1].token.as_mut().unwrap().amount = 3;
        assert!(heavy_authority.check_tokens(&spent(&heavy_authority, &coins)).is_err());
        let mut empty = issue.clone();
        empty.outputs.retain(|o| o.token.is_none());
        assert!(empty.check_tokens(&spent(&empty, &coins)).is_err());
        // Plain payments never pick coins that carry tokens
        let wallet = outputs_of(&issue);
        assert!(Transaction::build_payment(&key(1), Network::Testnet, &address(2), 1, 1, wallet.into_iter().filter(|(_, o)| o.token.is_some()).collect(), None).is_err());
    }

    /// A transaction moving the given spent outputs to `outputs`; the rules here need no signatures.
    fn moving(inputs: usize, outputs: Vec<TxOutput>) -> Transaction {
        Transaction {
            version: 1,
            inputs: (0..inputs)
                .map(|n| crate::TxInput { previous_outpoint: Outpoint { transaction_id: Hash([n as u8; 32]), index: 0 }, signature_script: Vec::new() })
                .collect(),
            outputs,
            payload: Vec::new(),
            service: None,
            issue: None,
        }
    }

    /// GHSA-4q7g-xvf3-9f32: one authority must not become several.
    #[test]
    fn a_mint_authority_cannot_be_copied() {
        let id = Hash([42u8; 32]);
        let mine = ScriptPublicKey::pay_to_address(&address(1));
        let authority = TxOutput::mint_authority(id, mine.clone());
        let one = moving(1, vec![TxOutput::mint_authority(id, mine.clone())]);
        assert!(one.check_tokens(&[&authority]).is_ok());
        let five = moving(1, (0..5).map(|_| TxOutput::mint_authority(id, mine.clone())).collect());
        assert!(five.check_tokens(&[&authority]).is_err());
        // Two in, two out is fine
        let two = moving(2, vec![TxOutput::mint_authority(id, mine.clone()), TxOutput::mint_authority(id, mine.clone())]);
        assert!(two.check_tokens(&[&authority, &authority]).is_ok());
    }

    /// The holder of an authority in their own address may still mint, keeping the authority.
    #[test]
    fn an_owned_authority_still_mints() {
        let id = Hash([42u8; 32]);
        let mine = ScriptPublicKey::pay_to_address(&address(1));
        let authority = TxOutput::mint_authority(id, mine.clone());
        let mint = moving(1, vec![TxOutput::mint_authority(id, mine.clone()), TxOutput::token(id, 1_000_000, mine)]);
        assert!(mint.check_tokens(&[&authority]).is_ok());
    }

    /// An authority held by a contract can be passed on, but whoever the contract lets spend it
    /// may not mint with it or copy it.
    #[test]
    fn an_authority_in_a_contract_grants_no_minting() {
        let id = Hash([42u8; 32]);
        let mine = ScriptPublicKey::pay_to_address(&address(1));
        for version in [crate::vault::SCRIPT_VERSION_VAULT_PENDING, crate::escrow::SCRIPT_VERSION_ESCROW] {
            let held = TxOutput::mint_authority(id, ScriptPublicKey { version, script: vec![7u8; 32] });
            let pass_on = moving(1, vec![TxOutput::mint_authority(id, mine.clone())]);
            assert!(pass_on.check_tokens(&[&held]).is_ok());
            let mint = moving(1, vec![TxOutput::mint_authority(id, mine.clone()), TxOutput::token(id, 1_000, mine.clone())]);
            assert!(mint.check_tokens(&[&held]).is_err());
            let copy = moving(1, vec![TxOutput::mint_authority(id, mine.clone()), TxOutput::mint_authority(id, mine.clone())]);
            assert!(copy.check_tokens(&[&held]).is_err());
        }
    }

}
