mod common;

use common::*;
use reopen::close::*;
use reopen::model::*;

fn burst_of(events: Vec<BurstEvent>, frozen: Vec<WorkspaceSnap>) -> Burst {
    let first = events.iter().map(|e| e.t0).min().unwrap_or(0);
    let last = events.iter().map(|e| e.t0).max().unwrap_or(0);
    Burst {
        first_ms: first,
        last_ms: last,
        last_token: "t".into(),
        events,
        frozen,
    }
}

fn frozen_of(snap: &Snapshot) -> Vec<WorkspaceSnap> {
    snap.workspaces.clone()
}

#[test]
fn parses_every_captured_close_payload() {
    let e = close_event("pane_closed", None);
    assert_eq!(e.pane_id.as_deref(), Some("w3G:p4"));
    assert_eq!(e.workspace_id.as_deref(), Some("w3G"));
    assert!(e.tab_id.is_none(), "pane.closed carries no tab_id anywhere");

    let e = close_event("tab_closed", None);
    assert_eq!(e.tab_id.as_deref(), Some("w3G:t3"));

    let e = close_event("workspace_closed", None);
    assert_eq!(e.workspace_label.as_deref(), Some("reopen-review-burst"));

    // pane.exited's CONTEXT is rich where pane.closed's is empty
    let e = close_event("pane_exited", None);
    assert_eq!(e.ctx_cwd.as_deref(), Some("/private/tmp"));
    assert_eq!(e.tab_id.as_deref(), Some("w3G:t1"));
}

#[test]
fn garbage_event_json_never_panics() {
    assert!(parse_event("pane.closed", "}{", None, 1).is_none());
    assert!(parse_event("pane.closed", "", None, 1).is_none());
    let e = parse_event("pane.closed", "{}", None, 1).unwrap();
    assert!(e.pane_id.is_none());
}

#[test]
fn lone_pane_in_a_multi_pane_tab_is_pane_granularity() {
    let snap = synth_snapshot("w3G", "w3G:t1", &["w3G:p1", "w3G:p4"], "/private/tmp");
    let b = burst_of(vec![close_event("pane_closed", None)], frozen_of(&snap));
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].granularity, Granularity::Pane);
    // the tab's whole layout is kept so the sibling can be found on restore
    assert_eq!(out[0].workspace.tabs[0].layout.leaf_count(), 2);
    assert_eq!(out[0].workspace.tabs[0].panes.len(), 1);
    assert_eq!(out[0].workspace.tabs[0].panes[0].pane_id, "w3G:p4");
}

#[test]
fn last_pane_of_a_tab_collapses_to_tab_granularity() {
    // herdr emits NO tab.closed when a tab empties out (see docs/HERDR_API_NOTES.md).
    let snap = synth_snapshot("w3G", "w3G:t1", &["w3G:p4"], "/private/tmp");
    let b = burst_of(vec![close_event("pane_closed", None)], frozen_of(&snap));
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].granularity, Granularity::Tab);
    assert_eq!(out[0].workspace.tabs[0].tab_id, "w3G:t1");
}

#[test]
fn pane_closed_plus_workspace_closed_is_one_workspace_entry() {
    let snap = synth_snapshot("w3N", "w3N:t1", &["w3N:p1"], "/private/tmp");
    let b = burst_of(
        vec![
            close_event("pane_closed_last_of_ws", None),
            close_event("workspace_closed", None),
        ],
        frozen_of(&snap),
    );
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1, "the pane.closed is subsumed");
    assert_eq!(out[0].granularity, Granularity::Workspace);
    assert_eq!(out[0].workspace.workspace_id, "w3N");
    assert_eq!(out[0].events.len(), 2);
}

#[test]
fn explicit_tab_close_is_tab_granularity() {
    let mut snap = synth_snapshot(
        "w3G",
        "w3G:t3",
        &["w3G:p6", "w3G:p7", "w3G:p8"],
        "/private/tmp",
    );
    snap.workspaces[0].tabs[0].label = Some("work".into());
    let b = burst_of(vec![close_event("tab_closed", None)], frozen_of(&snap));
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].granularity, Granularity::Tab);
    assert_eq!(out[0].workspace.tabs[0].panes.len(), 3);
}

