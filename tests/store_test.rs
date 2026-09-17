mod common;

use common::*;
use reopen::close;
use reopen::config::Config;
use reopen::daemon;
use reopen::model::*;
use reopen::rpc::Client;
use reopen::store::{self, Store};

fn tmp() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    (d, s)
}

#[test]
fn writes_are_atomic_and_leave_no_temp_files() {
    let (dir, s) = tmp();
    let snap = synth_snapshot("w1", "w1:t1", &["w1:p1"], "/tmp");
    s.write_snapshot(&snap).unwrap();
    assert_eq!(s.snapshot(), snap);
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "temp files were left behind");
}

#[test]
fn a_corrupt_state_file_degrades_to_empty_instead_of_panicking() {
    let (_d, s) = tmp();
    std::fs::write(s.path(store::CLOSED), "{not json").unwrap();
    assert_eq!(s.closed().entries.len(), 0);
    std::fs::write(s.path(store::SNAPSHOT), "").unwrap();
    assert_eq!(s.snapshot().workspaces.len(), 0);
}

#[test]
fn the_lock_is_reentrant_across_sequential_critical_sections() {
    let (_d, s) = tmp();
    for i in 0..3 {
        let _g = s.lock();
        let mut st = s.closed();
        st.entries.push(entry(i));
        s.write_closed(&st).unwrap();
    }
    assert_eq!(s.closed().entries.len(), 3);
}

fn entry(id: u64) -> ClosedEntry {
    ClosedEntry {
        id,
        closed_at_ms: reopen::now_ms(),
        granularity: Granularity::Pane,
        reason: CloseReason::Closed,
        events: vec!["pane.closed".into()],
        workspace: synth_snapshot("w1", "w1:t1", &["w1:p1"], "/tmp").workspaces[0].clone(),
        group: vec![],
        summary: format!("pane · {id}"),
        protocol: 22,
    }
}

#[test]
fn entries_expire_after_the_ttl_and_the_stack_is_capped() {
    let mut stack = ClosedStack::default();
    let now = reopen::now_ms();
    let mut old = entry(1);
    old.closed_at_ms = now - 25 * 3_600_000;
    stack.entries.push(old);
    for i in 2..30 {
        stack.entries.push(entry(i));
    }
    store::prune_entries(&mut stack, now, 24, 20);
    assert_eq!(stack.entries.len(), 20);
    assert!(stack.entries.iter().all(|e| e.id != 1));
    // ids 2..=21 survive the cap; the next id continues from the newest kept
    assert_eq!(store::next_entry_id(&stack), 22);
}

#[test]
fn self_created_ids_suppress_capture_for_five_seconds_only() {
    let (_d, s) = tmp();
    let now = reopen::now_ms();
    s.record_self_created(&["w9:p1".into()]);
    assert!(s.is_self_created("w9:p1", now));
    assert!(!s.is_self_created("w9:p2", now));
    assert!(!s.is_self_created("w9:p1", now + 6_000));
}

#[test]
fn startup_recovery_finalizes_an_abandoned_burst() {
    let (_d, s) = tmp();
    let cfg = Config::default();
    let client = Client::new("/nonexistent/herdr.sock"); // never contacted: notify is off
    let snap = synth_snapshot("w3G", "w3G:t1", &["w3G:p4", "w3G:p1"], "/private/tmp");
    s.write_snapshot(&snap).unwrap();
    let ev = close_event("pane_closed", Some(reopen::now_ms() - 10_000));
    let (pending, _) = close::push_event(Pending::default(), ev, "tok", snap.workspaces.clone());
    s.write_pending(&pending).unwrap();

    daemon::recover(&s, &client, &cfg);
    assert!(s.pending().burst.is_none(), "the burst was cleared");
    assert_eq!(s.closed().entries.len(), 1, "and became an undo entry");
    assert!(s.self_created().ids.is_empty());
}

