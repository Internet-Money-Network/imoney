use imoney_core::{Encode, Hash, Outpoint, Transaction};
use std::collections::HashMap;
use thiserror::Error;

pub use imoney_core::constants::MIN_RELAY_FEE_PER_BYTE;

/// Default cap on the total size of pending transactions, in bytes.
pub const DEFAULT_MAX_MEMPOOL_BYTES: usize = 50_000_000;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum MempoolError {
    #[error("Fee of {0} atoms is below the minimum of {1} atoms for a {2}-byte transaction")]
    FeeTooLow(u64, u64, usize),
    #[error("Input already spent by pending transaction {0}")]
    Conflict(Hash),
    #[error("Mempool is full and this transaction pays less per byte than everything in it")]
    Full,
}

/// A pending transaction with the figures used to rank it.
#[derive(Clone, Debug)]
pub struct MempoolEntry {
    pub tx: Transaction,
    pub fee: u64,
    pub size: usize,
}

impl MempoolEntry {
    /// True when this entry pays a higher fee per byte than `other`.
    /// Compared by cross-multiplication so no precision is lost.
    fn pays_more_than(&self, other: &MempoolEntry) -> bool {
        (self.fee as u128) * (other.size as u128) > (other.fee as u128) * (self.size as u128)
    }
}

/// Transactions waiting to be included in a block.
pub struct Mempool {
    entries: HashMap<Hash, MempoolEntry>,
    /// Which pending transaction spends each outpoint.
    spent: HashMap<Outpoint, Hash>,
    /// Pending transactions by the invoice they name.
    invoices: HashMap<String, Vec<Hash>>,
    total_bytes: usize,
    max_bytes: usize,
    /// Lowest fee admitted, in atoms per byte.
    min_fee_per_byte: u64,
}

impl Default for Mempool {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_MEMPOOL_BYTES)
    }
}

