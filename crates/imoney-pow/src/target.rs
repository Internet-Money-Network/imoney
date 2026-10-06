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
