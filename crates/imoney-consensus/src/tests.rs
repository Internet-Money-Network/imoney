use crate::daa::{work_from_bits, DaaParams};
use crate::dag::Dag;
use crate::ghostdag::{GhostdagData, GhostdagError, GhostdagParams};
use imoney_core::{Decode, Encode, Hash};
use imoney_pow::compact_to_u256;

const EASY_BITS: u32 = 0x207fffff;
const GENESIS: u8 = 0;

fn h(id: u8) -> Hash {
    Hash([id; 32])
}

fn dag_with_k(k: u64) -> Dag {
    dag_with_genesis_bits(k, EASY_BITS)
}

fn dag_with_genesis_bits(k: u64, bits: u32) -> Dag {
    let mut dag = Dag::new(GhostdagParams { k, mergeset_size_limit: 10 * k as usize });
    dag.insert_genesis(h(GENESIS), 0, bits);
    dag
}

/// Adds a block and returns its GHOSTDAG data.
fn add_at(dag: &mut Dag, id: u8, parents: &[u8], timestamp_ms: u64, bits: u32) -> GhostdagData {
    let parents: Vec<Hash> = parents.iter().map(|p| h(*p)).collect();
    dag.check_parents(&parents).expect("valid parents");
    let ghostdag = dag.ghostdag(&parents).expect("ghostdag");
    let daa_score = dag.daa_score(&ghostdag);
    dag.insert(h(id), parents, timestamp_ms, bits, daa_score, ghostdag.clone());
    ghostdag
}

fn add(dag: &mut Dag, id: u8, parents: &[u8]) -> GhostdagData {
    add_at(dag, id, parents, id as u64 * 5_000, EASY_BITS)
}

#[test]
fn chain_accumulates_blue_score_and_work() {
    let mut dag = dag_with_k(8);
    add(&mut dag, 1, &[GENESIS]);
    add(&mut dag, 2, &[1]);
    let data = add(&mut dag, 3, &[2]);

    assert_eq!(data.selected_parent, h(2));
    assert_eq!(data.blue_score, 3);
    assert_eq!(data.blue_work, 3 * work_from_bits(EASY_BITS));
    assert!(data.mergeset_reds.is_empty());
    assert_eq!(dag.get(&h(3)).daa_score, 3);
    assert_eq!(dag.get(&h(3)).level, 3);
}

#[test]
fn parallel_blocks_are_both_merged_as_blue() {
    let mut dag = dag_with_k(8);
    add(&mut dag, 1, &[GENESIS]);
    add(&mut dag, 2, &[GENESIS]);
    let data = add(&mut dag, 3, &[1, 2]);

    // Equal work: the higher hash is the selected parent
    assert_eq!(data.selected_parent, h(2));
    assert_eq!(data.mergeset_blues, vec![h(2), h(1)]);
    assert!(data.mergeset_reds.is_empty());
    assert_eq!(data.blue_score, 3); // genesis, 1 and 2
    assert_eq!(dag.get(&h(3)).daa_score, 3);
    assert_eq!(data.ordered_mergeset(&dag), vec![(h(2), true), (h(1), true)]);
}

