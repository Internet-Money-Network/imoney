use crate::dag::Dag;
use crate::ghostdag::GhostdagData;
use imoney_core::constants::TARGET_TIME_PER_BLOCK_MS;
use imoney_core::Hash;
use imoney_pow::{compact_to_u256, u256_to_compact};
use primitive_types::{U256, U512};

/// Time over which a steady error in the block rate changes the difficulty by a factor of two.
/// At 100 seconds (20 blocks) the difficulty follows a fifty-fold jump in hashrate within about
/// two minutes and recovers from a fifty-fold drop within about ten, while ordinary luck in
/// block times moves it by under a fifth.
pub const DAA_HALF_LIFE_MS: u64 = 100_000;

/// Number of recent blocks whose median timestamp a new block must exceed.
pub const MEDIAN_TIME_WINDOW: usize = 41;

/// Difficulty adjustment parameters.
#[derive(Clone, Debug)]
pub struct DaaParams {
    pub target_time_per_block_ms: u64,
    /// See `DAA_HALF_LIFE_MS`.
    pub half_life_ms: u64,
    /// The easiest allowed target, in compact form.
    pub max_target_bits: u32,
    /// When false the difficulty never changes (for tests and local development).
    pub retarget: bool,
}

impl DaaParams {
    pub fn new(max_target_bits: u32) -> Self {
        Self {
            target_time_per_block_ms: TARGET_TIME_PER_BLOCK_MS,
            half_life_ms: DAA_HALF_LIFE_MS,
            max_target_bits,
            retarget: true,
        }
    }
}

/// One in 16.16 fixed point.
const ONE: i128 = 1 << 16;

/// `2^(exponent / 65536)` in 16.16 fixed point, for an exponent between -1 and 1 (also 16.16).
/// The fractional power is a cubic fitted to `2^x - 1` on [0, 1), accurate to about 0.01%; it
/// is the approximation Bitcoin Cash's difficulty rule uses. Integer arithmetic only, so every
/// node gets the same answer.
fn pow2_fixed(exponent: i128) -> u128 {
    let exponent = exponent.clamp(-ONE, ONE);
    // Split into a whole power of two and a fraction in [0, 1)
    let whole = exponent.div_euclid(ONE);
    let frac = exponent.rem_euclid(ONE) as u128;
    let factor = (1u128 << 16)
        + ((195_766_423_245_049u128 * frac + 971_821_376u128 * frac * frac + 5_127u128 * frac * frac * frac + (1u128 << 47)) >> 48);
    match whole {
        1 => factor << 1,
        0 => factor,
        _ => factor >> 1,
    }
}

fn target_from_bits(bits: u32) -> U256 {
    U256::from_big_endian(&compact_to_u256(bits))
}

/// Expected number of hashes needed to meet a target: 2^256 / (target + 1).
pub fn work_from_bits(bits: u32) -> u128 {
    let target = target_from_bits(bits);
    // (2^256 - 1 - target) / (target + 1) + 1 equals 2^256 / (target + 1) without overflowing
    let work = match target.checked_add(U256::one()) {
        Some(divisor) => (!target / divisor) + U256::one(),
        None => U256::one(),
    };
    if work > U256::from(u128::MAX) {
        u128::MAX
    } else {
        work.as_u128()
    }
}

impl Dag {
    /// The most recent blocks in the past of a block with the given GHOSTDAG data,
    /// newest first, up to `size` blocks. Genesis is left out: it was not mined, and its
    /// fixed timestamp says nothing about how fast blocks are being found.
    pub fn block_window(&self, ghostdag: &GhostdagData, size: usize) -> Vec<Hash> {
        let mut window = Vec::with_capacity(size);
        let mut data = ghostdag;
        loop {
            for (hash, _) in data.ordered_mergeset(self) {
                if self.get(&hash).parents.is_empty() {
                    continue;
                }
                window.push(hash);
                if window.len() == size {
                    return window;
                }
            }
            if data.is_genesis() {
                return window;
            }
            data = &self.get(&data.selected_parent).ghostdag;
        }
    }