#[test]
fn workspace_close_keeps_all_tabs_and_panes() {
    let mut snap = synth_snapshot("w3J", "w3J:t1", &["w3J:p1", "w3J:p2"], "/private/tmp");
    let t2 =
        synth_snapshot("w3J", "w3J:t2", &["w3J:p3"], "/private/tmp").workspaces[0].tabs[0].clone();
    snap.workspaces[0].tabs.push(TabSnap { index: 1, ..t2 });
    let b = burst_of(
        vec![close_event("workspace_closed_2tabs", None)],
        frozen_of(&snap),
    );
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].granularity, Granularity::Workspace);
    assert_eq!(out[0].workspace.tabs.len(), 2);
    assert_eq!(out[0].workspace.pane_count(), 3);
}

#[test]
fn a_group_close_makes_exactly_one_workspace_group_entry() {
    // Two workspace.closed with DIFFERENT ids in one global burst.
    let a = close_event("workspace_closed", Some(1000));
    let b_ev = close_event("workspace_closed_2tabs", Some(1001));
    let s1 = synth_snapshot("w3N", "w3N:t1", &["w3N:p1"], "/private/tmp");
    let s2 = synth_snapshot("w3J", "w3J:t1", &["w3J:p1"], "/private/tmp");
    let mut frozen = frozen_of(&s1);
    frozen.extend(frozen_of(&s2));
    let b = burst_of(vec![a, b_ev], frozen);
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].granularity, Granularity::WorkspaceGroup);
    assert_eq!(out[0].group.len(), 1);
}

#[test]
fn exited_panes_are_captured_only_when_configured() {
    let snap = synth_snapshot("w3G", "w3G:t1", &["w3G:p9", "w3G:p1"], "/private/tmp");
    let b = burst_of(vec![close_event("pane_exited", None)], frozen_of(&snap));
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].reason, CloseReason::Exited);
    assert!(infer_entries(&b, false, 1, 22).is_empty());
}

#[test]
fn an_unknown_pane_falls_back_to_minimal_hydration() {
    let b = burst_of(vec![close_event("pane_exited", None)], Vec::new());
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].granularity, Granularity::Pane);
    // cwd recovered from the event context rather than a cache miss losing the entry
    assert_eq!(out[0].workspace.tabs[0].panes[0].cwd, "/private/tmp");
    assert_eq!(out[0].workspace.tabs[0].tab_id, "w3G:t1");
}

// ---------------------------------------------------------------- coalescing

fn push(pending: Pending, ev: BurstEvent, token: &str) -> (Pending, Option<Burst>) {
    push_event(pending, ev, token, Vec::new())
}

fn push_with(
    pending: Pending,
    ev: BurstEvent,
    token: &str,
    frozen: Vec<WorkspaceSnap>,
) -> (Pending, Option<Burst>) {
    push_event(pending, ev, token, frozen)
}

#[test]
fn events_1ms_apart_coalesce_and_300ms_apart_do_not() {
    let p = Pending::default();
    let (p, expired) = push(p, close_event("pane_closed", Some(1_000)), "a");
    assert!(expired.is_none());
    let (p, expired) = push(p, close_event("workspace_closed", Some(1_001)), "b");
    assert!(expired.is_none());
    assert_eq!(p.burst.as_ref().unwrap().events.len(), 2);

    let (p, expired) = push(p, close_event("pane_closed", Some(1_301)), "c");
    let expired = expired.expect("the first burst closed");
    assert_eq!(expired.events.len(), 2);
    assert_eq!(p.burst.as_ref().unwrap().events.len(), 1);
}