#[test]
fn startup_recovery_drops_a_burst_with_no_close_event() {
    let (_d, s) = tmp();
    let cfg = Config::default();
    let client = Client::new("/nonexistent/herdr.sock");
    let stale = Burst {
        first_ms: reopen::now_ms() - 10_000,
        last_ms: reopen::now_ms() - 10_000,
        last_token: "x".into(),
        events: vec![],
        frozen: vec![],
    };
    s.write_pending(&Pending { burst: Some(stale) }).unwrap();
    daemon::recover(&s, &client, &cfg);
    assert!(s.pending().burst.is_none());
    assert!(s.closed().entries.is_empty());
}

#[test]
fn a_fresh_burst_survives_recovery() {
    let (_d, s) = tmp();
    let cfg = Config::default();
    let client = Client::new("/nonexistent/herdr.sock");
    let ev = close_event("pane_closed", Some(reopen::now_ms()));
    let (pending, _) = close::push_event(Pending::default(), ev, "tok", vec![]);
    s.write_pending(&pending).unwrap();
    daemon::recover(&s, &client, &cfg);
    assert!(
        s.pending().burst.is_some(),
        "a live burst must not be stolen"
    );
}

#[test]
fn entry_ids_never_restart_after_the_stack_empties() {
    // review F15: a user who read `reopen list`, waited out the TTL and then ran
    // `reopen --id 1` restored a DIFFERENT entry than the one they saw.
    let mut stack = ClosedStack::default();
    assert_eq!(store::next_entry_id(&stack), 1);
    stack.entries.push(entry(1));
    stack.entries.push(entry(2));
    stack.next_id = 3;
    assert_eq!(store::next_entry_id(&stack), 3);
    // a TTL sweep empties it; the allocator survives in the file
    stack.entries.clear();
    assert_eq!(store::next_entry_id(&stack), 3);
    // and an entries list that somehow runs ahead of the counter still wins
    stack.entries.push(entry(9));
    assert_eq!(store::next_entry_id(&stack), 10);
}

#[test]
fn a_closed_stack_written_before_the_counter_existed_still_loads() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    std::fs::write(
        s.path(store::CLOSED),
        serde_json::to_vec(&serde_json::json!({"entries": []})).unwrap(),
    )
    .unwrap();
    assert_eq!(s.closed().next_id, 0);
    assert_eq!(store::next_entry_id(&s.closed()), 1);
}

#[test]
fn two_concurrent_hooks_for_one_gesture_produce_exactly_one_entry() {
    // The token protocol is the heart of the design and only its pure pieces were
    // covered (review F11 item 9). Two REAL `on_close` processes — here threads sharing
    // one Store — race on `pending.json`; exactly one of them must finalize.
    let (_d, s) = tmp();
    let cfg = Config {
        notify_on_close: false,
        ..Default::default()
    };
    // Pretend a healthy daemon is already running so `on_close` does not spawn one.
    s.write_json(
        store::WATCH_LOCK,
        &WatchLock {
            pid: std::process::id() as i32,
            started_at: reopen::now_ms(),
            updated_at: reopen::now_ms(),
        },
    )
    .unwrap();
    let snap = synth_snapshot("w3N", "w3N:t1", &["w3N:p1"], "/private/tmp");
    s.write_snapshot(&snap).unwrap();

    let f = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events_close.json"),
    )
    .unwrap();
    let f: serde_json::Value = serde_json::from_str(&f).unwrap();
    let gesture = [
        ("pane.closed", f["pane_closed_last_of_ws"].clone()),
        ("workspace.closed", f["workspace_closed"].clone()),
    ];

    let t0 = reopen::now_ms();
    std::thread::scope(|scope| {
        for (name, payload) in &gesture {
            let s = &s;
            let cfg = &cfg;
            scope.spawn(move || {
                let client = Client::new("/nonexistent/herdr.sock");
                close::on_close(
                    s,
                    &client,
                    cfg,
                    name,
                    &payload["event_json"].to_string(),
                    Some(&payload["context_json"].to_string()),
                    t0,
                );
            });
        }
    });

    let stack = s.closed();
    assert_eq!(stack.entries.len(), 1, "one gesture, one entry: {stack:#?}");
    assert_eq!(stack.entries[0].granularity, Granularity::Workspace);
    assert_eq!(
        stack.entries[0].events.len(),
        2,
        "both hooks' events folded in"
    );
    assert!(s.pending().burst.is_none(), "the burst was consumed");
}
