use imoney_core::constants::GHOSTDAG_K;
use imoney_core::Hash;
use std::collections::HashMap;

/// Parameters for GHOSTDAG consensus.
#[derive(Clone, Debug)]
pub struct GhostdagParams {
    pub k: u64,
}

impl Default for GhostdagParams {
    fn default() -> Self {
        Self { k: GHOSTDAG_K }
    }
}

/// The result of running GHOSTDAG selection on a block.
#[derive(Clone, Debug, Default)]
pub struct GhostdagResult {
    /// The virtual selected parent among the block's parents.
    pub selected_parent: Hash,
    /// Blocks in the past of this block that are colored Blue (ordered).
    pub blue_set: Vec<Hash>,
    /// Cumulative blue score.
    pub blue_score: u64,
    /// Cumulative blue work.
    pub blue_work: u128,
}

/// Evaluates GHOSTDAG coloring for a block given its parent DAG state.
pub fn order_ghostdag_parents(
    parents: &[Hash],
    parent_blue_scores: &HashMap<Hash, u64>,
    params: &GhostdagParams,
) -> GhostdagResult {
    if parents.is_empty() {
        return GhostdagResult::default();
    }

    // Selected parent is the parent with the highest blue score
    let selected_parent = *parents
        .iter()
        .max_by_key(|p| parent_blue_scores.get(p).copied().unwrap_or(0))
        .unwrap();

    let base_score = parent_blue_scores.get(&selected_parent).copied().unwrap_or(0);
    
    // In GHOSTDAG, candidate merges within cluster limit k are accepted as blue
    let mut blue_set = Vec::new();
    for p in parents {
        if *p != selected_parent && blue_set.len() < (params.k as usize) {
            blue_set.push(*p);
        }
    }

    let blue_score = base_score + 1 + (blue_set.len() as u64);

    GhostdagResult {
        selected_parent,
        blue_set,
        blue_score,
        blue_work: blue_score as u128 * 1000,
    }
}
