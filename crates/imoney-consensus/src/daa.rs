use imoney_core::constants::TARGET_TIME_PER_BLOCK_MS;
use imoney_pow::{compact_to_u256, u256_to_compact};

/// The number of recent blocks used to calculate the rolling DAA difficulty window.
/// At 5 seconds per block, 144 blocks = 720 seconds (12 minutes).
pub const DAA_WINDOW_SIZE: usize = 144;

/// Computes the retargeted difficulty bits given the window duration and current target.
pub fn calculate_next_target_bits(
    current_bits: u32,
    actual_timespan_ms: u64,
) -> u32 {
    let target_timespan_ms = (DAA_WINDOW_SIZE as u64) * TARGET_TIME_PER_BLOCK_MS;

    // Dampen extreme fluctuations: maximum 2x adjustment upwards or downwards per window
    let clamped_timespan_ms = actual_timespan_ms.clamp(
        target_timespan_ms / 2,
        target_timespan_ms * 2,
    );

    let current_target = compact_to_u256(current_bits);
    
    // Convert 256-bit target bytes to big integer multiplication:
    // new_target = (current_target * clamped_timespan) / target_timespan
    let mut new_target = [0u8; 32];
    
    // Approximate scaling factor
    let ratio = (clamped_timespan_ms as f64) / (target_timespan_ms as f64);
    
    // Scale target representation smoothly
    let mut temp = [0u64; 4];
    for i in 0..4 {
        let chunk = &current_target[i * 8..(i + 1) * 8];
        temp[i] = u64::from_be_bytes(chunk.try_into().unwrap());
    }
    
    for i in 0..4 {
        let scaled = ((temp[i] as f64) * ratio) as u64;
        new_target[i * 8..(i + 1) * 8].copy_from_slice(&scaled.to_be_bytes());
    }

    u256_to_compact(&new_target)
}