#[test]
fn blocks_outside_the_k_cluster_are_red() {
    // An honest chain 1 <- 2 <- 3 and three blocks 11, 12, 13 mined directly on genesis
    let build = |k: u64| {
        let mut dag = dag_with_k(k);
        add(&mut dag, 1, &[GENESIS]);
        add(&mut dag, 2, &[1]);
        add(&mut dag, 3, &[2]);
        for side in [11, 12, 13] {
            add(&mut dag, side, &[GENESIS]);
        }
        let data = add(&mut dag, 20, &[3, 11, 12, 13]);
        (dag, data)
    };

    // k = 1: every side block has three chain blues in its anticone
    let (_, strict) = build(1);
    assert_eq!(strict.selected_parent, h(3));
    assert_eq!(strict.mergeset_blues, vec![h(3)]);
    assert_eq!(strict.mergeset_reds.len(), 3);
    assert_eq!(strict.blue_score, 4);

    // k = 3: one side block fits; the next would give it four blues in its anticone
    let (dag, relaxed) = build(3);
    assert_eq!(relaxed.mergeset_blues.len(), 2);
    assert_eq!(relaxed.mergeset_reds.len(), 2);
    assert_eq!(relaxed.blue_score, 5);
    // Red blocks add no work, but they still count towards the DAA score
    assert_eq!(relaxed.blue_work, 5 * work_from_bits(EASY_BITS));
    assert_eq!(dag.get(&h(20)).daa_score, 7);
    // The chain blues each gained the blue side block in their anticone
    let blue_side = relaxed.mergeset_blues[1];
    assert_eq!(relaxed.blues_anticone_sizes[&blue_side], 3);
    assert_eq!(relaxed.blues_anticone_sizes[&h(3)], 1);
    assert_eq!(relaxed.blues_anticone_sizes[&h(1)], 1);
}

#[test]
fn heavier_branch_becomes_the_selected_parent() {
    let mut dag = dag_with_k(8);
    add(&mut dag, 1, &[GENESIS]);
    add(&mut dag, 2, &[1]);
    add(&mut dag, 9, &[GENESIS]); // highest hash, but less work behind it
    let data = add(&mut dag, 10, &[2, 9]);
    assert_eq!(data.selected_parent, h(2));
    assert_eq!(dag.best_of([h(2), h(9)].iter()), Some(h(2)));
}

#[test]
fn parent_rules_are_enforced() {
    let mut dag = dag_with_k(8);
    add(&mut dag, 1, &[GENESIS]);
    add(&mut dag, 2, &[1]);

    assert_eq!(dag.check_parents(&[]), Err(GhostdagError::NoParents));
    assert_eq!(dag.check_parents(&[h(77)]), Err(GhostdagError::UnknownParent(h(77))));
    assert_eq!(dag.check_parents(&[h(2), h(2)]), Err(GhostdagError::DuplicateParent(h(2))));
    assert_eq!(dag.check_parents(&[h(2), h(1)]), Err(GhostdagError::ParentIsAncestor(h(1), h(2))));
    assert_eq!(dag.check_parents(&[h(2)]), Ok(()));
}

#[test]
fn ancestry_follows_every_parent_edge() {
    let mut dag = dag_with_k(8);
    add(&mut dag, 1, &[GENESIS]);
    add(&mut dag, 2, &[GENESIS]);
    add(&mut dag, 3, &[1, 2]);
    add(&mut dag, 4, &[1]);

    assert!(dag.is_ancestor(&h(GENESIS), &h(3)));
    assert!(dag.is_ancestor(&h(2), &h(3)));
    assert!(dag.is_ancestor(&h(3), &h(3)));
    assert!(!dag.is_ancestor(&h(2), &h(4)));
    assert!(!dag.is_ancestor(&h(3), &h(1)));
}

#[test]
fn oversized_mergeset_is_rejected() {
    let mut dag = Dag::new(GhostdagParams { k: 8, mergeset_size_limit: 3 });
    dag.insert_genesis(h(GENESIS), 0, EASY_BITS);
    for side in 1..=4 {
        add(&mut dag, side, &[GENESIS]);
    }
    assert!(dag.ghostdag(&[h(1), h(2), h(3)]).is_ok());
    assert_eq!(dag.ghostdag(&[h(1), h(2), h(3), h(4)]), Err(GhostdagError::MergesetTooLarge(3)));
}

#[test]
fn ghostdag_data_round_trips() {
    let mut dag = dag_with_k(3);
    add(&mut dag, 1, &[GENESIS]);
    add(&mut dag, 2, &[GENESIS]);
    let data = add(&mut dag, 3, &[1, 2]);
    assert_eq!(GhostdagData::from_bytes(&data.to_bytes()).unwrap(), data);
}

