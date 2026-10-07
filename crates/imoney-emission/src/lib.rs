use imoney_core::constants::{
    BLOCKS_PER_HALVING_ERA, FLOOR_SUBSIDY_ATOMS, LAUNCH_BLOCK_SUBSIDY_ATOMS,
    ATOMS_PER_IMN,
};

/// Calculates the halving era index for a given cumulative block height / DAA score.
/// Era 0: Years 0..4 (Blocks 0..25,228,799)
/// Era 1: Years 4..8
/// Era 2: Years 8..12
/// Era 3: Years 12..16
/// Era 4+: Years 16+ (Permanent floor)
#[inline]
pub fn halving_era(daa_score: u64) -> u64 {
    daa_score / BLOCKS_PER_HALVING_ERA
}

/// Calculates the block reward subsidy in atomic units (atoms)
/// for a given block height / DAA score.
///
/// Guaranteed zero floating-point arithmetic.
pub fn block_subsidy_atoms(daa_score: u64) -> u64 {
    let era = halving_era(daa_score);
    match era {
        0 => LAUNCH_BLOCK_SUBSIDY_ATOMS,                 // 5.00000000 IMN (500M atoms)
        1 => LAUNCH_BLOCK_SUBSIDY_ATOMS / 2,             // 2.50000000 IMN (250M atoms)
        2 => LAUNCH_BLOCK_SUBSIDY_ATOMS / 4,             // 1.25000000 IMN (125M atoms)
        3 => LAUNCH_BLOCK_SUBSIDY_ATOMS / 8,             // 0.62500000 IMN (62.5M atoms)
        _ => FLOOR_SUBSIDY_ATOMS,                        // 0.31250000 IMN (31.25M atoms) - Floor forever
    }
}

/// Helper returning the block subsidy formatted as human-readable whole IMN coins.
pub fn block_subsidy_imn(daa_score: u64) -> f64 {
    (block_subsidy_atoms(daa_score) as f64) / (ATOMS_PER_IMN as f64)
}

/// Calculates the cumulative total supply mined up to a given DAA score.
pub fn cumulative_supply_atoms(daa_score: u64) -> u128 {
    let era = halving_era(daa_score);
    let mut total: u128 = 0;

    for e in 0..era {
        let subsidy = match e {
            0 => LAUNCH_BLOCK_SUBSIDY_ATOMS as u128,
            1 => (LAUNCH_BLOCK_SUBSIDY_ATOMS / 2) as u128,
            2 => (LAUNCH_BLOCK_SUBSIDY_ATOMS / 4) as u128,
            3 => (LAUNCH_BLOCK_SUBSIDY_ATOMS / 8) as u128,
            _ => FLOOR_SUBSIDY_ATOMS as u128,
        };
        total += subsidy * (BLOCKS_PER_HALVING_ERA as u128);
    }

    let remaining_blocks = (daa_score % BLOCKS_PER_HALVING_ERA) as u128;
    let current_subsidy = block_subsidy_atoms(daa_score) as u128;
    total += remaining_blocks * current_subsidy;

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use imoney_core::constants::BLOCKS_PER_YEAR;

    #[test]
    fn test_emission_schedule_milestones() {
        // Block 0: Launch (5 IMN)
        assert_eq!(block_subsidy_atoms(0), 500_000_000);
        assert_eq!(block_subsidy_imn(0), 5.0);

        // Year 1 (Block 6,307,200): Still 5 IMN
        assert_eq!(block_subsidy_atoms(BLOCKS_PER_YEAR), 500_000_000);

        // Year 4 exact boundary: Halves to 2.5 IMN
        assert_eq!(block_subsidy_atoms(BLOCKS_PER_HALVING_ERA), 250_000_000);
        assert_eq!(block_subsidy_imn(BLOCKS_PER_HALVING_ERA), 2.5);

        // Year 8: Halves to 1.25 IMN
        assert_eq!(block_subsidy_atoms(2 * BLOCKS_PER_HALVING_ERA), 125_000_000);

        // Year 12: Halves to 0.625 IMN
        assert_eq!(block_subsidy_atoms(3 * BLOCKS_PER_HALVING_ERA), 62_500_000);

        // Year 16+: Permanent floor at 0.3125 IMN
        assert_eq!(block_subsidy_atoms(4 * BLOCKS_PER_HALVING_ERA), 31_250_000);
        assert_eq!(block_subsidy_atoms(10 * BLOCKS_PER_HALVING_ERA), 31_250_000);
        assert_eq!(block_subsidy_imn(10 * BLOCKS_PER_HALVING_ERA), 0.3125);
    }

    #[test]
    fn test_annual_tail_emission() {
        // Year 16+ annual emission should be exactly ~1.97M IMN per year
        let annual_floor_atoms = (BLOCKS_PER_YEAR as u128) * (FLOOR_SUBSIDY_ATOMS as u128);
        let annual_floor_coins = (annual_floor_atoms as f64) / (ATOMS_PER_IMN as f64);
        assert!((annual_floor_coins - 1_971_000.0).abs() < 100.0);
    }
}
