use crate::hash::Hash;
use crate::serialize::tagged_hash;

const LEAF_CONTEXT: &str = "IMN 2026 merkle leaf";
const NODE_CONTEXT: &str = "IMN 2026 merkle node";

/// Computes the merkle root of a list of hashes.
///
/// Leaves and inner nodes are hashed under different domains, and an unpaired node is carried
/// up unchanged rather than duplicated, so two different lists cannot share a root.
/// An empty list has the root `Hash::ZERO`.
pub fn merkle_root(leaves: &[Hash]) -> Hash {
    if leaves.is_empty() {
        return Hash::ZERO;
    }

    let mut level: Vec<Hash> = leaves.iter().map(|leaf| tagged_hash(LEAF_CONTEXT, &[&leaf.0])).collect();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| match pair {
                [left, right] => tagged_hash(NODE_CONTEXT, &[&left.0, &right.0]),
                [single] => *single,
                _ => unreachable!(),
            })
            .collect();
    }
    level[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> Hash {
        Hash([byte; 32])
    }

    #[test]
    fn root_depends_on_content_order_and_length() {
        assert_eq!(merkle_root(&[]), Hash::ZERO);
        assert_ne!(merkle_root(&[h(1)]), h(1));
        assert_ne!(merkle_root(&[h(1), h(2)]), merkle_root(&[h(2), h(1)]));
        // Duplicating the last leaf must change the root
        assert_ne!(merkle_root(&[h(1), h(2), h(3)]), merkle_root(&[h(1), h(2), h(3), h(3)]));
        assert_eq!(merkle_root(&[h(1), h(2), h(3)]), merkle_root(&[h(1), h(2), h(3)]));
    }
}
