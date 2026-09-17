//! The restore DRIVER, against a scripted fake herdr socket (review F11).
//!
//! `tests/restore_test.rs` covers the pure helpers; everything here exercises the code
//! that actually issues socket calls: the three granularities, escalation, ordering, the
//! pane-granularity swap, the `agent.start` ladders and self-exclusion.

mod common;

use common::*;
use reopen::config::{Config, RerunMode};
use reopen::model::*;
use reopen::restore::{self, Ctx};
use reopen::store::Store;
use serde_json::{json, Value};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::new(d.path());
    (d, s)
}

fn pane(id: &str, tab: &str, ws: &str, cwd: &str) -> PaneSnap {
    PaneSnap {
        pane_id: id.into(),
        tab_id: tab.into(),
        workspace_id: ws.into(),
        cwd: cwd.into(),
        ..Default::default()
    }
}

fn agent_pane(id: &str, tab: &str, ws: &str, label: Option<&str>) -> PaneSnap {
    PaneSnap {
        agent: Some("claude".into()),
        agent_session: Some("sess-1".into()),
        agent_session_kind: Some("id".into()),
        label: label.map(|s| s.into()),
        ..pane(id, tab, ws, "/private/tmp")
    }
}

fn tab_snap(tab: &str, ws: &str, index: usize, label: Option<&str>, layout: LayoutNode) -> TabSnap {
    let panes = layout
        .leaf_pane_ids()
        .iter()
        .map(|p| pane(p, tab, ws, "/private/tmp"))
        .collect();
    TabSnap {
        tab_id: tab.into(),
        workspace_id: ws.into(),
        label: label.map(|s| s.into()),
        index,
        number: index as u32 + 1,
        layout,
        focused_pane_id: None,
        panes,
    }
}

fn leaf(id: &str) -> LayoutNode {
    LayoutNode::pane(Some(id.into()), Some("/private/tmp".into()), None)
}

fn split(dir: SplitDir, ratio: f64, a: LayoutNode, b: LayoutNode) -> LayoutNode {
    LayoutNode::Split {
        direction: dir,
        ratio,
        first: Box::new(a),
        second: Box::new(b),
    }
}

fn entry(g: Granularity, ws: WorkspaceSnap) -> ClosedEntry {
    ClosedEntry {
        id: 1,
        closed_at_ms: reopen::now_ms(),
        granularity: g,
        reason: CloseReason::Closed,
        events: vec!["pane.closed".into()],
        workspace: ws,
        group: vec![],
        summary: "entry".into(),
        protocol: 22,
    }
}

fn workspace(id: &str, label: Option<&str>, tabs: Vec<TabSnap>) -> WorkspaceSnap {
    WorkspaceSnap {
        workspace_id: id.into(),
        label: label.map(|s| s.into()),
        index: 0,
        number: 1,
        cwd: Some("/private/tmp".into()),
        active_tab_id: None,
        tabs,
    }
}

fn live(workspaces: Value, tabs: Value, panes: Value) -> Value {
    json!({"workspaces": workspaces, "tabs": tabs, "panes": panes})
}

fn empty_world() -> Value {
    live(json!([]), json!([]), json!([]))
}

fn cfg_no_focus() -> Config {
    Config {
        focus_on_reopen: false,
        ..Default::default()
    }
}

// ---------------------------------------------------------------- granularity: tab

