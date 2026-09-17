//! The `watch` daemon and the restart/crash recovery pass.
//!
//! The daemon exists for the events plugin hooks never receive (`layout.updated`,
//! `pane.updated`, `pane.moved`, `tab.moved`). It is self-healing: every non-close hook
//! calls `ensure_running`, so a herdr restart re-arms it within one event.

use crate::config::Config;
use crate::model::*;
use crate::rpc::{Client, Frame};
use crate::snapshot;
use crate::store::{self, Store};
use crate::{linfo, lwarn, now_ms};
use std::process::{Command, Stdio};

pub const HEARTBEAT_STALE_MS: u64 = 30_000;
pub const TICKER_SECS: u64 = 15;
pub const DEBOUNCE_MS: u64 = 150;
/// `reopen.log` is rotated once past this size.
pub const MAX_LOG_BYTES: u64 = 1_000_000;

pub const SUBSCRIPTIONS: &[&str] = &[
    "pane.created",
    "pane.closed",
    "pane.updated",
    "pane.focused",
    "pane.moved",
    "pane.exited",
    "pane.agent_detected",
    // NOTE: `pane.agent_status_changed`, `pane.scroll_changed` and `pane.output_matched`
    // require a `pane_id` in the subscription, so they cannot be subscribed globally.
    // The plugin hooks cover agent status anyway.
    "tab.created",
    "tab.closed",
    "tab.moved",
    "tab.renamed",
    "workspace.created",
    "workspace.closed",
    "workspace.renamed",
    "workspace.moved",
    "workspace.reordered",
    "layout.updated",
];

fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    match rustix::process::Pid::from_raw(pid) {
        Some(p) => rustix::process::test_kill_process(p).is_ok(),
        None => false,
    }
}

pub fn daemon_status(store: &Store) -> Option<(i32, bool)> {
    let lock: WatchLock = store.read_json(store::WATCH_LOCK);
    if lock.pid == 0 {
        return None;
    }
    let fresh = now_ms().saturating_sub(lock.updated_at) < HEARTBEAT_STALE_MS;
    Some((lock.pid, pid_alive(lock.pid) && fresh))
}

/// Spawn `reopen watch` detached (own session) unless a healthy one is already running.
pub fn ensure_running(store: &Store) {
    if let Some((_, true)) = daemon_status(store) {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            lwarn!("cannot find own exe: {e}");
            return;
        }
    };
    let log = store.path(store::DAEMON_LOG);
    // Only unbounded file in the plugin: one generation of rotation is plenty, herdr's
    // own `plugin log list` is the primary surface (F10).
    if std::fs::metadata(&log)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(&log, store.path(store::DAEMON_LOG_1));
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log);
    let mut cmd = Command::new(exe);
    cmd.arg("watch").stdin(Stdio::null());
    match out {
        Ok(f) => {
            let f2 = f.try_clone().ok();
            cmd.stdout(Stdio::from(f));
            if let Some(f2) = f2 {
                cmd.stderr(Stdio::from(f2));
            }
        }
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            let _ = rustix::process::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => linfo!("daemon spawned (pid {})", child.id()),
        Err(e) => lwarn!("cannot spawn daemon: {e}"),
    }
}

