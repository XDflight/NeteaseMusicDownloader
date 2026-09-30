//! Tunable constants of the download engine.
//!
//! The values marked *chosen* were picked explicitly by the project owner; the rest are
//! internals of the chosen policy (see `adaptive.rs`) or structural limits.

use std::time::Duration;

// ---- chunking (chosen) ---------------------------------------------------------------------

/// Size of one HTTP `Range` request.
pub const CHUNK_SIZE: u64 = 512 * 1024;

// ---- connection budget (chosen: 1 / 2 / 8) -------------------------------------------------

pub const MIN_CONNECTIONS: usize = 1;
pub const INITIAL_CONNECTIONS: usize = 2;
pub const MAX_CONNECTIONS: usize = 8;

// ---- adaptation policy (chosen: "conservative") --------------------------------------------

/// Length of one measurement window.
pub const PROBE_WINDOW: Duration = Duration::from_secs(3);
/// Relative throughput gain a window must show for the previous +1 to count as worthwhile.
pub const GAIN_THRESHOLD: f64 = 0.15;
/// Connections added per successful probe.
pub const INCREASE_STEP: usize = 1;
/// Multiplier applied to the connection limit on errors or a throughput collapse.
pub const DECREASE_FACTOR: f64 = 0.5;

// ---- retry / timeout policy (chosen: "persistent") -----------------------------------------

pub const MAX_RETRIES: u32 = 8;
pub const BACKOFF_BASE: Duration = Duration::from_millis(500);
pub const BACKOFF_CAP: Duration = Duration::from_secs(15);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A chunk that receives no bytes for this long is considered failed.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(30);

// ---- internals of the conservative policy ---------------------------------------------------

/// After the controller settles, wait this many windows before probing +1 again.
pub const HOLD_WINDOWS: u32 = 6;
/// A window slower than `reference * DROP_TOLERANCE` counts as a throughput collapse.
pub const DROP_TOLERANCE: f64 = 0.8;
/// Connections must be busy at least this fraction of the time for a window to be informative.
pub const SATURATION_RATIO: f64 = 0.8;
/// Below this aggregate speed a window is treated as idle (nothing to learn from it).
pub const IDLE_BYTES_PER_SEC: f64 = 16.0 * 1024.0;
/// How often in-flight connections are sampled inside a window.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
