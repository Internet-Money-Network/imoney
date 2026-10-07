use imoney_core::Hash;

/// Converts a compact 32-bit bits integer (Bitcoin/Kaspa style) into a 256-bit target array.
pub fn compact_to_u256(compact: u32) -> [u8; 32] {
    let size = (compact >> 24) as usize;
    let word = compact & 0x007f_ffff;

    let mut target = [0u8; 32];
    if size <= 3 {
        let shifted = word >> (8 * (3 - size));
        if size > 0 {
            target[32 - size..32].copy_from_slice(&(shifted as u32).to_be_bytes()[4 - size..4]);
        }
    } else if size <= 32 {
        let bytes = (word as u32).to_be_bytes();
        let start = 32 - size;
        target[start] = bytes[1];
        if start + 1 < 32 { target[start + 1] = bytes[2]; }
        if start + 2 < 32 { target[start + 2] = bytes[3]; }
    }
    target
}

/// Converts a 256-bit target into compact bits representation.
pub fn u256_to_compact(target: &[u8; 32]) -> u32 {
    let mut i = 0;
    while i < 32 && target[i] == 0 {
        i += 1;
    }
    if i == 32 {
        return 0;
    }

    let size = (32 - i) as u32;
    let mut word = (target[i] as u32) << 16;
    if i + 1 < 32 {
        word |= (target[i + 1] as u32) << 8;
    }
    if i + 2 < 32 {
        word |= target[i + 2] as u32;
    }

    if (word & 0x0080_0000) != 0 {
        word >>= 8;
        return ((size + 1) << 24) | word;
    }

    (size << 24) | word
}

/// Checks if a 32-byte hash meets the difficulty target defined by `bits`.
pub fn is_valid_pow(hash: &Hash, bits: u32) -> bool {
    let target = compact_to_u256(bits);
    for i in 0..32 {
        if hash.0[i] < target[i] {
            return true;
        } else if hash.0[i] > target[i] {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_form_round_trips() {
        for bits in [0x207fffffu32, 0x1f00ffff, 0x1e7fff80, 0x1d2b3c4d, 0x1b0404cb, 0x03123456, 0x04123456] {
            assert_eq!(u256_to_compact(&compact_to_u256(bits)), bits, "0x{:08x}", bits);
        }
        assert_eq!(u256_to_compact(&[0u8; 32]), 0);
    }

    #[test]
    fn compact_expands_to_the_expected_target() {
        let target = compact_to_u256(0x1d00ffff);
        // 0x00ffff followed by 26 zero bytes
        assert_eq!(&target[..6], &[0, 0, 0, 0, 0xff, 0xff]);
        assert!(target[6..].iter().all(|b| *b == 0));

        let easiest = compact_to_u256(0x207fffff);
        assert_eq!(&easiest[..3], &[0x7f, 0xff, 0xff]);
    }

    #[test]
    fn hash_must_not_exceed_the_target() {
        let bits = 0x1d00ffff;
        let target = compact_to_u256(bits);
        assert!(is_valid_pow(&Hash(target), bits)); // equal is valid

        let mut above = target;
        above[31] = 1;
        assert!(!is_valid_pow(&Hash(above), bits));

        let mut below = target;
        below[5] = 0xfe;
        assert!(is_valid_pow(&Hash(below), bits));
        assert!(is_valid_pow(&Hash([0u8; 32]), bits));
        assert!(!is_valid_pow(&Hash([0xff; 32]), 0x207fffff));
    }
}