#[test]
fn a_tab_restore_applies_one_layout_and_moves_it_to_its_remembered_index() {
    // The workspace is still live; the tab is gone.
    let world = live(
        json!([{"workspace_id": "w1", "label": "scratch"}]),
        json!([{"tab_id": "w1:t1", "workspace_id": "w1"},
               {"tab_id": "w1:t2", "workspace_id": "w1"}]),
        json!([{"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "cwd": "/private/tmp"}]),
    );
    let fake = Fake::new(default_reply(world));
    let (_d, st) = store();
    let cfg = cfg_no_focus();

    let tab = tab_snap(
        "w1:t9",
        "w1",
        1,
        Some("work"),
        split(SplitDir::Right, 0.3, leaf("w1:p8"), leaf("w1:p9")),
    );
    let e = entry(
        Granularity::Tab,
        workspace("w1", Some("scratch"), vec![tab]),
    );
    let report = restore::run(&e, &Ctx::new(&fake, &st, &cfg));

    assert!(report.ok, "{report:?}");
    assert_eq!(fake.count("workspace.create"), 0, "the workspace was alive");
    assert_eq!(fake.count("layout.apply"), 1);
    assert_eq!(report.created.tabs.len(), 1);
    assert_eq!(report.created.panes.len(), 2);

    let apply = &fake.calls_to("layout.apply")[0];
    assert_eq!(apply["workspace_id"], "w1");
    assert_eq!(apply["tab_label"], "work");
    // F13: `live` was fetched before the apply, so the count must include the new tab.
    let mv = &fake.calls_to("tab.move")[0];
    assert_eq!(mv["insert_index"], 1);
}

#[test]
fn layout_apply_carries_no_pane_ids_and_a_ratio_on_every_split() {
    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let tab = tab_snap(
        "w1:t1",
        "w1",
        0,
        None,
        split(
            SplitDir::Right,
            0.25,
            leaf("w1:p1"),
            split(SplitDir::Down, 0.75, leaf("w1:p2"), leaf("w1:p3")),
        ),
    );
    let e = entry(Granularity::Tab, workspace("w1", None, vec![tab]));
    restore::run(&e, &Ctx::new(&fake, &st, &cfg));

    let root = &fake.calls_to("layout.apply")[0]["root"];
    fn check(n: &Value) {
        assert!(n.get("pane_id").is_none(), "a stripped root has no pane_id");
        if n["type"] == "split" {
            assert!(n.get("ratio").is_some(), "ratio is required on a split");
            check(&n["first"]);
            check(&n["second"]);
        }
    }
    check(root);
    assert_eq!(root["ratio"], 0.25);
    assert_eq!(root["second"]["ratio"], 0.75);
}

// ---------------------------------------------------------------- escalation

#[test]
fn a_pane_whose_tab_and_workspace_are_both_gone_escalates_to_one_new_workspace() {
    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = cfg_no_focus();

    // The remembered tab had THREE panes; only the closed one owns this entry.
    let layout = split(
        SplitDir::Right,
        0.5,
        leaf("w1:p1"),
        split(SplitDir::Down, 0.5, leaf("w1:p2"), leaf("w1:p3")),
    );
    let mut tab = tab_snap("w1:t1", "w1", 0, None, layout);
    tab.panes = vec![pane("w1:p2", "w1:t1", "w1", "/private/tmp")];
    let e = entry(Granularity::Pane, workspace("w1", None, vec![tab]));
    let report = restore::run(&e, &Ctx::new(&fake, &st, &cfg));

    assert!(report.ok, "{report:?}");
    assert_eq!(fake.count("workspace.create"), 1);
    assert_eq!(fake.count("layout.apply"), 1);
    assert_eq!(fake.count("pane.split"), 0);
    // exactly this pane's leaf — the tab's other panes own their own entries
    let root = &fake.calls_to("layout.apply")[0]["root"];
    assert_eq!(root["type"], "pane");
    assert_eq!(report.created.panes.len(), 1);
    assert!(report
        .notes
        .iter()
        .any(|n| n.contains("original tab is gone")));
}

#[test]
fn a_workspace_that_is_still_alive_only_gets_its_missing_tabs_back() {
    let world = live(
        json!([{"workspace_id": "w1", "label": "scratch"}]),
        json!([{"tab_id": "w1:t1", "workspace_id": "w1", "label": "kept"}]),
        json!([{"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "cwd": "/private/tmp"}]),
    );
    let fake = Fake::new(default_reply(world));
    let (_d, st) = store();
    let cfg = cfg_no_focus();

    let ws = workspace(
        "w1",
        Some("scratch"),
        vec![
            tab_snap("w1:t1", "w1", 0, Some("kept"), leaf("w1:p1")),
            tab_snap("w1:t2", "w1", 1, Some("gone"), leaf("w1:p2")),
        ],
    );
    let report = restore::run(
        &entry(Granularity::Workspace, ws),
        &Ctx::new(&fake, &st, &cfg),
    );

    assert_eq!(fake.count("workspace.create"), 0);
    assert_eq!(fake.count("layout.apply"), 1, "only the missing tab");
    assert_eq!(fake.calls_to("layout.apply")[0]["tab_label"], "gone");
    assert!(report.ok, "{report:?}");
}

// ---------------------------------------------------------------- ordering

#[test]
fn every_layout_is_applied_before_the_ascending_tab_move_pass() {
    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let ws = workspace(
        "w1",
        Some("three"),
        vec![
            tab_snap("w1:t1", "w1", 0, Some("a"), leaf("w1:p1")),
            tab_snap("w1:t2", "w1", 1, Some("b"), leaf("w1:p2")),
            tab_snap("w1:t3", "w1", 2, Some("c"), leaf("w1:p3")),
        ],
    );
    restore::run(
        &entry(Granularity::Workspace, ws),
        &Ctx::new(&fake, &st, &cfg),
    );

    let seq = fake.sequence();
    let last_apply = seq.iter().rposition(|m| m == "layout.apply").unwrap();
    let first_move = seq.iter().position(|m| m == "tab.move").unwrap();
    assert!(
        last_apply < first_move,
        "layout.apply must never be interleaved with tab.move: {seq:?}"
    );
    let idx: Vec<i64> = fake
        .calls_to("tab.move")
        .iter()
        .map(|p| p["insert_index"].as_i64().unwrap())
        .collect();
    assert_eq!(idx, [0, 1, 2], "ascending, one per created tab");
}

// ---------------------------------------------------------------- pane granularity

/// Build a live world holding one tab with the given pane ids.
fn world_with_tab(pane_ids: &[&str]) -> Value {
    let panes: Vec<Value> = pane_ids
        .iter()
        .map(|p| json!({"pane_id": p, "tab_id": "w1:t1", "workspace_id": "w1", "cwd": "/private/tmp"}))
        .collect();
    live(
        json!([{"workspace_id": "w1", "label": "scratch"}]),
        json!([{"tab_id": "w1:t1", "workspace_id": "w1", "label": "work"}]),
        json!(panes),
    )
}

fn pane_entry(layout: LayoutNode, closed: &str) -> ClosedEntry {
    let mut tab = tab_snap("w1:t1", "w1", 0, Some("work"), layout);
    tab.panes = vec![pane(closed, "w1:t1", "w1", "/private/tmp")];
    entry(
        Granularity::Pane,
        workspace("w1", Some("scratch"), vec![tab]),
    )
}

#[test]
fn a_pane_whose_sibling_is_a_single_live_leaf_keeps_its_ratio_and_is_swapped_back() {
    // p1 sat FIRST of a 0.3 split; its sibling p2 is a single live leaf.
    let fake = Fake::new(default_reply(world_with_tab(&["w1:p2"])));
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let e = pane_entry(
        split(SplitDir::Right, 0.3, leaf("w1:p1"), leaf("w1:p2")),
        "w1:p1",
    );
    let report = restore::run(&e, &Ctx::new(&fake, &st, &cfg));

    let sp = &fake.calls_to("pane.split")[0];
    assert_eq!(sp["target_pane_id"], "w1:p2");
    assert_eq!(sp["direction"], "right");
    assert_eq!(sp["ratio"], 0.3, "the ORIGINAL ratio, not the complement");
    let swap = &fake.calls_to("pane.swap")[0];
    assert_eq!(swap["target_pane_id"], "w1:p2");
    assert_eq!(swap["source_pane_id"], report.created.panes[0]);
    assert!(report.ok, "{report:?}");
}

#[test]
fn a_pane_whose_sibling_is_a_subtree_gets_the_complement_ratio_no_swap_and_a_note() {
    // p1 sat FIRST of a 0.3 split whose sibling is a 3-leaf subtree.
    let sibling = split(
        SplitDir::Down,
        0.5,
        leaf("w1:p2"),
        split(SplitDir::Right, 0.5, leaf("w1:p3"), leaf("w1:p4")),
    );
    let fake = Fake::new(default_reply(world_with_tab(&["w1:p2", "w1:p3", "w1:p4"])));
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let e = pane_entry(split(SplitDir::Right, 0.3, leaf("w1:p1"), sibling), "w1:p1");
    let report = restore::run(&e, &Ctx::new(&fake, &st, &cfg));

    let sp = &fake.calls_to("pane.split")[0];
    assert!(
        (sp["ratio"].as_f64().unwrap() - 0.7).abs() < 1e-9,
        "complement of 0.3, got {}",
        sp["ratio"]
    );
    assert_eq!(fake.count("pane.swap"), 0);
    assert!(report
        .notes
        .iter()
        .any(|n| n.contains("opposite side of its original split")));
}

// ---------------------------------------------------------------- agent.start

fn agent_entry(label: Option<&str>) -> ClosedEntry {
    let mut tab = tab_snap("w1:t1", "w1", 0, Some("work"), leaf("w1:p1"));
    tab.panes = vec![agent_pane("w1:p1", "w1:t1", "w1", label)];
    entry(
        Granularity::Tab,
        workspace("w1", Some("scratch"), vec![tab]),
    )
}

#[test]
fn the_alias_ladder_retries_on_agent_name_taken_and_reports_one_resume() {
    let base = default_reply(empty_world());
    let fake = Fake::new(move |m: &str, p: &Value, n: usize| {
        if m == "agent.start" {
            return if n < 2 {
                Err(remote("agent_name_taken"))
            } else {
                Ok(json!({"ok": true}))
            };
        }
        base(m, p, n)
    });
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let report = restore::run(&agent_entry(Some("study")), &Ctx::new(&fake, &st, &cfg));

    let names: Vec<String> = fake
        .calls_to("agent.start")
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["study", "study-2", "study-3"]);
    assert_eq!(report.resumed, 1);
    assert!(report.failed.is_empty(), "{report:?}");
    // every attempt asks herdr for the same server-side startup budget
    assert_eq!(fake.calls_to("agent.start")[0]["timeout_ms"], 60000);
    assert_eq!(
        fake.calls_to("agent.start")[0]["args"],
        json!(["--resume", "sess-1"])
    );
}

#[test]
fn an_exhausted_alias_ladder_fails_once_instead_of_looping() {
    let base = default_reply(empty_world());
    let fake = Fake::new(move |m: &str, p: &Value, n: usize| {
        if m == "agent.start" {
            return Err(remote("agent_name_taken"));
        }
        base(m, p, n)
    });
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let report = restore::run(&agent_entry(Some("study")), &Ctx::new(&fake, &st, &cfg));

    assert_eq!(
        fake.count("agent.start"),
        5,
        "base + 4 ladder steps, then stop"
    );
    assert_eq!(report.resumed, 0);
    assert_eq!(report.failed.len(), 1);
    // a remote refusal is safe to fall back from: the pane is definitely free
    assert_eq!(report.prefilled, 1);
    assert_eq!(fake.count("pane.send_text"), 1);
}

#[test]
fn a_busy_pane_is_retried_until_it_settles() {
    let base = default_reply(empty_world());
    let fake = Fake::new(move |m: &str, p: &Value, n: usize| {
        if m == "agent.start" {
            return if n < 3 {
                Err(remote("agent_pane_busy"))
            } else {
                Ok(json!({"ok": true}))
            };
        }
        base(m, p, n)
    });
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let report = restore::run(&agent_entry(Some("busy")), &Ctx::new(&fake, &st, &cfg));

    assert_eq!(fake.count("agent.start"), 4);
    // the alias never changes for a busy pane — only for a taken name
    let names: Vec<String> = fake
        .calls_to("agent.start")
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["busy", "busy", "busy", "busy"]);
    assert_eq!(report.resumed, 1);
}

#[test]
fn a_transport_timeout_never_types_into_a_pane_that_may_be_starting_an_agent() {
    // F2: herdr was asked to wait 60 s; if OUR read gives up we cannot know the agent
    // did not start, so prefilling would land inside the agent's own prompt.
    let base = default_reply(empty_world());
    let fake = Fake::new(move |m: &str, p: &Value, n: usize| {
        if m == "agent.start" {
            return Err(transport_timeout());
        }
        base(m, p, n)
    });
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let report = restore::run(&agent_entry(Some("slow")), &Ctx::new(&fake, &st, &cfg));

    assert_eq!(fake.count("agent.start"), 1, "a timeout is not retried");
    assert_eq!(fake.count("pane.send_text"), 0, "MUST NOT prefill");
    assert_eq!(report.prefilled, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(report.notes.iter().any(|n| n.contains("not prefilling")));
}

#[test]
fn a_server_side_agent_start_timeout_is_treated_as_probably_started() {
    let base = default_reply(empty_world());
    let fake = Fake::new(move |m: &str, p: &Value, n: usize| {
        if m == "agent.start" {
            return Err(remote("timeout"));
        }
        base(m, p, n)
    });
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let report = restore::run(&agent_entry(None), &Ctx::new(&fake, &st, &cfg));
    assert_eq!(report.resumed, 1);
    assert_eq!(fake.count("pane.send_text"), 0);
    assert!(report
        .notes
        .iter()
        .any(|n| n.contains("may still be starting")));
}

// ---------------------------------------------------------------- occupants

#[test]
fn a_non_agent_pane_is_prefilled_but_never_executed_unless_rerun_is_on() {
    let mut tab = tab_snap("w1:t1", "w1", 0, None, leaf("w1:p1"));
    tab.panes[0].foreground = Some(ForegroundCmd {
        argv: vec!["tail".into(), "-f".into(), "/dev/null".into()],
        cwd: "/private/tmp".into(),
        pid: 42,
        captured_at_ms: 1,
    });
    let e = entry(Granularity::Tab, workspace("w1", None, vec![tab]));

    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let report = restore::run(&e, &Ctx::new(&fake, &st, &cfg));
    assert_eq!(report.prefilled, 1);
    assert_eq!(
        fake.calls_to("pane.send_text")[0]["text"],
        "tail -f /dev/null"
    );
    assert_eq!(
        fake.count("pane.send_keys"),
        0,
        "prefill must not press Enter"
    );

    // mode = "run" and `tail` is on the default allow list
    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = Config {
        focus_on_reopen: false,
        rerun: reopen::config::Rerun {
            mode: RerunMode::Run,
            ..Default::default()
        },
        ..Default::default()
    };
    restore::run(&e, &Ctx::new(&fake, &st, &cfg));
    assert_eq!(fake.count("pane.send_keys"), 1);
}

#[test]
fn a_broken_deny_pattern_stops_every_rerun_instead_of_failing_open() {
    let mut tab = tab_snap("w1:t1", "w1", 0, None, leaf("w1:p1"));
    tab.panes[0].foreground = Some(ForegroundCmd {
        argv: vec!["npm".into(), "run".into(), "dev".into()],
        cwd: "/private/tmp".into(),
        pid: 42,
        captured_at_ms: 1,
    });
    let e = entry(Granularity::Tab, workspace("w1", None, vec![tab]));
    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = Config {
        focus_on_reopen: false,
        rerun: reopen::config::Rerun {
            mode: RerunMode::Run,
            deny: vec!["^(rm|dd".into()], // unbalanced paren
            ..Default::default()
        },
        ..Default::default()
    };
    let report = restore::run(&e, &Ctx::new(&fake, &st, &cfg));
    assert_eq!(fake.count("pane.send_keys"), 0, "fails CLOSED");
    assert_eq!(report.prefilled, 1);
}

// ---------------------------------------------------------------- focus

#[test]
fn the_remembered_focused_pane_is_focused_through_the_positional_map() {
    let mut tab = tab_snap(
        "w1:t1",
        "w1",
        0,
        None,
        split(SplitDir::Right, 0.5, leaf("w1:p1"), leaf("w1:p2")),
    );
    tab.focused_pane_id = Some("w1:p2".into());
    let e = entry(Granularity::Tab, workspace("w1", None, vec![tab]));
    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = Config::default(); // focus_on_reopen = true
    let report = restore::run(&e, &Ctx::new(&fake, &st, &cfg));

    let focused: Vec<String> = fake
        .calls_to("pane.focus")
        .iter()
        .map(|p| p["pane_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(focused, [report.created.panes[1].clone()]);
}

// ---------------------------------------------------------------- self-exclusion

#[test]
fn ids_a_restore_created_are_not_captured_as_a_new_close_until_the_ttl_expires() {
    let fake = Fake::new(default_reply(empty_world()));
    let (_d, st) = store();
    let cfg = cfg_no_focus();
    let tab = tab_snap("w1:t1", "w1", 0, None, leaf("w1:p1"));
    let report = restore::run(
        &entry(Granularity::Tab, workspace("w1", None, vec![tab])),
        &Ctx::new(&fake, &st, &cfg),
    );
    let new_pane = report.created.panes[0].clone();

    let now = reopen::now_ms();
    assert!(st.is_self_created(&new_pane, now));
    assert!(!st.is_self_created("someone:else", now));
    // …and the exclusion is time-boxed, so a genuine close later is still captured
    assert!(!st.is_self_created(&new_pane, now + reopen::SELF_CREATED_TTL_MS + 1));
}
