//! reopen — undo-close for herdr panes, tabs and workspaces.
//!
//! The pure logic (granularity inference, coalescing, resume args, alias generation,
//! process-info child selection, layout tree conversion) lives in modules that need no
//! socket, so they are unit-testable from `tests/`.

pub mod agents;
pub mod close;
pub mod config;
pub mod daemon;
pub mod env;
pub mod log;
pub mod model;
pub mod restore;
pub mod rpc;
pub mod snapshot;
pub mod store;

/// Socket protocol this plugin was verified against (herdr 0.9.1).
pub const EXPECTED_PROTOCOL: u32 = 22;

/// Coalescing window for one close gesture, milliseconds.
pub const BURST_WINDOW_MS: u64 = 250;

/// A burst older than this is leftover from a crashed hook.
pub const BURST_STALE_MS: u64 = 5_000;

/// Ids created by a restore are ignored by capture for this long.
pub const SELF_CREATED_TTL_MS: u64 = 5_000;

/// Maximum entries kept on the undo stack.
pub const STACK_CAP: usize = 20;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
