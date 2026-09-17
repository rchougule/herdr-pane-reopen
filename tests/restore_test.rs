mod common;

use common::*;
use reopen::config::{Config, Rerun, RerunMode};
use reopen::model::*;
use reopen::restore::*;

#[test]
fn strip_pane_ids_keeps_geometry_labels_and_cwds() {
    let root = layout("layout_export_3pane.json").root;
    let stripped = strip_pane_ids(&root);
    assert!(stripped.leaf_pane_ids().is_empty());
    assert_eq!(
        stripped.leaf_cwds(),
        ["/private/tmp", "/private/tmp", "/usr"]
    );
    match &stripped {
        LayoutNode::Split {
            direction,
            ratio,
            first,
            ..
        } => {
            assert_eq!(*direction, SplitDir::Right);
            assert!((ratio - 0.3).abs() < 1e-9);
            match &**first {
                LayoutNode::Pane { label, .. } => assert_eq!(label.as_deref(), Some("leftie")),
                _ => panic!("expected a leaf"),
            }
        }
        _ => panic!("expected a split"),
    }
    // and it is still valid layout.apply input
    let json = serde_json::to_value(&stripped).unwrap();
    assert_eq!(json["type"], "split");
    assert!(json["first"].get("pane_id").is_none());
}

#[test]
fn old_to_new_pane_mapping_is_by_depth_first_position() {
    let old = layout("layout_export_3pane.json").root;
    let new = layout("layout_apply_3pane.json").root;
    let m = map_pane_ids(&old, &new);
    assert_eq!(
        m,
        vec![
            ("w3M:p1".into(), "w3M:p4".into()),
            ("w3M:p2".into(), "w3M:p5".into()),
            ("w3M:p3".into(), "w3M:p6".into()),
        ]
    );
}

#[test]
fn locate_finds_the_split_side_direction_and_ratio() {
    let root = layout("layout_export_3pane.json").root;
    let l = locate(&root, "w3M:p1").expect("p1 is the first child of the root split");
    assert_eq!(l.side, Side::First);
    assert_eq!(l.direction, SplitDir::Right);
    assert!((l.ratio - 0.3).abs() < 1e-9);
    assert_eq!(l.sibling.leaf_count(), 2, "sibling is itself a split");

    let l = locate(&root, "w3M:p3").expect("p3 is the second child of the nested split");
    assert_eq!(l.side, Side::Second);
    assert_eq!(l.direction, SplitDir::Down);
    assert_eq!(l.sibling.leaf_pane_ids(), ["w3M:p2"]);

    assert!(locate(&root, "nope").is_none());
}

#[test]
fn locate_on_the_four_pane_layout_picks_a_single_leaf_anchor() {
    let root = layout("layout_export_4pane.json").root;
    // p4 sat second of the innermost right-split; its sibling is the single leaf p1,
    // so restore can split p1 and keep the exact ratio.
    let l = locate(&root, "w3G:p4").unwrap();
    assert_eq!(l.side, Side::Second);
    assert_eq!(l.direction, SplitDir::Right);
    assert_eq!(l.sibling.leaf_pane_ids(), ["w3G:p1"]);
    // p2 was the second child of the root; its sibling is a 3-leaf subtree
    let l = locate(&root, "w3G:p2").unwrap();
    assert_eq!(l.sibling.leaf_count(), 3);
}

#[test]
fn tab_move_index_comes_from_the_array_index_never_from_number() {
    // tab_list_moved: array [t4,t3,t2] with numbers [4,3,2]. A restore of that workspace
    // must issue insert_index 0,1,2 — using `number-1` would give 3,2,1.
    let tabs: Vec<reopen::snapshot::RawTab> = result_of("tab_list_moved.json", "tabs");
    let ws = vec![reopen::snapshot::RawWorkspace {
        workspace_id: "w3M".into(),
        ..Default::default()
    }];
    let snap = reopen::snapshot::assemble(&ws, &tabs, &[], &Default::default(), 1);
    let idx: Vec<usize> = snap.workspaces[0]
        .tabs
        .iter()
        .map(|t| insert_index(t.index, 3))
        .collect();
    assert_eq!(idx, [0, 1, 2]);
    // clamped when fewer tabs are live than were remembered
    assert_eq!(insert_index(5, 2), 2);
}

#[test]
fn same_container_rejects_a_recycled_id() {
    // herdr reissues ids; a matching id with a different label AND different cwds is a
    // stranger's container, not "already open".
    assert!(!same_container(
        "w3M",
        Some("reopen-qa-1"),
        &["/private/tmp".into()],
        "w3M",
        Some("someone-elses"),
        &["/Users/chaugs/go".into()],
    ));
    // same id + same label → same container
    assert!(same_container(
        "w3M",
        Some("reopen-qa-1"),
        &["/private/tmp".into()],
        "w3M",
        Some("reopen-qa-1"),
        &["/elsewhere".into()],
    ));
    // labels differ but a cwd matches → same container. Paths are canonicalised, so a
    // symlink and its target count as the same cwd on every platform.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(same_container(
        "w3M",
        Some("a"),
        &[link.to_string_lossy().into_owned()],
        "w3M",
        Some("b"),
        &[real.to_string_lossy().into_owned()],
    ));
    // a different id is never the same container
    assert!(!same_container("w3M", None, &[], "w3N", None, &[]));
}

