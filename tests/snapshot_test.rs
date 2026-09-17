mod common;

use common::*;
use reopen::model::*;
use reopen::snapshot::*;

#[test]
fn assembles_from_real_list_payloads() {
    let snap = assemble(&workspaces(), &tabs(), &panes(), &Default::default(), 1_000);
    assert_eq!(snap.workspaces.len(), 2);
    assert_eq!(snap.workspaces[0].workspace_id, "wH");
    // index is the ARRAY position, number stays the creation ordinal
    assert_eq!(snap.workspaces[1].index, 1);
    assert_eq!(snap.workspaces[1].number, 2);
    let claude = snap.pane("wJ:pJ").expect("claude pane");
    assert_eq!(claude.agent.as_deref(), Some("claude"));
    assert_eq!(
        claude.agent_session.as_deref(),
        Some("3a134238-8c83-427f-9558-51ea36073773")
    );
    assert_eq!(claude.agent_session_kind.as_deref(), Some("id"));
    assert_eq!(claude.label.as_deref(), Some("backend-caching-plan"));
    // a pane with no label / agent keeps those absent instead of erroring
    let plain = snap.pane("wH:p1").expect("plain pane");
    assert!(plain.label.is_none() && plain.agent.is_none());
    assert_eq!(
        plain.cwd,
        "/Users/chaugs/go/src/github.com/workloom-dev/core"
    );
}

#[test]
fn leaf_order_is_depth_first_and_drives_pane_order() {
    let l = layout("layout_export_3pane.json");
    assert_eq!(l.root.leaf_pane_ids(), ["w3M:p1", "w3M:p2", "w3M:p3"]);
    let l4 = layout("layout_export_4pane.json");
    assert_eq!(
        l4.root.leaf_pane_ids(),
        ["w3G:p1", "w3G:p4", "w3G:p3", "w3G:p2"]
    );
    assert_eq!(l4.root.leaf_count(), 4);
}