#[test]
fn only_the_last_writer_finalizes_even_on_the_same_millisecond() {
    // Verified pair 1789667462044/…045 landed on adjacent ms; concurrent hooks routinely
    // share one. A timestamp test would let both finalize.
    let t = 1_789_667_462_044u64;
    let tok_a = make_token(t, 111, 1);
    let tok_b = make_token(t, 222, 2);
    assert_ne!(tok_a, tok_b);
    let (p, _) = push(
        Pending::default(),
        // the real cascade: the last pane of w3N, then w3N itself
        close_event("pane_closed_last_of_ws", Some(t)),
        &tok_a,
    );
    let (p, _) = push(p, close_event("workspace_closed", Some(t)), &tok_b);
    assert!(!owns_finalization(&p, &tok_a));
    assert!(owns_finalization(&p, &tok_b));
    let entries = infer_entries(p.burst.as_ref().unwrap(), true, 1, 22);
    // NOTE: the OTHER fixtures name DIFFERENT workspaces (w3G and w3N), so they are two
    // independent gestures and must stay two entries — see
    // `two_unrelated_gestures_in_one_burst_are_not_collapsed`.
    assert_eq!(
        entries.len(),
        1,
        "one gesture in one workspace is one undo entry"
    );
}

#[test]
fn the_window_never_moves_backwards() {
    // A late-scheduled early event must not shorten the window.
    let (p, _) = push(
        Pending::default(),
        close_event("pane_closed", Some(2_000)),
        "a",
    );
    let (p, _) = push(p, close_event("pane_closed", Some(1_900)), "b");
    assert_eq!(p.burst.as_ref().unwrap().last_ms, 2_000);
    assert_eq!(p.burst.as_ref().unwrap().first_ms, 2_000);
}

#[test]
fn a_crashed_hooks_burst_is_finalized_not_merged_into_the_next() {
    let (p, _) = push(
        Pending::default(),
        close_event("pane_closed", Some(1_000)),
        "a",
    );
    // 6 s later: past both the 250 ms window and the 5 s staleness bound.
    let (p, expired) = push(p, close_event("pane_closed", Some(7_000)), "b");
    assert!(expired.is_some());
    assert_eq!(p.burst.as_ref().unwrap().events.len(), 1);
}

#[test]
fn freezing_copies_the_pre_close_subtree() {
    let snap = synth_snapshot("w3G", "w3G:t1", &["w3G:p4", "w3G:p1"], "/private/tmp");
    let ev = close_event("pane_closed", None);
    let frozen = freeze_for(&snap, &ev);
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].tabs[0].panes.len(), 2);
    // an event for an unknown workspace freezes nothing rather than erroring
    let mut other = ev.clone();
    other.workspace_id = Some("nope".into());
    other.pane_id = Some("nope:p1".into());
    assert!(freeze_for(&snap, &other).is_empty());
}

#[test]
fn a_hook_that_starts_after_the_daemon_refreshed_falls_back_to_the_previous_snapshot() {
    // Real race seen in live QA: the watch daemon refreshed snapshot.json past the close
    // before the hook process had even started, so the pane was already gone and the
    // entry degraded to minimal hydration (cwd "~", no agent).
    let pre = synth_snapshot("wD", "wD:t1", &["wD:p1", "wD:p2"], "/private/tmp");
    let post = synth_snapshot("wD", "wD:t1", &["wD:p1"], "/private/tmp");
    let ev = BurstEvent {
        t0: 10,
        name: "pane.closed".into(),
        pane_id: Some("wD:p2".into()),
        tab_id: None,
        workspace_id: Some("wD".into()),
        workspace_label: None,
        ctx_cwd: None,
    };
    let candidates = [&post, &pre];
    let chosen = best_snapshot(&candidates, &ev);
    assert!(
        chosen.pane("wD:p2").is_some(),
        "the pre-close snapshot wins"
    );
    let b = burst_of(vec![ev.clone()], freeze_for(chosen, &ev));
    let out = infer_entries(&b, true, 1, 22);
    assert_eq!(out[0].granularity, Granularity::Pane);
    assert_eq!(out[0].workspace.tabs[0].panes[0].cwd, "/private/tmp");
}

#[test]
fn a_refresh_that_loses_a_pane_is_detected() {
    let pre = synth_snapshot("wD", "wD:t1", &["wD:p1", "wD:p2"], "/private/tmp");
    let post = synth_snapshot("wD", "wD:t1", &["wD:p1"], "/private/tmp");
    assert!(reopen::snapshot::lost_containers(&pre, &post));
    assert!(!reopen::snapshot::lost_containers(&post, &pre));
    assert!(!reopen::snapshot::lost_containers(&pre, &pre));
    assert!(!reopen::snapshot::lost_containers(
        &Snapshot::default(),
        &post
    ));
}

