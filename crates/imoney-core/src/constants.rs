//! Consensus and Network Constants for Internet Money (IMN).

/// Ticker symbol for the currency.
pub const TICKER: &str = "IMN";

/// Full human-readable currency name.
pub const CURRENCY_NAME: &str = "Internet Money";

/// Target block time in milliseconds: 5,000 ms (5 seconds).
pub const TARGET_TIME_PER_BLOCK_MS: u64 = 5_000;

/// Number of blocks per second: 0.2 BPS (1 block every 5 seconds).
pub const BLOCKS_PER_SECOND: f64 = 0.2;

/// Number of blocks expected per day: 17,280.
pub const BLOCKS_PER_DAY: u64 = (24 * 3600 * 1000) / TARGET_TIME_PER_BLOCK_MS;

/// Number of blocks per normal 365-day year: 6,307,200.
pub const BLOCKS_PER_YEAR: u64 = 365 * BLOCKS_PER_DAY;

/// Number of blocks in one 4-year halving era: 25,228,800 blocks.
pub const BLOCKS_PER_HALVING_ERA: u64 = 4 * BLOCKS_PER_YEAR;

/// Maximum number of direct parent blocks in a blockDAG header.
pub const MAX_BLOCK_PARENTS: usize = 16;

/// GHOSTDAG k-cluster parameter for 5-second blocks.
///
/// Because block spacing is 5 seconds and network latency is ~200-400ms (<8%),
/// k=8 guarantees >98% blue block convergence with negligible reorg risk.
pub const GHOSTDAG_K: u64 = 8;

/// Atoms per single whole coin (8 decimal places, same as Bitcoin).
pub const ATOMS_PER_IMN: u64 = 100_000_000;

/// Launch block reward: 5 IMN per 5-second block (500,000,000 atoms).
pub const LAUNCH_BLOCK_SUBSIDY_ATOMS: u64 = 5 * ATOMS_PER_IMN;

/// Permanent tail emission subsidy floor: 0.3125 IMN per block (31,250,000 atoms).
pub const FLOOR_SUBSIDY_ATOMS: u64 = ATOMS_PER_IMN * 5 / 16;

/// Maximum canonical-encoded size of a single transaction, in bytes.
pub const MAX_TX_BYTES: usize = 50_000;

/// Maximum canonical-encoded size of a block (header plus transactions), in bytes.
pub const MAX_BLOCK_BYTES: usize = 250_000;