/// Builds a chain of `count` blocks spaced `interval_ms` apart and returns the bits expected next.
fn bits_after_chain(count: u8, interval_ms: u64, bits: u32, params: &DaaParams) -> u32 {
    let mut dag = dag_with_genesis_bits(8, bits);
    let mut data = GhostdagData::default();
    for id in 1..=count {
        data = add_at(&mut dag, id, &[id - 1], id as u64 * interval_ms, bits);
    }
    // The next block would have the last block as its only parent
    let next = dag.ghostdag(&[h(count)]).unwrap();
    assert_eq!(next.selected_parent, h(count));
    assert_eq!(next.blue_score, data.blue_score + 1);
    dag.expected_bits(&next, params)
}

#[test]
fn difficulty_tracks_block_rate() {
    let params = DaaParams::new(EASY_BITS);
    let harder_start = 0x1f00ffff;
    let target = |bits: u32| compact_to_u256(bits);

    // On schedule: unchanged
    assert_eq!(bits_after_chain(30, 5_000, harder_start, &params), harder_start);
    // Twice as fast: the target halves (difficulty doubles)
    let fast = bits_after_chain(30, 2_500, harder_start, &params);
    assert!(target(fast) < target(harder_start));
    assert_eq!(fast, 0x1e7fff80);
    // A thousand times too fast: one block may only double the difficulty
    assert_eq!(bits_after_chain(30, 5, harder_start, &params), 0x1e7fff80);
    // Far too slow: one block may only halve it
    assert_eq!(bits_after_chain(30, 5_000_000, harder_start, &params), 0x1f01fffe);
    // Too slow: the target grows
    assert!(target(bits_after_chain(30, 10_000, harder_start, &params)) > target(harder_start));
    // Never easier than the maximum target
    assert_eq!(bits_after_chain(30, 60_000, EASY_BITS, &params), EASY_BITS);
}

#[test]
fn difficulty_is_fixed_for_short_windows_and_when_retargeting_is_off() {
    let params = DaaParams::new(EASY_BITS);
    assert_eq!(bits_after_chain(3, 1, 0x1f00ffff, &params), EASY_BITS);

    let fixed = DaaParams { retarget: false, ..DaaParams::new(EASY_BITS) };
    assert_eq!(bits_after_chain(30, 1, EASY_BITS, &fixed), EASY_BITS);
}

#[test]
fn work_and_median_time() {
    assert_eq!(work_from_bits(EASY_BITS), 2);
    assert!(work_from_bits(0x1f00ffff) > work_from_bits(EASY_BITS));

    let mut dag = dag_with_k(8);
    for id in 1..=5u8 {
        add_at(&mut dag, id, &[id - 1], id as u64 * 1_000, EASY_BITS);
    }
    let next = dag.ghostdag(&[h(5)]).unwrap();
    // Window is blocks 5, 4, 3, 2, 1, genesis: timestamps 5000..0, median 3000
    assert_eq!(dag.past_median_time(&next), 3_000);
}

/// Small deterministic generator, so a failing case can be reproduced from its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound
    }
}

/// Everything in the past of `hash`, by walking every parent edge.
fn past(dag: &Dag, hash: &Hash) -> std::collections::HashSet<Hash> {
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![*hash];
    while let Some(current) = stack.pop() {
        for parent in &dag.get(&current).parents {
            if seen.insert(*parent) {
                stack.push(*parent);
            }
        }
    }
    seen
}