impl Mempool {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            spent: HashMap::new(),
            invoices: HashMap::new(),
            total_bytes: 0,
            max_bytes,
            min_fee_per_byte: MIN_RELAY_FEE_PER_BYTE,
        }
    }

    /// Overrides the minimum fee rate this pool admits.
    pub fn with_min_fee_rate(mut self, atoms_per_byte: u64) -> Self {
        self.min_fee_per_byte = atoms_per_byte;
        self
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, tx_id: &Hash) -> bool {
        self.entries.contains_key(tx_id)
    }

    pub fn get(&self, tx_id: &Hash) -> Option<&Transaction> {
        self.entries.get(tx_id).map(|entry| &entry.tx)
    }

    /// Every pending transaction, in no particular order.
    pub fn transactions(&self) -> impl Iterator<Item = &Transaction> {
        self.entries.values().map(|entry| &entry.tx)
    }

    /// Admits a transaction that has already been checked against the UTXO set.
    /// When the pool is full, the lowest-paying transactions are evicted to make room.
    pub fn insert(&mut self, tx: Transaction, fee: u64) -> Result<Hash, MempoolError> {
        let tx_id = tx.id();
        if self.entries.contains_key(&tx_id) {
            return Ok(tx_id);
        }

        let size = tx.to_bytes().len();
        let min_fee = size as u64 * self.min_fee_per_byte;
        if fee < min_fee {
            return Err(MempoolError::FeeTooLow(fee, min_fee, size));
        }
        for input in &tx.inputs {
            if let Some(spender) = self.spent.get(&input.previous_outpoint) {
                return Err(MempoolError::Conflict(*spender));
            }
        }

        let entry = MempoolEntry { tx, fee, size };
        if self.total_bytes + size > self.max_bytes {
            self.make_room_for(&entry)?;
        }

        for input in &entry.tx.inputs {
            self.spent.insert(input.previous_outpoint.clone(), tx_id);
        }
        if let Some(invoice_id) = entry.tx.invoice_id() {
            self.invoices.entry(invoice_id.to_string()).or_default().push(tx_id);
        }
        self.total_bytes += size;
        self.entries.insert(tx_id, entry);
        Ok(tx_id)
    }

    /// Evicts the cheapest transactions until `incoming` fits, but only ones that pay less than it.
    fn make_room_for(&mut self, incoming: &MempoolEntry) -> Result<(), MempoolError> {
        let mut cheapest: Vec<(Hash, MempoolEntry)> =
            self.entries.iter().map(|(id, entry)| (*id, entry.clone())).collect();
        cheapest.sort_by(|(_, a), (_, b)| {
            if a.pays_more_than(b) {
                std::cmp::Ordering::Greater
            } else if b.pays_more_than(a) {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        });

        let mut freed = 0usize;
        let mut victims = Vec::new();
        for (id, entry) in &cheapest {
            if self.total_bytes - freed + incoming.size <= self.max_bytes {
                break;
            }
            if !incoming.pays_more_than(entry) {
                return Err(MempoolError::Full);
            }
            freed += entry.size;
            victims.push(*id);
        }
        if self.total_bytes - freed + incoming.size > self.max_bytes {
            return Err(MempoolError::Full);
        }
        for id in victims {
            self.remove(&id);
        }
        Ok(())
    }

    pub fn remove(&mut self, tx_id: &Hash) -> Option<Transaction> {
        let entry = self.entries.remove(tx_id)?;
        for input in &entry.tx.inputs {
            self.spent.remove(&input.previous_outpoint);
        }
        if let Some(invoice_id) = entry.tx.invoice_id() {
            if let Some(ids) = self.invoices.get_mut(invoice_id) {
                ids.retain(|id| id != tx_id);
                if ids.is_empty() {
                    self.invoices.remove(invoice_id);
                }
            }
        }
        self.total_bytes -= entry.size;
        Some(entry.tx)
    }

    /// Removes the pending transaction that spends `outpoint`, if there is one. Called for each
    /// coin a new block removed from the ledger: that is the only way a block can invalidate a
    /// pending transaction, so nothing else in the pool needs to be looked at.
    pub fn remove_spender(&mut self, outpoint: &Outpoint) -> Option<Hash> {
        let tx_id = *self.spent.get(outpoint)?;
        self.remove(&tx_id);
        Some(tx_id)
    }

    /// Pending transactions that name `invoice_id`.
    pub fn by_invoice(&self, invoice_id: &str) -> impl Iterator<Item = &Transaction> {
        self.invoices
            .get(invoice_id)
            .into_iter()
            .flatten()
            .filter_map(|tx_id| self.entries.get(tx_id).map(|entry| &entry.tx))
    }

    /// Keeps only the transactions for which `still_valid` returns true.
    pub fn retain(&mut self, mut still_valid: impl FnMut(&Transaction) -> bool) {
        let stale: Vec<Hash> = self
            .entries
            .iter()
            .filter(|(_, entry)| !still_valid(&entry.tx))
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.remove(&id);
        }
    }

    /// Picks transactions for a block: highest fee per byte first, up to `max_bytes` in total.
    /// Ties are broken by transaction ID so the choice is deterministic.
    pub fn select_for_block(&self, max_bytes: usize) -> Vec<Transaction> {
        let mut ranked: Vec<(&Hash, &MempoolEntry)> = self.entries.iter().collect();
        ranked.sort_by(|(id_a, a), (id_b, b)| {
            if a.pays_more_than(b) {
                std::cmp::Ordering::Less
            } else if b.pays_more_than(a) {
                std::cmp::Ordering::Greater
            } else {
                id_a.cmp(id_b)
            }
        });

        let mut used = 0usize;
        let mut selected = Vec::new();
        for (_, entry) in ranked {
            if used + entry.size > max_bytes {
                continue;
            }
            used += entry.size;
            selected.push(entry.tx.clone());
        }
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imoney_core::{ScriptPublicKey, TxInput, TxOutput};

    /// A structurally valid transaction spending a made-up outpoint. The mempool does not
    /// check signatures; the ledger does that before calling `insert`.
    fn tx(spends: u8, payload_len: usize) -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_outpoint: Outpoint { transaction_id: Hash([spends; 32]), index: 0 },
                signature_script: vec![0u8; 96],
                sequence: 0,
            }],
            outputs: vec![TxOutput {
                value_atoms: 1,
                script_public_key: ScriptPublicKey { version: 0, script: vec![spends; 32] },
            }],
            lock_time: 0,
            subnetwork_id: [0u8; 20],
            gas: 0,
            payload: vec![0u8; payload_len],
            service: None,
        }
    }

    fn size_of(t: &Transaction) -> usize {
        t.to_bytes().len()
    }

    #[test]
    fn default_policy_requires_ten_atoms_per_byte() {
        let mut pool = Mempool::default();
        let size = size_of(&tx(1, 0)) as u64;
        assert!(matches!(pool.insert(tx(1, 0), 10 * size - 1), Err(MempoolError::FeeTooLow(..))));
        assert!(pool.insert(tx(1, 0), 10 * size).is_ok());
    }

    #[test]
    fn rejects_low_fees_and_conflicts() {
        let mut pool = Mempool::default().with_min_fee_rate(1);
        let a = tx(1, 0);
        let size = size_of(&a);

        assert_eq!(pool.insert(a.clone(), size as u64 - 1), Err(MempoolError::FeeTooLow(size as u64 - 1, size as u64, size)));
        let id = pool.insert(a.clone(), size as u64).unwrap();
        assert_eq!(pool.insert(a.clone(), size as u64), Ok(id)); // already known

        // Same outpoint, different transaction
        let conflicting = tx(1, 5);
        assert_eq!(pool.insert(conflicting, 10_000), Err(MempoolError::Conflict(id)));

        // Once removed, the outpoint is free again
        pool.remove(&id);
        assert!(pool.is_empty());
        assert!(pool.insert(tx(1, 5), 10_000).is_ok());
    }

    #[test]
    fn block_selection_prefers_higher_fee_rate_and_respects_size() {
        let mut pool = Mempool::default().with_min_fee_rate(1);
        let size = size_of(&tx(1, 0));
        let low = pool.insert(tx(1, 0), 1_000).unwrap();
        let high = pool.insert(tx(2, 0), 9_000).unwrap();
        let mid = pool.insert(tx(3, 0), 5_000).unwrap();
        // Pays the most in total but is large, so its rate is the lowest
        let bulky = pool.insert(tx(4, 20_000), 20_500).unwrap();

        let order: Vec<Hash> = pool.select_for_block(1_000_000).iter().map(|t| t.id()).collect();
        assert_eq!(order, vec![high, mid, low, bulky]);

        let limited: Vec<Hash> = pool.select_for_block(2 * size).iter().map(|t| t.id()).collect();
        assert_eq!(limited, vec![high, mid]);
    }

    #[test]
    fn full_pool_evicts_cheapest_for_better_paying_transaction() {
        let size = size_of(&tx(1, 0));
        let mut pool = Mempool::new(2 * size).with_min_fee_rate(1);
        let cheap = pool.insert(tx(1, 0), 1_000).unwrap();
        let good = pool.insert(tx(2, 0), 5_000).unwrap();

        // Pays less than everything already in the pool
        assert_eq!(pool.insert(tx(3, 0), 500), Err(MempoolError::Full));
        assert_eq!(pool.len(), 2);

        // Pays more than the cheapest: that one is evicted
        let better = pool.insert(tx(4, 0), 3_000).unwrap();
        assert!(!pool.contains(&cheap));
        assert!(pool.contains(&good) && pool.contains(&better));

        // The evicted transaction's outpoint is spendable again
        assert!(pool.insert(tx(1, 0), 9_000).is_ok());
        assert!(!pool.contains(&better));
    }

    #[test]
    fn spender_and_invoice_lookups_follow_inserts_and_removals() {
        let mut pool = Mempool::default().with_min_fee_rate(1);
        let mut invoiced = tx(1, 0);
        invoiced.payload = [imoney_core::transaction::INVOICE_TAG, b"INV-9"].concat();
        let invoiced_id = pool.insert(invoiced.clone(), 1_000).unwrap();
        let plain_id = pool.insert(tx(2, 0), 1_000).unwrap();

        assert_eq!(pool.by_invoice("INV-9").map(|t| t.id()).collect::<Vec<_>>(), vec![invoiced_id]);
        assert_eq!(pool.by_invoice("INV-8").count(), 0);

        // A block spent the coin the plain transaction wanted: only that one goes
        let outpoint = Outpoint { transaction_id: Hash([2u8; 32]), index: 0 };
        assert_eq!(pool.remove_spender(&outpoint), Some(plain_id));
        assert_eq!(pool.remove_spender(&outpoint), None);
        assert!(pool.contains(&invoiced_id));

        pool.remove(&invoiced_id);
        assert_eq!(pool.by_invoice("INV-9").count(), 0);
        assert!(pool.is_empty());
    }

    #[test]
    fn retain_drops_stale_transactions() {
        let mut pool = Mempool::default().with_min_fee_rate(1);
        let keep = pool.insert(tx(1, 0), 1_000).unwrap();
        let drop = pool.insert(tx(2, 0), 1_000).unwrap();
        pool.retain(|t| t.id() == keep);
        assert!(pool.contains(&keep) && !pool.contains(&drop));
        assert!(pool.insert(tx(2, 3), 1_000).is_ok());
    }
}
