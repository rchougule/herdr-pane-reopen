//! The manifest is part of the contract: herdr rejects unknown events and the action ids
//! are what the keybinding in the README refers to.

#[test]
fn manifest_declares_only_valid_hooks_and_the_documented_actions() {
    let s = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml"),
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&s).unwrap();
    assert_eq!(v["id"].as_str(), Some("rchougule.reopen"));
    assert_eq!(v["min_herdr_version"].as_str(), Some("0.9.1"));

    // `layout.updated` / `pane.updated` are NOT dispatched to plugin hooks (see docs/HERDR_API_NOTES.md).
    let valid = [
        "pane.closed",
        "tab.closed",
        "workspace.closed",
        "pane.created",
        "pane.focused",
        "pane.exited",
        "tab.created",
        "workspace.created",
        "pane.agent_detected",
        "pane.agent_status_changed",
    ];
    let events = v["events"].as_array().unwrap();
    for e in events {
        let on = e["on"].as_str().unwrap();
        assert!(
            valid.contains(&on),
            "{on} is not dispatched to plugin hooks"
        );
    }
    for want in [
        "pane.closed",
        "tab.closed",
        "workspace.closed",
        "pane.exited",
    ] {
        assert!(
            events.iter().any(|e| e["on"].as_str() == Some(want)),
            "{want}"
        );
    }

    let actions: Vec<&str> = v["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["reopen-last", "list", "doctor"]);
    // the picker is deferred to v0.2: the pane entrypoint stays, the action does not
    assert!(!actions.contains(&"pick"));
    assert_eq!(
        v["panes"].as_array().unwrap()[0]["id"].as_str(),
        Some("picker")
    );
}

#[test]
fn every_manifest_command_points_at_a_binary_this_crate_actually_builds() {
    // review F20.7: nothing caught a rename between Cargo.toml's [[bin]] and the
    // manifest's command paths, and a wrong path only shows up at runtime.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join("herdr-plugin.toml")).unwrap()).unwrap();
    let cargo: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();

    let bins: Vec<String> = cargo["bin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect();
    assert!(bins.contains(&"reopen".to_string()));

    let mut checked = 0;
    let mut check = |cmd: &toml::Value| {
        for arg in cmd.as_array().unwrap() {
            let a = arg.as_str().unwrap();
            for token in a.split(['"', '\'', ' ']) {
                if let Some(rest) = token.rsplit('/').next() {
                    if token.contains("target/release/") {
                        assert!(
                            bins.contains(&rest.to_string()),
                            "{token} is not a [[bin]] in Cargo.toml (have {bins:?})"
                        );
                        checked += 1;
                    }
                }
            }
        }
    };
    for key in ["startup", "events", "actions", "panes"] {
        if let Some(arr) = manifest.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(cmd) = item.get("command") {
                    check(cmd);
                }
            }
        }
    }
    assert!(
        checked >= 12,
        "expected every hook to be checked, saw {checked}"
    );
}