    /// DAA score of a block: the number of blocks in its past.
    pub fn daa_score(&self, ghostdag: &GhostdagData) -> u64 {
        self.get(&ghostdag.selected_parent).daa_score + ghostdag.mergeset_size() as u64
    }

    /// Median timestamp of the recent past. A new block's timestamp must be later than this.
    pub fn past_median_time(&self, ghostdag: &GhostdagData) -> u64 {
        let mut timestamps: Vec<u64> = self
            .block_window(ghostdag, MEDIAN_TIME_WINDOW)
            .iter()
            .map(|h| self.get(h).timestamp_ms)
            .collect();
        timestamps.sort_unstable();
        match timestamps.get(timestamps.len() / 2) {
            Some(median) => *median,
            // Only genesis is in the past
            None => self.get(&ghostdag.selected_parent).timestamp_ms,
        }
    }

    /// The difficulty a block with the given GHOSTDAG data must have.
    ///
    /// Each selected-chain block moves the target from its predecessor's by how far that step
    /// was off schedule. The step from the selected parent's own selected parent to the
    /// selected parent added `n` blocks to the DAG (the selected parent and everything it
    /// merged), which should have taken `n` block intervals. If it took longer the target
    /// eases, if shorter it tightens: `target = parent target * 2^((elapsed - n * interval) /
    /// half life)`, moving at most a factor of two per step.
    ///
    /// The rule looks only at the last step. That makes it quick in both directions, and
    /// because one step can at most double or halve the target, a long silence (everyone
    /// stops mining for a day) eases the difficulty once instead of leaving a backlog of
    /// easy blocks to be mined in a burst, and one false timestamp can move the next block's
    /// difficulty by no more than that. Integer arithmetic only.
    pub fn expected_bits(&self, ghostdag: &GhostdagData, params: &DaaParams) -> u32 {
        if !params.retarget {
            return params.max_target_bits;
        }
        let parent = self.get(&ghostdag.selected_parent);
        // The first block follows genesis, whose fixed timestamp measures nothing
        if parent.ghostdag.is_genesis() || parent.parents.is_empty() {
            return params.max_target_bits;
        }
        let grandparent = self.get(&parent.ghostdag.selected_parent);

        let elapsed_ms = parent.timestamp_ms as i128 - grandparent.timestamp_ms as i128;
        let scheduled_ms = parent.ghostdag.mergeset_size() as i128 * params.target_time_per_block_ms as i128;
        let exponent = ((elapsed_ms - scheduled_ms) * ONE / params.half_life_ms.max(1) as i128).clamp(-ONE, ONE);

        let max_target = target_from_bits(params.max_target_bits);
        let scaled = (U512::from(target_from_bits(parent.bits)) * U512::from(pow2_fixed(exponent))) >> 16;
        let new_target = if scaled > U512::from(max_target) {
            max_target
        } else {
            U256::try_from(scaled).unwrap_or(max_target).max(U256::one())
        };

        let mut bytes = [0u8; 32];
        new_target.to_big_endian(&mut bytes);
        // Compact form truncates; never report a target of zero or above the maximum
        let bits = u256_to_compact(&bytes);
        if bits == 0 || target_from_bits(bits) > max_target {
            params.max_target_bits
        } else {
            bits
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_power_of_two_is_accurate() {
        assert_eq!(pow2_fixed(0), 1 << 16);
        assert_eq!(pow2_fixed(ONE), 2 << 16);
        assert_eq!(pow2_fixed(-ONE), 1 << 15);
        // Beyond one doubling it stays at one doubling
        assert_eq!(pow2_fixed(5 * ONE), 2 << 16);
        assert_eq!(pow2_fixed(-5 * ONE), 1 << 15);
        for step in -64..=64i128 {
            let exponent = step * ONE / 64;
            let exact = 2f64.powf(exponent as f64 / 65536.0) * 65536.0;
            let got = pow2_fixed(exponent) as f64;
            assert!((got - exact).abs() / exact < 0.0002, "2^({}/64): {} against {}", step, got, exact);
        }
        // More time always means an easier target
        let mut last = 0;
        for exponent in (-ONE..=ONE).step_by(97) {
            let value = pow2_fixed(exponent);
            assert!(value >= last);
            last = value;
        }
    }
}