#[test]
fn self_created_ids_are_recognised_from_the_event() {
    let ev = close_event("workspace_closed", None);
    let ids = event_ids(&ev);
    assert!(ids.contains(&"w3N".to_string()));
    assert!(ids.contains(&"w3N:t1".to_string()));
}

#[test]
fn an_unbroken_chain_of_closes_past_the_staleness_bound_is_dropped() {
    // The BURST_STALE_MS branch: events ≤250 ms apart sustained for over 5 s is a hook
    // that crashed mid-burst, not a gesture. Previously untested (review F19).
    let mut p = Pending::default();
    let mut t = 1_000u64;
    let mut dropped = false;
    while t <= 1_000 + 5_400 {
        let (next, expired) = push(p, close_event("pane_closed", Some(t)), "tok");
        assert!(
            expired.is_none(),
            "nothing expires inside the 250 ms window"
        );
        // once the chain passes 5 s the accumulated burst is thrown away and a fresh one
        // starts with just this event
        if next.burst.as_ref().unwrap().events.len() == 1 && t > 1_000 {
            dropped = true;
        }
        p = next;
        t += 200;
    }
    assert!(dropped, "the stale chain must be dropped, not accumulated");
    assert!(p.burst.unwrap().events.len() < 27);
}

#[test]
fn two_unrelated_gestures_in_one_burst_are_not_collapsed() {
    // review F4: the burst is a GLOBAL 250 ms time window, not one gesture. Closing
    // workspace w3N and, 100 ms later, pane w3G:p4 must yield TWO entries — the pane
    // entry used to be silently dropped by the workspace branch's early return.
    let snap = synth_snapshot("w3G", "w3G:t1", &["w3G:p4", "w3G:p1"], "/private/tmp");
    let ws_ev = close_event("workspace_closed", Some(1_000));
    let pane_ev = close_event("pane_closed", Some(1_100));
    let (p, _) = push_with(Pending::default(), ws_ev, "a", vec![]);
    let (p, _) = push_with(
        p,
        pane_ev,
        "b",
        freeze_for(&snap, &close_event("pane_closed", None)),
    );

    let entries = infer_entries(p.burst.as_ref().unwrap(), true, 7, 22);
    assert_eq!(entries.len(), 2, "one per gesture: {entries:#?}");
    assert_eq!(entries[0].granularity, Granularity::Workspace);
    assert_eq!(entries[0].workspace.workspace_id, "w3N");
    assert_eq!(entries[1].granularity, Granularity::Pane);
    assert_eq!(entries[1].workspace.tabs[0].panes[0].pane_id, "w3G:p4");
    // ids stay unique and ascending
    assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), [7, 8]);
}

#[test]
fn the_cascade_inside_one_workspace_is_still_exactly_one_entry() {
    // The other half of F4: partitioning must NOT break herdr's own
    // "coarsest level wins" rule within a workspace.
    let (p, _) = push(
        Pending::default(),
        close_event("pane_closed_last_of_ws", Some(2_000)),
        "a",
    );
    let (p, _) = push(p, close_event("workspace_closed", Some(2_001)), "b");
    let entries = infer_entries(p.burst.as_ref().unwrap(), true, 1, 22);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].granularity, Granularity::Workspace);
    assert_eq!(entries[0].events.len(), 2, "both events folded into it");
}

#[test]
fn a_tab_close_and_a_pane_close_in_different_workspaces_both_survive() {
    let g = synth_snapshot("w3G", "w3G:t3", &["w3G:p4", "w3G:p1"], "/private/tmp");
    let tab_ev = close_event("tab_closed", Some(3_000));
    let pane_ev = close_event("pane_closed_last_of_ws", Some(3_050));
    let (p, _) = push_with(
        Pending::default(),
        tab_ev,
        "a",
        freeze_for(&g, &close_event("tab_closed", None)),
    );
    let (p, _) = push_with(p, pane_ev, "b", vec![]);
    let entries = infer_entries(p.burst.as_ref().unwrap(), true, 1, 22);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].granularity, Granularity::Tab);
    assert_eq!(entries[1].workspace.workspace_id, "w3N");
}