#[test]
fn herdr_auto_labels_are_treated_as_unset() {
    assert_eq!(effective_tab_label(Some("1")), None);
    assert_eq!(effective_tab_label(Some("22")), None);
    assert_eq!(effective_tab_label(Some("flow")), Some("flow".into()));
    assert_eq!(effective_tab_label(None), None);
    assert_eq!(effective_ws_label(Some("tmp"), Some("/private/tmp")), None);
    assert_eq!(
        effective_ws_label(Some("reopen-qa"), Some("/private/tmp")),
        Some("reopen-qa".into())
    );
}

#[test]
fn rerun_allow_and_deny_lists() {
    let raw = Rerun::default();
    assert_eq!(raw.mode, RerunMode::Prefill);
    let r = CompiledRerun::new(&raw);
    assert!(rerun_allowed(
        &r,
        &["npm".into(), "run".into(), "dev".into()]
    ));
    assert!(rerun_allowed(&r, &["/usr/bin/tail".into(), "-f".into()]));
    assert!(!rerun_allowed(&r, &["rm".into(), "-rf".into(), "/".into()]));
    assert!(!rerun_allowed(&r, &["ssh".into(), "prod".into()]));
    assert!(!rerun_allowed(&r, &[]));
}

#[test]
fn the_session_allow_list_scopes_a_globally_linked_plugin() {
    // herdr's plugin registry is global, so this allow-list is the only way to keep the
    // plugin inert in sessions it was not meant for.
    let c = Config::default();
    assert!(c.allowed_sockets.is_empty());
    assert!(
        c.session_allowed(std::path::Path::new("/anything/herdr.sock")),
        "an empty list means every session"
    );
    let c = Config::parse(r#"allowed_sockets = ["/tmp/qa/herdr.sock"]"#);
    assert!(c.session_allowed(std::path::Path::new("/tmp/qa/herdr.sock")));
    assert!(!c.session_allowed(std::path::Path::new(
        "/Users/someone/.config/herdr/herdr.sock"
    )));
}

#[test]
fn config_defaults_and_overrides_round_trip() {
    let c = Config::default();
    assert!(c.focus_on_reopen && !c.notify_on_close && c.capture_exited);
    assert_eq!(c.entry_ttl_hours, 24);
    let c = Config::parse(
        r#"
focus_on_reopen = false
entry_ttl_hours = 6
[rerun]
mode = "run"
[resume.mystery]
args = ["--continue", "{id}"]
"#,
    );
    assert!(!c.focus_on_reopen);
    assert_eq!(c.entry_ttl_hours, 6);
    assert_eq!(c.rerun.mode, RerunMode::Run);
    assert!(
        !c.rerun.allow.is_empty(),
        "defaults survive a partial table"
    );
    assert_eq!(
        c.resume.get("mystery").unwrap().args,
        ["--continue", "{id}"]
    );
    // a broken config degrades to defaults instead of killing the hook
    assert_eq!(Config::parse("this is not toml = = =").entry_ttl_hours, 24);
}

// ---------------------------------------------------------------- agent liveness

fn info(shell_pid: i64, procs: &[(i64, &str)]) -> reopen::snapshot::RawProcessInfo {
    serde_json::from_value(serde_json::json!({
        "pane_id": "p1",
        "shell_pid": shell_pid,
        "foreground_processes": procs.iter().map(|(pid, a0)| serde_json::json!({
            "pid": pid, "argv0": a0, "argv": [a0], "cwd": "/private/tmp"
        })).collect::<Vec<_>>(),
    }))
    .unwrap()
}

#[test]
fn a_pane_with_only_its_shell_is_exited() {
    assert_eq!(
        pane_liveness(Some(&info(100, &[(100, "zsh")]))),
        Liveness::Exited
    );
}

#[test]
fn a_pane_running_an_agent_is_alive() {
    // Real shape: claude rewrites its process NAME to a version string, so only argv0
    // identifies it — and the classifier does not even look at which binary it is.
    assert_eq!(
        pane_liveness(Some(&info(100, &[(100, "zsh"), (200, "claude")]))),
        Liveness::Alive
    );
}

#[test]
fn a_pane_running_anything_else_is_alive_too_and_is_left_alone() {
    assert_eq!(
        pane_liveness(Some(&info(100, &[(100, "zsh"), (200, "vim")]))),
        Liveness::Alive
    );
}

#[test]
fn a_shell_startup_burst_is_not_mistaken_for_an_agent() {
    // `starship prompt` and friends are the shell's own bookkeeping, not an occupant.
    assert_eq!(
        pane_liveness(Some(&info(100, &[(100, "zsh"), (201, "starship")]))),
        Liveness::Exited
    );
}

#[test]
fn a_sample_we_did_not_get_is_unknown() {
    assert_eq!(pane_liveness(None), Liveness::Unknown);
    // An empty payload (herdr answered, but with nothing in it) is not evidence either.
    assert_eq!(pane_liveness(Some(&info(0, &[]))), Liveness::Unknown);
}
