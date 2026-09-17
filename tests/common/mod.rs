//! Fixture helpers. Every fixture under `tests/fixtures/` is real herdr output copied
//! from a live herdr 0.9.1 server (synthetic ones say so in a `_note`).

#![allow(dead_code)]

use reopen::model::*;
use reopen::snapshot::*;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn fixture(name: &str) -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `.result.<key>` as a typed value.
pub fn result_of<T: serde::de::DeserializeOwned>(name: &str, key: &str) -> T {
    let v = fixture(name);
    serde_json::from_value(v["result"][key].clone()).expect("shape")
}

pub fn panes() -> Vec<RawPane> {
    result_of("pane_list.json", "panes")
}
pub fn tabs() -> Vec<RawTab> {
    result_of("tab_list.json", "tabs")
}
pub fn workspaces() -> Vec<RawWorkspace> {
    result_of("workspace_list.json", "workspaces")
}
pub fn layout(name: &str) -> RawLayout {
    result_of(name, "layout")
}

pub fn layouts_for(pairs: &[(&str, &str)]) -> BTreeMap<String, RawLayout> {
    let mut m = BTreeMap::new();
    for (tab_id, file) in pairs {
        let mut l = layout(file);
        l.tab_id = tab_id.to_string();
        m.insert(tab_id.to_string(), l);
    }
    m
}

pub fn process_info(name: &str) -> RawProcessInfo {
    result_of(name, "process_info")
}

/// A snapshot with one workspace / one tab / `n` panes sharing a tab.
pub fn synth_snapshot(ws: &str, tab: &str, pane_ids: &[&str], cwd: &str) -> Snapshot {
    let layout = if pane_ids.len() == 1 {
        LayoutNode::pane(Some(pane_ids[0].into()), Some(cwd.into()), None)
    } else {
        let mut node = LayoutNode::pane(Some(pane_ids[0].into()), Some(cwd.into()), None);
        for id in &pane_ids[1..] {
            node = LayoutNode::Split {
                direction: SplitDir::Right,
                ratio: 0.5,
                first: Box::new(node),
                second: Box::new(LayoutNode::pane(Some((*id).into()), Some(cwd.into()), None)),
            };
        }
        node
    };
    let panes: Vec<PaneSnap> = pane_ids
        .iter()
        .map(|id| PaneSnap {
            pane_id: (*id).to_string(),
            tab_id: tab.to_string(),
            workspace_id: ws.to_string(),
            cwd: cwd.to_string(),
            ..Default::default()
        })
        .collect();
    Snapshot {
        taken_at_ms: 1,
        workspaces: vec![WorkspaceSnap {
            workspace_id: ws.to_string(),
            label: Some("scratch".into()),
            index: 0,
            number: 1,
            cwd: Some(cwd.to_string()),
            active_tab_id: Some(tab.to_string()),
            tabs: vec![TabSnap {
                tab_id: tab.to_string(),
                workspace_id: ws.to_string(),
                label: Some("work".into()),
                index: 0,
                number: 1,
                layout,
                focused_pane_id: None,
                panes,
            }],
        }],
    }
}

/// Parse one of the captured close events out of `events_close.json`.
pub fn close_event(key: &str, t0_override: Option<u64>) -> BurstEvent {
    let f = fixture("events_close.json");
    let e = &f[key];
    let name = e["event"].as_str().unwrap();
    let t0 = t0_override.unwrap_or_else(|| e["t0"].as_u64().unwrap());
    reopen::close::parse_event(
        name,
        &e["event_json"].to_string(),
        Some(&e["context_json"].to_string()),
        t0,
    )
    .expect("parse_event")
}

// ---------------------------------------------------------------- fake herdr socket

use reopen::rpc::{Error as RpcError, Result as RpcResult, Rpc};
use serde_json::json;
use std::sync::Mutex;

/// A scripted stand-in for the herdr socket. The restore driver talks to `&dyn Rpc`, so
/// every decision it makes (escalation, ordering, the swap, the alias ladder) can be
/// asserted from the recorded call sequence without touching a real herdr.
pub struct Fake {
    calls: Mutex<Vec<(String, Value)>>,
    #[allow(clippy::type_complexity)]
    handler: Box<dyn Fn(&str, &Value, usize) -> RpcResult<Value> + Send + Sync>,
}

