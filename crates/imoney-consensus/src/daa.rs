use crate::dag::Dag;
use crate::ghostdag::GhostdagData;
use imoney_core::constants::TARGET_TIME_PER_BLOCK_MS;
use imoney_core::Hash;
use imoney_pow::{compact_to_u256, u256_to_compact};
use primitive_types::{U256, U512};

/// The number of recent blocks used to calculate the rolling DAA difficulty window.
/// At 5 seconds per block, 144 blocks = 720 seconds (12 minutes).
pub const DAA_WINDOW_SIZE: usize = 144;

/// Number of recent blocks whose median timestamp a new block must exceed.
pub const MEDIAN_TIME_WINDOW: usize = 41;

/// Difficulty adjustment parameters.
#[derive(Clone, Debug)]
pub struct DaaParams {
    pub window_size: usize,
    /// Below this many blocks in the window the easiest target is used.
    pub min_window_size: usize,
    pub target_time_per_block_ms: u64,
    /// The easiest allowed target, in compact form.
    pub max_target_bits: u32,
    /// When false the difficulty never changes (for tests and local development).
    pub retarget: bool,
}

impl DaaParams {
    pub fn new(max_target_bits: u32) -> Self {
        Self {
            window_size: DAA_WINDOW_SIZE,
            min_window_size: 8,
            target_time_per_block_ms: TARGET_TIME_PER_BLOCK_MS,
            max_target_bits,
            retarget: true,
        }
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
    /// Measures the network's hashrate as the work done across the window divided by the time
    /// the window took, then picks the target that this hashrate meets once per block interval.
    /// Summing work (rather than averaging targets) keeps the estimate right when the window
    /// mixes very different difficulties, as it does just after launch. The result is limited to
    /// a 2x change from the selected parent's target. Integer arithmetic only.
    pub fn expected_bits(&self, ghostdag: &GhostdagData, params: &DaaParams) -> u32 {
        if !params.retarget {
            return params.max_target_bits;
        }
        let window = self.block_window(ghostdag, params.window_size);
        if window.len() < params.min_window_size.max(2) {
            return params.max_target_bits;
        }

        let mut oldest = self.get(&window[0]);
        let mut newest_time = 0u64;
        let mut total_work = U256::zero();
        for hash in &window {
            let block = self.get(hash);
            if block.timestamp_ms < oldest.timestamp_ms {
                oldest = block;
            }
            newest_time = newest_time.max(block.timestamp_ms);
            total_work += U256::from(block.work);
        }
        // The window spans the time after its oldest block, so that block's work is not part of it
        let work_in_span = total_work - U256::from(oldest.work);
        let actual_ms = (newest_time - oldest.timestamp_ms).max(1);

        // work per block = work_in_span * target_time / actual; target = 2^256 / work per block
        let max_target = target_from_bits(params.max_target_bits);
        let numerator = (U512::one() << 256) * U512::from(actual_ms);
        let denominator = U512::from(work_in_span) * U512::from(params.target_time_per_block_ms);
        let new_target = match numerator.checked_div(denominator) {
            Some(target) if target <= U512::from(max_target) => U256::try_from(target).unwrap_or(max_target),
            _ => max_target,
        };

        // A single block may move the target at most 2x from its selected parent's target.
        // One unusually fast or slow block is weak evidence, and this stops it from causing
        // a large swing, while still letting difficulty double every block when it must.
        let parent_target = target_from_bits(self.get(&ghostdag.selected_parent).bits);
        let lower = parent_target / 2;
        let upper = parent_target.checked_mul(U256::from(2u8)).map_or(max_target, |t| t.min(max_target));
        let new_target = new_target.clamp(lower, upper.max(lower));

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