#[test]
fn tab_order_is_array_order_not_number() {
    // tab.list after a tab.move: array order [t4,t3,t2], numbers [4,3,2].
    let tabs: Vec<RawTab> = result_of("tab_list_moved.json", "tabs");
    let ws = vec![RawWorkspace {
        workspace_id: "w3M".into(),
        number: 11,
        label: Some("reopen-review-1".into()),
        active_tab_id: None,
    }];
    let snap = assemble(&ws, &tabs, &[], &Default::default(), 1);
    let t = &snap.workspaces[0].tabs;
    assert_eq!(
        t.iter().map(|x| x.tab_id.as_str()).collect::<Vec<_>>(),
        ["w3M:t4", "w3M:t3", "w3M:t2"]
    );
    assert_eq!(t.iter().map(|x| x.index).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(t.iter().map(|x| x.number).collect::<Vec<_>>(), [4, 3, 2]);
}

#[test]
fn foreground_cwd_wins_over_cwd() {
    let mut p = RawPane {
        pane_id: "x:p1".into(),
        cwd: "/home".into(),
        foreground_cwd: Some("/home/project".into()),
        ..Default::default()
    };
    let snap = assemble(
        &[RawWorkspace {
            workspace_id: "x".into(),
            ..Default::default()
        }],
        &[RawTab {
            tab_id: "x:t1".into(),
            workspace_id: "x".into(),
            ..Default::default()
        }],
        &{
            p.tab_id = "x:t1".into();
            p.workspace_id = "x".into();
            vec![p]
        },
        &Default::default(),
        1,
    );
    assert_eq!(snap.pane("x:p1").unwrap().cwd, "/home/project");
}

#[test]
fn picks_the_shells_direct_child_and_ignores_wrappers() {
    // claude pane: caffeinate is a wrapper, so the pick is claude (refresh skips agent
    // panes outright — agent_session is authoritative there).
    let fg = pick_foreground(&process_info("process_info_claude.json"), 7).unwrap();
    assert_eq!(fg.argv, ["claude", "--dangerously-skip-permissions"]);
    // npm (pid 50290) is older than its node child (50301)
    let fg = pick_foreground(&process_info("process_info_npm.json"), 7).unwrap();
    assert_eq!(fg.argv, ["npm", "run", "dev"]);
    assert_eq!(fg.captured_at_ms, 7);
    // idle shell → nothing to re-run
    assert!(pick_foreground(&process_info("process_info_shell.json"), 7).is_none());
}

#[test]
fn shell_prompt_helpers_are_never_mistaken_for_the_command() {
    // Observed live during QA: a snapshot taken mid-prompt saw `starship prompt` and
    // `/usr/libexec/path_helper -s` as the shell's foreground children.
    let info: RawProcessInfo = serde_json::from_value(serde_json::json!({
        "shell_pid": 100,
        "foreground_processes": [
            {"pid": 100, "argv0": "zsh", "argv": ["-zsh"], "cwd": "/tmp"},
            {"pid": 101, "argv0": "starship", "argv": ["/usr/local/bin/starship", "prompt"], "cwd": "/tmp"},
            {"pid": 102, "argv0": "path_helper", "argv": ["/usr/libexec/path_helper", "-s"], "cwd": "/tmp"}
        ]
    }))
    .unwrap();
    assert!(pick_foreground(&info, 1).is_none());
}

#[test]
fn shell_quoting_is_safe_for_prefill() {
    assert_eq!(
        shell_quote(&["tail".into(), "-f".into(), "/dev/null".into()]),
        "tail -f /dev/null"
    );
    assert_eq!(
        shell_quote(&["echo".into(), "two words".into()]),
        "echo 'two words'"
    );
    assert_eq!(shell_quote(&["it's".into()]), r"'it'\''s'");
}

#[test]
fn create_events_seed_the_cache_with_no_socket_calls() {
    let f = fixture("events_create.json");
    let mut snap = Snapshot::default();
    let w: RawWorkspace =
        serde_json::from_value(f["workspace_created"]["data"]["workspace"].clone()).unwrap();
    merge_workspace_created(&mut snap, &w);
    let t: RawTab = serde_json::from_value(f["tab_created"]["data"]["tab"].clone()).unwrap();
    merge_tab_created(&mut snap, &t);
    let p: RawPane = serde_json::from_value(f["pane_created"]["data"]["pane"].clone()).unwrap();
    merge_pane_created(&mut snap, &p, 5);
    assert_eq!(snap.workspaces.len(), 1);
    assert_eq!(snap.workspaces[0].label.as_deref(), Some("reopen-probe-ws"));
    assert_eq!(snap.workspaces[0].tabs.len(), 1);
    let pane = snap.pane("w3G:p1").expect("seeded pane");
    assert_eq!(pane.cwd, "/private/tmp");
    assert_eq!(pane.tab_id, "w3G:t1");
    // idempotent: the same pane arriving twice does not duplicate
    merge_pane_created(&mut snap, &p, 6);
    assert_eq!(snap.workspaces[0].tabs[0].panes.len(), 1);
}

#[test]
fn a_transient_shell_startup_child_is_never_captured_as_the_panes_command() {
    // THE LIVE QA FAILURE (scenario 5): a pane running `tail -f /dev/null` had
    // `mkdir -p ~/.oh-my-zsh/cache/completions` captured as its foreground command,
    // because the sample landed inside zsh's own startup and `mkdir` had a smaller pid
    // than `tail`.
    let info = process_info("process_info_zsh_startup.json");
    let fg = pick_foreground(&info, 7).expect("the long-lived child");
    assert_eq!(
        fg.argv,
        ["tail", "-f", "/dev/null"],
        "the smallest pid is not automatically the user's command"
    );
    assert_eq!(fg.pid, 50333);

    // …and when the transient helper is the ONLY non-shell child, the pane is idle:
    // remembering `mkdir` would prefill it on the next reopen.
    let only = process_info("process_info_zsh_startup_only.json");
    assert!(pick_foreground(&only, 7).is_none());
}

#[test]
fn legitimate_long_running_foreground_jobs_are_not_filtered() {
    // The transient list must never swallow a command a user can sit in front of.
    for (file, want) in [
        ("process_info_npm.json", vec!["npm", "run", "dev"]),
        (
            "process_info_python_http.json",
            vec!["python3", "-m", "http.server", "8000"],
        ),
    ] {
        let fg = pick_foreground(&process_info(file), 1).expect(file);
        assert_eq!(fg.argv, want, "{file}");
    }
    // `tail -f` and `watch` in particular: both are on the rerun allow list.
    let info: RawProcessInfo = serde_json::from_value(serde_json::json!({
    "shell_pid": 1, "foreground_processes": [
        {"pid": 1, "argv0": "zsh", "argv": ["-zsh"], "cwd": "/tmp"},
        {"pid": 2, "argv0": "watch", "argv": ["watch", "-n1", "ls"], "cwd": "/tmp"}
    ]}))
    .unwrap();
    assert_eq!(
        pick_foreground(&info, 1).unwrap().argv,
        ["watch", "-n1", "ls"]
    );
}

#[test]
fn a_dotfile_cache_cwd_marks_a_command_as_housekeeping() {
    assert!(is_housekeeping_cwd(
        "/Users/chaugs/.oh-my-zsh/cache/completions"
    ));
    assert!(is_housekeeping_cwd("/Users/chaugs/.cache/pip"));
    assert!(is_housekeeping_cwd("/Users/chaugs/.npm/_cacache"));
    // a real project directory is never housekeeping, even with a dot in the path
    assert!(!is_housekeeping_cwd(
        "/Users/chaugs/Personal/herdr-pane-reopen"
    ));
    assert!(!is_housekeeping_cwd("/Users/chaugs/.config/nvim"));
    assert!(!is_housekeeping_cwd("/var/cache/nginx"));
}

#[test]
fn a_command_that_does_not_survive_a_second_sample_is_not_confirmed() {
    // `pane.process_info` carries no start time, so "long-lived" can only be answered by
    // sampling twice: the mkdir is gone 120 ms later, the tail is not.
    let first = process_info("process_info_zsh_startup.json");
    let tail = pick_foreground(&first, 1).unwrap();
    assert!(confirms(&first, &tail));

    let gone = process_info("process_info_zsh_startup_only.json");
    let mkdir = ForegroundCmd {
        argv: vec!["mkdir".into(), "-p".into()],
        cwd: "/tmp".into(),
        pid: 50291,
        captured_at_ms: 1,
    };
    // same pid still present in the earlier sample…
    assert!(confirms(&gone, &mkdir));
    // …but not in the later one, where only zsh remains
    let idle = process_info("process_info_shell.json");
    assert!(!confirms(&idle, &mkdir));
    // a DIFFERENT process that happens to reuse the name is not a confirmation either
    let mut recycled = mkdir.clone();
    recycled.pid = 99999;
    assert!(!confirms(&gone, &recycled));
}