/// The daemon loop. Exits 0 on socket EOF so the next hook re-arms it.
pub fn watch(client: &Client, store: &Store, cfg: &Config) {
    let lock = WatchLock {
        pid: std::process::id() as i32,
        started_at: now_ms(),
        updated_at: now_ms(),
    };
    {
        let _g = store.lock();
        let _ = store.write_json(store::WATCH_LOCK, &lock);
    }
    linfo!("watch started (pid {})", lock.pid);

    let mut sub = match client.subscribe(SUBSCRIPTIONS) {
        Ok(s) => s,
        Err(e) => {
            lwarn!("events.subscribe failed: {e}");
            cleanup_lock(store);
            return;
        }
    };

    // Ticker thread: a pure safety net now that hooks cover every create/close.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let tick_tx = tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(TICKER_SECS));
        if tick_tx.send(()).is_err() {
            return;
        }
    });
    std::thread::spawn(move || {
        while let Some(frame) = sub.next_frame() {
            if let Frame::Event { .. } = frame {
                if tx.send(()).is_err() {
                    return;
                }
            }
        }
        // EOF: dropping tx ends the main loop.
    });

    loop {
        match rx.recv() {
            Ok(()) => {
                // debounce: swallow anything that arrives inside the window
                std::thread::sleep(std::time::Duration::from_millis(DEBOUNCE_MS));
                while rx.try_recv().is_ok() {}
                snapshot::refresh(client, store, cfg.process_info_ttl_ms);
                let _g = store.lock();
                let mut l: WatchLock = store.read_json(store::WATCH_LOCK);
                if l.pid != std::process::id() as i32 {
                    linfo!("another daemon owns the lock; exiting");
                    return;
                }
                l.updated_at = now_ms();
                let _ = store.write_json(store::WATCH_LOCK, &l);
            }
            Err(_) => {
                linfo!("subscription closed; exiting so the next hook re-arms");
                cleanup_lock(store);
                return;
            }
        }
    }
}

fn cleanup_lock(store: &Store) {
    let _g = store.lock();
    let l: WatchLock = store.read_json(store::WATCH_LOCK);
    if l.pid == std::process::id() as i32 {
        let _ = std::fs::remove_file(store.path(store::WATCH_LOCK));
    }
}

/// Restart/crash recovery. Runs on `startup`, and once per boot from the
/// first non-close hook (guarded by a marker file) so a server that never fires
/// `startup` still heals.
pub fn recover(store: &Store, client: &Client, cfg: &Config) {
    let _g = store.lock();
    let now = now_ms();

    let mut pending = store.pending();
    if let Some(b) = pending.burst.take() {
        if now.saturating_sub(b.first_ms) > crate::BURST_STALE_MS {
            let has_close = b
                .events
                .iter()
                .any(|e| crate::close::is_close_event(&e.name));
            if has_close {
                linfo!("recovering an abandoned burst from {}", b.first_ms);
                crate::close::finalize_locked(store, client, cfg, &b);
            } else {
                linfo!("dropping an abandoned burst with no close event");
            }
            let _ = store.write_pending(&Pending { burst: None });
        } else {
            pending.burst = Some(b);
        }
    }

    let _ = store.write_json(store::SELF_CREATED, &SelfCreatedFile::default());

    let lock: WatchLock = store.read_json(store::WATCH_LOCK);
    if lock.pid != 0
        && (!pid_alive(lock.pid) || now.saturating_sub(lock.updated_at) > HEARTBEAT_STALE_MS)
    {
        let _ = std::fs::remove_file(store.path(store::WATCH_LOCK));
    }

    let mut stack = store.closed();
    let before = stack.entries.len();
    store::prune_entries(&mut stack, now, cfg.entry_ttl_hours, crate::STACK_CAP);
    if stack.entries.len() != before {
        let _ = store.write_closed(&stack);
    }
}

/// Identifies one herdr SERVER generation: the socket is re-created by every server
/// start, so its inode + mtime change on exactly the event recovery cares about. An
/// hour bucket (the previous key) both missed real restarts and fired hourly on a
/// healthy session (F9).
pub fn boot_id(socket_path: &std::path::Path) -> String {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(socket_path) {
        Ok(md) => format!("{}-{}-{}", md.dev(), md.ino(), md.mtime()),
        Err(_) => "no-socket".to_string(),
    }
}

/// Cheap once-per-server-generation guard for `recover`.
pub fn recover_once(store: &Store, client: &Client, cfg: &Config) {
    let marker = store.path(store::BOOT_MARKER);
    let id = boot_id(client.path());
    let prev = std::fs::read_to_string(&marker).unwrap_or_default();
    if prev.trim() == id {
        return;
    }
    let _ = store::write_atomic(&store.dir, store::BOOT_MARKER, id.as_bytes());
    recover(store, client, cfg);
}