impl Fake {
    pub fn new(
        h: impl Fn(&str, &Value, usize) -> RpcResult<Value> + Send + Sync + 'static,
    ) -> Fake {
        Fake {
            calls: Mutex::new(Vec::new()),
            handler: Box::new(h),
        }
    }

    /// Every method name, in call order.
    pub fn sequence(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.0.clone())
            .collect()
    }
    /// The params of every call to `method`, in order.
    pub fn calls_to(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.0 == method)
            .map(|c| c.1.clone())
            .collect()
    }
    pub fn count(&self, method: &str) -> usize {
        self.calls_to(method).len()
    }
    /// Index of the first call to `method` in the overall sequence.
    pub fn first_index(&self, method: &str) -> Option<usize> {
        self.sequence().iter().position(|m| m == method)
    }
}

impl Rpc for Fake {
    fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        _t: std::time::Duration,
    ) -> RpcResult<Value> {
        let n = {
            let mut g = self.calls.lock().unwrap();
            let n = g.iter().filter(|c| c.0 == method).count();
            g.push((method.to_string(), params.clone()));
            n
        };
        (self.handler)(method, &params, n)
    }
}

pub fn remote(code: &str) -> RpcError {
    RpcError::Remote {
        code: code.to_string(),
        message: format!("fake {code}"),
    }
}

pub fn transport_timeout() -> RpcError {
    RpcError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "fake read timeout",
    ))
}

/// Fill a stripped `layout.apply` root with fresh pane ids, depth first.
fn fill_ids(node: &Value, next: &mut u32) -> Value {
    let mut n = node.clone();
    if n["type"] == "pane" {
        n["pane_id"] = json!(format!("new:p{}", next));
        *next += 1;
        return n;
    }
    n["first"] = fill_ids(&node["first"], next);
    n["second"] = fill_ids(&node["second"], next);
    n
}

/// The default herdr: empty world, every mutation succeeds.
/// `live` is the `{workspaces, tabs, panes}` the list calls report.
pub fn default_reply(
    live: Value,
) -> impl Fn(&str, &Value, usize) -> RpcResult<Value> + Send + Sync {
    let seq = Mutex::new(1u32);
    let tabs = Mutex::new(0u32);
    move |method: &str, params: &Value, _n: usize| -> RpcResult<Value> {
        Ok(match method {
            "ping" => json!({"version": "0.9.1", "protocol": 22}),
            "workspace.list" => json!({"workspaces": live["workspaces"]}),
            "tab.list" => json!({"tabs": live["tabs"]}),
            "pane.list" => json!({"panes": live["panes"]}),
            "workspace.create" => json!({
                "workspace": {"workspace_id": "new:w"},
                "tab": {"tab_id": "new:t0"},
                "root_pane": {"pane_id": "new:p0"}
            }),
            "layout.apply" => {
                let mut s = seq.lock().unwrap();
                let mut t = tabs.lock().unwrap();
                let tab_id = params
                    .get("tab_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        *t += 1;
                        format!("new:t{t}")
                    });
                let root = fill_ids(&params["root"], &mut s);
                json!({"layout": {"tab_id": tab_id, "root": root}})
            }
            "pane.split" => {
                let mut s = seq.lock().unwrap();
                let id = format!("new:p{s}");
                *s += 1;
                json!({"pane": {"pane_id": id}})
            }
            _ => json!({"ok": true}),
        })
    }
}

// ---------------------------------------------------------------- restore Ctx

use reopen::config::Config;
use reopen::restore::{Ctx, Verify};
use reopen::store::Store;

/// Fast timings for the post-start liveness check: the same state machine the real
/// restore runs, in milliseconds instead of seconds.
pub fn fast_verify() -> Verify {
    Verify {
        budget_ms: 60,
        interval_ms: 5,
        settle_ms: 20,
        exit_streak: 2,
    }
}

/// A `Ctx` for driver tests. `projects_dir` defaults to `None` — "no Claude Code
/// transcript store we can consult" — so the pre-check never reaches the developer's
/// real `~/.claude/projects`. Tests that exercise the pre-check pass a tempdir.
pub fn test_ctx<'a>(
    client: &'a dyn Rpc,
    store: &'a Store,
    cfg: &'a Config,
    projects_dir: Option<std::path::PathBuf>,
) -> Ctx<'a> {
    Ctx {
        verify: fast_verify(),
        projects_dir,
        ..Ctx::new(client, store, cfg)
    }
}