/// A random DAG as a list of (block, parents), parents always earlier in the list. Blocks
/// pick parents among recent tips, with the occasional stale one, to get forks and reds.
fn random_dag(seed: u64, blocks: u8, k: u64) -> (Dag, Vec<(u8, Vec<u8>)>) {
    let mut rng = Lcg(seed);
    let mut dag = dag_with_k(k);
    let mut shape: Vec<(u8, Vec<u8>)> = Vec::new();
    for id in 1..=blocks {
        let existing: Vec<u8> = (0..id).collect();
        let mut parents: Vec<u8> = Vec::new();
        for _ in 0..1 + rng.next(3) {
            // Usually one of the last few blocks, sometimes any block at all
            let window = if rng.next(6) == 0 { existing.len() } else { existing.len().min(4) };
            let candidate = existing[existing.len() - 1 - rng.next(window)];
            let hashes: Vec<Hash> = parents.iter().chain([&candidate]).map(|p| h(*p)).collect();
            if !parents.contains(&candidate) && dag.check_parents(&hashes).is_ok() && dag.ghostdag(&hashes).is_ok() {
                parents.push(candidate);
            }
        }
        if parents.is_empty() {
            parents.push(id - 1);
        }
        add(&mut dag, id, &parents);
        shape.push((id, parents));
    }
    (dag, shape)
}

#[test]
fn random_dags_keep_the_ghostdag_invariants() {
    for seed in 0..40u64 {
        let k = 1 + seed % 4;
        let (dag, shape) = random_dag(seed, 60, k);

        for (id, _) in &shape {
            let block = h(*id);
            let data = &dag.get(&block).ghostdag;
            let block_past = past(&dag, &block);

            // The mergeset is exactly what the block adds to its selected parent's past
            let mut selected_past = past(&dag, &data.selected_parent);
            selected_past.insert(data.selected_parent);
            let mut expected: Vec<Hash> = block_past.difference(&selected_past).copied().collect();
            expected.sort();
            let mut merged: Vec<Hash> =
                data.mergeset_blues.iter().skip(1).chain(&data.mergeset_reds).copied().collect();
            merged.sort();
            assert_eq!(merged, expected, "seed {} block {}", seed, id);

            // The blue set: this block's blues and those of every selected-chain ancestor
            let mut blues: Vec<Hash> = Vec::new();
            let mut cursor = data;
            loop {
                blues.extend(&cursor.mergeset_blues);
                if cursor.is_genesis() {
                    break;
                }
                cursor = &dag.get(&cursor.selected_parent).ghostdag;
            }
            assert_eq!(data.blue_score as usize, blues.len(), "seed {} block {}", seed, id);

            // No blue block has more than k other blues it is neither before nor after
            let pasts: Vec<_> = blues.iter().map(|blue| past(&dag, blue)).collect();
            for (i, blue) in blues.iter().enumerate() {
                let anticone = blues
                    .iter()
                    .enumerate()
                    .filter(|(j, other)| *j != i && !pasts[i].contains(*other) && !pasts[*j].contains(blue))
                    .count();
                assert!(anticone as u64 <= k, "seed {} block {}: blue anticone {} > k {}", seed, id, anticone, k);
            }
        }
    }
}

#[test]
fn ghostdag_does_not_depend_on_the_order_blocks_arrive_in() {
    for seed in 100..120u64 {
        let (dag, shape) = random_dag(seed, 50, 3);

        // Rebuild with blocks arriving in a different order that still puts parents first
        let mut rng = Lcg(seed ^ 0xabcdef);
        let mut waiting = shape.clone();
        let mut rebuilt = dag_with_k(3);
        let mut placed = vec![GENESIS];
        while !waiting.is_empty() {
            let ready: Vec<usize> = (0..waiting.len())
                .filter(|i| waiting[*i].1.iter().all(|p| placed.contains(p)))
                .collect();
            let (id, parents) = waiting.remove(ready[rng.next(ready.len())]);
            add(&mut rebuilt, id, &parents);
            placed.push(id);
        }

        for (id, _) in &shape {
            assert_eq!(rebuilt.get(&h(*id)).ghostdag, dag.get(&h(*id)).ghostdag, "seed {} block {}", seed, id);
            assert_eq!(rebuilt.get(&h(*id)).daa_score, dag.get(&h(*id)).daa_score);
        }
    }
}
