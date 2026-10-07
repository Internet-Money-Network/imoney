use crate::dag::Dag;
use imoney_core::constants::GHOSTDAG_K;
use imoney_core::serialize::{put_list, Decode, DecodeError, Encode, Reader};
use imoney_core::Hash;
use std::collections::{BTreeMap, HashSet, VecDeque};
use thiserror::Error;

/// Parameters for GHOSTDAG consensus.
#[derive(Clone, Debug)]
pub struct GhostdagParams {
    /// Largest blue anticone a blue block may have.
    pub k: u64,
    /// Largest number of blocks a single block may merge.
    pub mergeset_size_limit: usize,
}

impl Default for GhostdagParams {
    fn default() -> Self {
        Self {
            k: GHOSTDAG_K,
            mergeset_size_limit: 10 * GHOSTDAG_K as usize,
        }
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum GhostdagError {
    #[error("Block has no parents")]
    NoParents,
    #[error("Unknown parent block hash: {0}")]
    UnknownParent(Hash),
    #[error("Parent {0} is listed twice")]
    DuplicateParent(Hash),
    #[error("Parent {0} is an ancestor of parent {1}")]
    ParentIsAncestor(Hash, Hash),
    #[error("Block merges more than {0} blocks")]
    MergesetTooLarge(usize),
}

/// The GHOSTDAG view from one block: which blocks it merges and how they are coloured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GhostdagData {
    /// The parent with the most blue work. `Hash::ZERO` for genesis.
    pub selected_parent: Hash,
    /// Blue blocks merged by this block, in consensus order. Starts with the selected parent.
    pub mergeset_blues: Vec<Hash>,
    /// Red blocks merged by this block, in consensus order.
    pub mergeset_reds: Vec<Hash>,
    /// Number of blue blocks in this block's past.
    pub blue_score: u64,
    /// Total proof-of-work of the blue blocks in this block's past.
    pub blue_work: u128,
    /// For each blue in the mergeset (and each blue whose count grew), the size of its
    /// blue anticone as seen from this block.
    pub blues_anticone_sizes: BTreeMap<Hash, u32>,
}

impl GhostdagData {
    pub fn is_genesis(&self) -> bool {
        self.selected_parent == Hash::ZERO
    }

    pub fn mergeset_size(&self) -> usize {
        self.mergeset_blues.len() + self.mergeset_reds.len()
    }

    /// Every merged block in the order their transactions are applied: the selected parent
    /// first, then the rest by ascending blue work. The flag is true for blue blocks.
    pub fn ordered_mergeset(&self, dag: &Dag) -> Vec<(Hash, bool)> {
        let mut rest: Vec<(Hash, bool)> = self
            .mergeset_blues
            .iter()
            .skip(1)
            .map(|h| (*h, true))
            .chain(self.mergeset_reds.iter().map(|h| (*h, false)))
            .collect();
        rest.sort_by_key(|(h, _)| dag.sort_key(h));

        let mut ordered = Vec::with_capacity(rest.len() + 1);
        if let Some(selected_parent) = self.mergeset_blues.first() {
            ordered.push((*selected_parent, true));
        }
        ordered.extend(rest);
        ordered
    }
}

impl Encode for GhostdagData {
    fn encode(&self, out: &mut Vec<u8>) {
        self.selected_parent.encode(out);
        put_list(out, &self.mergeset_blues);
        put_list(out, &self.mergeset_reds);
        out.extend_from_slice(&self.blue_score.to_be_bytes());
        out.extend_from_slice(&self.blue_work.to_be_bytes());
        out.extend_from_slice(&(self.blues_anticone_sizes.len() as u32).to_be_bytes());
        for (hash, size) in &self.blues_anticone_sizes {
            hash.encode(out);
            out.extend_from_slice(&size.to_be_bytes());
        }
    }
}

impl Decode for GhostdagData {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let selected_parent = reader.hash()?;
        let mergeset_blues = reader.list(u32::MAX as usize)?;
        let mergeset_reds = reader.list(u32::MAX as usize)?;
        let blue_score = reader.u64()?;
        let blue_work = reader.u128()?;
        let count = reader.len(u32::MAX as usize)?;
        let mut blues_anticone_sizes = BTreeMap::new();
        for _ in 0..count {
            blues_anticone_sizes.insert(reader.hash()?, reader.u32()?);
        }
        Ok(Self {
            selected_parent,
            mergeset_blues,
            mergeset_reds,
            blue_score,
            blue_work,
            blues_anticone_sizes,
        })
    }
}

enum Colour {
    Blue(u32, BTreeMap<Hash, u32>),
    Red,
}

impl Dag {
    /// Checks the parent list of a new block: all known, no duplicates, none an ancestor of another.
    pub fn check_parents(&self, parents: &[Hash]) -> Result<(), GhostdagError> {
        if parents.is_empty() {
            return Err(GhostdagError::NoParents);
        }
        let mut seen = HashSet::new();
        for parent in parents {
            if !self.contains(parent) {
                return Err(GhostdagError::UnknownParent(*parent));
            }
            if !seen.insert(*parent) {
                return Err(GhostdagError::DuplicateParent(*parent));
            }
        }
        for a in parents {
            for b in parents {
                if a != b && self.is_ancestor(a, b) {
                    return Err(GhostdagError::ParentIsAncestor(*a, *b));
                }
            }
        }
        Ok(())
    }

