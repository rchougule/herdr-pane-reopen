//! State files. Everything lives under `$HERDR_PLUGIN_STATE_DIR`.
//!
//! Rules: write-temp-then-rename for every file; one advisory `flock` on a dedicated
//! `reopen.lock` around every read-modify-write (never on a data file — rename swaps
//! inodes). A missing or corrupt file degrades to the default value plus a log line.

use crate::model::*;
use crate::{lwarn, now_ms, SELF_CREATED_TTL_MS};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SNAPSHOT: &str = "snapshot.json";
/// The last snapshot taken *before* something disappeared — the close hook's fallback
/// when the daemon has already refreshed the live snapshot past the close.
pub const PREV_SNAPSHOT: &str = "snapshot.prev.json";
pub const CLOSED: &str = "closed.json";
pub const PENDING: &str = "pending.json";
pub const SELF_CREATED: &str = "self_created.json";
pub const WATCH_LOCK: &str = "watch.lock";
pub const LOCK: &str = "reopen.lock";
pub const BOOT_MARKER: &str = "recovered.marker";
pub const DAEMON_LOG: &str = "reopen.log";
pub const DAEMON_LOG_1: &str = "reopen.log.1";

/// Held for the duration of a read-modify-write.
pub struct Guard {
    file: File,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let _ = std::fs::create_dir_all(&dir);
        Store { dir }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Blocking exclusive lock. On failure we proceed unlocked rather than panicking:
    /// a degraded write beats a dead hook.
    pub fn lock(&self) -> Option<Guard> {
        let p = self.path(LOCK);
        let file = match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&p)
        {
            Ok(f) => f,
            Err(e) => {
                lwarn!("cannot open lock {}: {e}", p.display());
                return None;
            }
        };
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive) {
            Ok(()) => Some(Guard { file }),
            Err(e) => {
                lwarn!("flock failed: {e}");
                None
            }
        }
    }

    pub fn read_json<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        let p = self.path(name);
        match std::fs::read_to_string(&p) {
            Ok(s) if !s.trim().is_empty() => match serde_json::from_str(&s) {
                Ok(v) => v,
                Err(e) => {
                    lwarn!("corrupt {name} ({e}); starting empty");
                    T::default()
                }
            },
            _ => T::default(),
        }
    }

    pub fn write_json<T: Serialize>(&self, name: &str, value: &T) -> std::io::Result<()> {
        write_atomic(
            &self.dir,
            name,
            &serde_json::to_vec_pretty(value).unwrap_or_default(),
        )
    }

    // ---- typed helpers ----

    pub fn snapshot(&self) -> Snapshot {
        self.read_json(SNAPSHOT)
    }
    pub fn write_snapshot(&self, s: &Snapshot) -> std::io::Result<()> {
        self.write_json(SNAPSHOT, s)
    }
    pub fn prev_snapshot(&self) -> Snapshot {
        self.read_json(PREV_SNAPSHOT)
    }
    pub fn write_prev_snapshot(&self, s: &Snapshot) -> std::io::Result<()> {
        self.write_json(PREV_SNAPSHOT, s)
    }
    pub fn closed(&self) -> ClosedStack {
        self.read_json(CLOSED)
    }
    pub fn write_closed(&self, s: &ClosedStack) -> std::io::Result<()> {
        self.write_json(CLOSED, s)
    }
    pub fn pending(&self) -> Pending {
        self.read_json(PENDING)
    }
    pub fn write_pending(&self, p: &Pending) -> std::io::Result<()> {
        self.write_json(PENDING, p)
    }

    pub fn self_created(&self) -> SelfCreatedFile {
        self.read_json(SELF_CREATED)
    }

    /// Record ids created by a restore so their close events are ignored for 5 s.
    pub fn record_self_created(&self, ids: &[String]) {
        let _g = self.lock();
        let mut f: SelfCreatedFile = self.read_json(SELF_CREATED);
        let now = now_ms();
        f.ids
            .retain(|e| now.saturating_sub(e.created_at_ms) < SELF_CREATED_TTL_MS);
        for id in ids {
            f.ids.push(SelfCreated {
                id: id.clone(),
                created_at_ms: now,
            });
        }
        let _ = self.write_json(SELF_CREATED, &f);
    }

    pub fn is_self_created(&self, id: &str, now: u64) -> bool {
        self.self_created()
            .ids
            .iter()
            .any(|e| e.id == id && now.saturating_sub(e.created_at_ms) < SELF_CREATED_TTL_MS)
    }
}

/// write `dir/name.tmp-<pid>` then rename over `dir/name`.
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(name))
}

/// Drop entries older than the TTL (herdr recycles ids).
pub fn prune_entries(stack: &mut ClosedStack, now: u64, ttl_hours: u64, cap: usize) {
    let ttl_ms = ttl_hours.saturating_mul(3_600_000);
    stack
        .entries
        .retain(|e| ttl_ms == 0 || now.saturating_sub(e.closed_at_ms) <= ttl_ms);
    stack.entries.truncate(cap);
}

/// Never reuses an id: the persisted allocator wins over anything derived from the
/// (prunable) entries.
pub fn next_entry_id(stack: &ClosedStack) -> u64 {
    let derived = stack.entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
    derived.max(stack.next_id).max(1)
}