    /// Runs GHOSTDAG for a block with the given parents, which must already pass `check_parents`.
    pub fn ghostdag(&self, parents: &[Hash]) -> Result<GhostdagData, GhostdagError> {
        let selected_parent = *parents
            .iter()
            .max_by_key(|p| self.sort_key(p))
            .ok_or(GhostdagError::NoParents)?;

        let mut candidates = self.mergeset_without_selected_parent(&selected_parent, parents)?;
        candidates.sort_by_key(|h| self.sort_key(h));

        let mut data = GhostdagData {
            selected_parent,
            mergeset_blues: vec![selected_parent],
            ..Default::default()
        };
        data.blues_anticone_sizes.insert(selected_parent, 0);

        for candidate in candidates {
            match self.colour_candidate(&data, &candidate) {
                Colour::Blue(anticone_size, changed) => {
                    data.mergeset_blues.push(candidate);
                    data.blues_anticone_sizes.insert(candidate, anticone_size);
                    // Each blue in the candidate's anticone now has one more blue in its own
                    for (blue, size) in changed {
                        data.blues_anticone_sizes.insert(blue, size + 1);
                    }
                }
                Colour::Red => data.mergeset_reds.push(candidate),
            }
        }

        let parent_data = &self.get(&selected_parent).ghostdag;
        data.blue_score = parent_data.blue_score + data.mergeset_blues.len() as u64;
        data.blue_work = data
            .mergeset_blues
            .iter()
            .fold(parent_data.blue_work, |work, blue| work.saturating_add(self.get(blue).work));
        Ok(data)
    }

    /// The blocks in the new block's past that are not in the selected parent's past,
    /// excluding the selected parent itself.
    fn mergeset_without_selected_parent(
        &self,
        selected_parent: &Hash,
        parents: &[Hash],
    ) -> Result<Vec<Hash>, GhostdagError> {
        let mut queue: VecDeque<Hash> = parents.iter().filter(|p| *p != selected_parent).copied().collect();
        let mut mergeset: HashSet<Hash> = queue.iter().copied().collect();
        let mut in_selected_past: HashSet<Hash> = HashSet::new();
        let limit = self.params.mergeset_size_limit;

        while let Some(current) = queue.pop_front() {
            for parent in &self.get(&current).parents {
                if mergeset.contains(parent) || in_selected_past.contains(parent) {
                    continue;
                }
                if self.is_ancestor(parent, selected_parent) {
                    in_selected_past.insert(*parent);
                    continue;
                }
                mergeset.insert(*parent);
                queue.push_back(*parent);
            }
            // The selected parent counts towards the limit
            if mergeset.len() + 1 > limit {
                return Err(GhostdagError::MergesetTooLarge(limit));
            }
        }
        Ok(mergeset.into_iter().collect())
    }

    /// Decides whether adding `candidate` to the blue set keeps every blue block's
    /// blue anticone at most k.
    fn colour_candidate(&self, new_data: &GhostdagData, candidate: &Hash) -> Colour {
        let k = self.params.k as u32;
        // The selected parent plus k blues is the most a mergeset can hold
        if new_data.mergeset_blues.len() as u32 == k + 1 {
            return Colour::Red;
        }

        let mut changed: BTreeMap<Hash, u32> = BTreeMap::new();
        let mut anticone_size: u32 = 0;

        // Walk the selected chain backwards, starting with the block being built
        let mut chain_hash: Option<Hash> = None;
        let mut chain_data = new_data;
        loop {
            // Once a chain block is in the candidate's past, so is everything behind it
            if let Some(hash) = chain_hash {
                if self.is_ancestor(&hash, candidate) {
                    break;
                }
            }

            for blue in &chain_data.mergeset_blues {
                if self.is_ancestor(blue, candidate) {
                    continue;
                }
                // `blue` is in the candidate's anticone
                let blue_anticone = self.blue_anticone_size(blue, new_data);
                changed.insert(*blue, blue_anticone);
                anticone_size += 1;
                if anticone_size > k || blue_anticone == k {
                    return Colour::Red;
                }
            }

            if chain_data.is_genesis() {
                break;
            }
            chain_hash = Some(chain_data.selected_parent);
            chain_data = &self.get(&chain_data.selected_parent).ghostdag;
        }

        Colour::Blue(anticone_size, changed)
    }

    /// The blue anticone size of `blue` as seen from the block being built.
    fn blue_anticone_size(&self, blue: &Hash, new_data: &GhostdagData) -> u32 {
        let mut data = new_data;
        loop {
            if let Some(size) = data.blues_anticone_sizes.get(blue) {
                return *size;
            }
            if data.is_genesis() {
                return 0;
            }
            data = &self.get(&data.selected_parent).ghostdag;
        }
    }
}
