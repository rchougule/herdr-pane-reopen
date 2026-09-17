//! Restore: `ClosedEntry` → herdr calls. Layout-tree surgery (`strip_pane_ids`,
//! `locate`, `map_pane_ids`, `same_container`) is pure and unit-tested; the rest drives
//! the socket.

use crate::agents;
use crate::config::{Config, Rerun, RerunMode};
use crate::model::*;
use crate::rpc::{Error as RpcError, Rpc};
use crate::snapshot::{shell_quote, RawPane, RawTab, RawWorkspace};
use crate::store::Store;
use crate::{linfo, lwarn};
use serde::Serialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------- pure helpers

/// `layout.export` output is directly reusable as `layout.apply` input once the
/// `pane_id`s are stripped (they name dead panes).
pub fn strip_pane_ids(node: &LayoutNode) -> LayoutNode {
    match node {
        LayoutNode::Pane {
            cwd,
            label,
            command,
            env,
            ..
        } => LayoutNode::Pane {
            pane_id: None,
            cwd: cwd.clone(),
            label: label.clone(),
            command: command.clone(),
            env: env.clone(),
        },
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => LayoutNode::Split {
            direction: *direction,
            ratio: *ratio,
            first: Box::new(strip_pane_ids(first)),
            second: Box::new(strip_pane_ids(second)),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    First,
    Second,
}

#[derive(Debug, Clone)]
pub struct Located {
    pub direction: SplitDir,
    pub ratio: f64,
    pub side: Side,
    pub sibling: LayoutNode,
}

/// Find the split that had this pane as a child, which side it was on, and its sibling.
pub fn locate(node: &LayoutNode, pane_id: &str) -> Option<Located> {
    match node {
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            if matches!(&**first, LayoutNode::Pane { pane_id: Some(p), .. } if p == pane_id) {
                return Some(Located {
                    direction: *direction,
                    ratio: *ratio,
                    side: Side::First,
                    sibling: (**second).clone(),
                });
            }
            if matches!(&**second, LayoutNode::Pane { pane_id: Some(p), .. } if p == pane_id) {
                return Some(Located {
                    direction: *direction,
                    ratio: *ratio,
                    side: Side::Second,
                    sibling: (**first).clone(),
                });
            }
            locate(first, pane_id).or_else(|| locate(second, pane_id))
        }
    }
}

/// Depth-first positional map old pane id → new pane id, as `layout.apply` fills them in.
pub fn map_pane_ids(old: &LayoutNode, new: &LayoutNode) -> Vec<(String, String)> {
    let o = old.leaves();
    let n = new.leaves();
    let mut out = Vec::new();
    for (a, b) in o.iter().zip(n.iter()) {
        if let (
            LayoutNode::Pane {
                pane_id: Some(x), ..
            },
            LayoutNode::Pane {
                pane_id: Some(y), ..
            },
        ) = (a, b)
        {
            out.push((x.clone(), y.clone()));
        }
    }
    out
}

pub fn canon(p: &str) -> String {
    std::fs::canonicalize(p)
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.trim_end_matches('/').to_string())
}

/// herdr recycles container ids, so a matching id is not proof of identity.
pub fn same_container(
    remembered_id: &str,
    remembered_label: Option<&str>,
    remembered_cwds: &[String],
    live_id: &str,
    live_label: Option<&str>,
    live_cwds: &[String],
) -> bool {
    if remembered_id != live_id {
        return false;
    }
    let labels_match = match (remembered_label, live_label) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    };
    if labels_match {
        return true;
    }
    let live: Vec<String> = live_cwds.iter().map(|c| canon(c)).collect();
    remembered_cwds.iter().any(|c| live.contains(&canon(c)))
}

/// `tab.move` target: the remembered ARRAY index, clamped to the live tab count.
/// Never derived from `tab.number`, which is a creation ordinal.
pub fn insert_index(remembered_index: usize, live_tab_count: usize) -> usize {
    remembered_index.min(live_tab_count)
}

/// herdr auto-labels tabs "1", "2"… — treat an all-digit label as unset.
pub fn effective_tab_label(label: Option<&str>) -> Option<String> {
    let l = label?;
    if l.is_empty() || l.chars().all(|c| c.is_ascii_digit()) {
        None
    } else {
        Some(l.to_string())
    }
}

/// A workspace label equal to `basename(cwd)` is herdr's own default.
pub fn effective_ws_label(label: Option<&str>, cwd: Option<&str>) -> Option<String> {
    let l = label?;
    if l.is_empty() {
        return None;
    }
    if let Some(c) = cwd {
        let b = std::path::Path::new(c)
            .file_name()
            .map(|s| s.to_string_lossy().to_string());
        if b.as_deref() == Some(l) {
            return None;
        }
    }
    Some(l.to_string())
}

// ---------------------------------------------------------------- report

#[derive(Debug, Default, Serialize)]
pub struct Created {
    pub workspaces: Vec<String>,
    pub tabs: Vec<String>,
    pub panes: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub entry_id: u64,
    pub granularity: String,
    pub summary: String,
    pub created: Created,
    pub resumed: usize,
    pub prefilled: usize,
    pub failed: Vec<String>,
    pub notes: Vec<String>,
    pub ok: bool,
}

impl Report {
    /// One predicate, used both for `ok` and for the push-back decision in `main` —
    /// they disagreed about created WORKSPACES, which left an empty workspace behind
    /// AND pushed the entry back, so a second undo created a second one (F12).
    pub fn created_nothing(&self) -> bool {
        self.created.panes.is_empty()
            && self.created.tabs.is_empty()
            && self.created.workspaces.is_empty()
    }
    pub fn note(&mut self, s: impl Into<String>) {
        self.notes.push(s.into());
    }
    pub fn fail(&mut self, s: impl Into<String>) {
        self.failed.push(s.into());
    }
}

// ---------------------------------------------------------------- live index

pub struct Live {
    pub workspaces: Vec<RawWorkspace>,
    pub tabs: Vec<RawTab>,
    pub panes: Vec<RawPane>,
}

impl Live {
    pub fn fetch(c: &dyn Rpc) -> Live {
        fn arr<T: serde::de::DeserializeOwned>(c: &dyn Rpc, m: &str, k: &str) -> Vec<T> {
            c.call(m, json!({}))
                .ok()
                .and_then(|v| v.get(k).cloned())
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default()
        }
        Live {
            workspaces: arr(c, "workspace.list", "workspaces"),
            tabs: arr(c, "tab.list", "tabs"),
            panes: arr(c, "pane.list", "panes"),
        }
    }
    pub fn workspace(&self, id: &str) -> Option<&RawWorkspace> {
        self.workspaces.iter().find(|w| w.workspace_id == id)
    }
    pub fn tab(&self, id: &str) -> Option<&RawTab> {
        self.tabs.iter().find(|t| t.tab_id == id)
    }
    pub fn tab_cwds(&self, tab_id: &str) -> Vec<String> {
        self.panes
            .iter()
            .filter(|p| p.tab_id == tab_id)
            .map(|p| p.cwd.clone())
            .collect()
    }
    pub fn ws_cwds(&self, ws_id: &str) -> Vec<String> {
        self.panes
            .iter()
            .filter(|p| p.workspace_id == ws_id)
            .map(|p| p.cwd.clone())
            .collect()
    }
    pub fn tabs_of(&self, ws_id: &str) -> Vec<&RawTab> {
        self.tabs
            .iter()
            .filter(|t| t.workspace_id == ws_id)
            .collect()
    }
    pub fn panes_of_tab(&self, tab_id: &str) -> Vec<&RawPane> {
        self.panes.iter().filter(|p| p.tab_id == tab_id).collect()
    }
}

// ---------------------------------------------------------------- entry point

pub struct Ctx<'a> {
    pub client: &'a dyn Rpc,
    pub store: &'a Store,
    pub cfg: &'a Config,
    /// Allow/deny patterns compiled ONCE per restore instead of once per pane.
    pub rerun: CompiledRerun,
}

impl<'a> Ctx<'a> {
    pub fn new(client: &'a dyn Rpc, store: &'a Store, cfg: &'a Config) -> Ctx<'a> {
        Ctx {
            client,
            store,
            cfg,
            rerun: CompiledRerun::new(&cfg.rerun),
        }
    }
}

/// NOTE on budgets: `snapshot::refresh` carries a wall-clock budget because it runs
/// inside event hooks, where herdr is waiting on us. A restore does NOT: it is
/// user-initiated, and abandoning one half way leaves a partly built workspace that is
/// worse than a slow one. Its worst case is bounded instead by the per-call timeouts and
/// by `AGENT_SETTLE_BUDGET_MS` per agent pane.
pub fn run(entry: &ClosedEntry, ctx: &Ctx) -> Report {
    let mut report = Report {
        entry_id: entry.id,
        granularity: entry.granularity.as_str().to_string(),
        summary: entry.summary.clone(),
        ok: true,
        ..Default::default()
    };
    if entry.protocol != 0 && entry.protocol != crate::EXPECTED_PROTOCOL {
        report.note(format!(
            "entry captured on socket protocol {} but this build expects {}",
            entry.protocol,
            crate::EXPECTED_PROTOCOL
        ));
    }
    let live = Live::fetch(ctx.client);

    match entry.granularity {
        Granularity::Workspace => restore_workspace(&entry.workspace, ctx, &live, &mut report),
        Granularity::WorkspaceGroup => {
            let mut all = vec![entry.workspace.clone()];
            all.extend(entry.group.iter().cloned());
            all.sort_by_key(|w| w.index);
            for w in &all {
                restore_workspace(w, ctx, &live, &mut report);
            }
        }
        Granularity::Tab => {
            if let Some(tab) = entry.workspace.tabs.first() {
                restore_tab_granularity(&entry.workspace, tab, ctx, &live, &mut report);
            } else {
                report.fail("tab entry carries no tab");
            }
        }
        Granularity::Pane => restore_pane_granularity(entry, ctx, &live, &mut report),
    }

    report.ok = report.failed.is_empty() && !report.created_nothing();
    report
}

fn record_created(ctx: &Ctx, ids: &[String]) {
    ctx.store.record_self_created(ids);
}

// ---------------------------------------------------------------- workspace

fn restore_workspace(ws: &WorkspaceSnap, ctx: &Ctx, live: &Live, report: &mut Report) {
    let remembered_cwds: Vec<String> = ws
        .tabs
        .iter()
        .flat_map(|t| t.layout.leaf_cwds())
        .chain(ws.cwd.clone())
        .collect();
    let alive = live.workspace(&ws.workspace_id).is_some_and(|w| {
        same_container(
            &ws.workspace_id,
            ws.label.as_deref(),
            &remembered_cwds,
            &w.workspace_id,
            w.label.as_deref(),
            &live.ws_cwds(&ws.workspace_id),
        )
    });
    if alive {
        report.note(format!(
            "workspace {} still exists; restoring only its missing tabs",
            ws.workspace_id
        ));
        for tab in &ws.tabs {
            let tab_alive = live.tab(&tab.tab_id).is_some_and(|t| {
                same_container(
                    &tab.tab_id,
                    tab.label.as_deref(),
                    &tab.layout.leaf_cwds(),
                    &t.tab_id,
                    t.label.as_deref(),
                    &live.tab_cwds(&tab.tab_id),
                )
            });
            if !tab_alive {
                create_tab_from_layout(&ws.workspace_id, tab, ctx, live, report);
            }
        }
        return;
    }

    let cwd = ws.first_cwd().unwrap_or_else(|| "~".to_string());
    let mut params = json!({"cwd": cwd, "focus": false});
    if let Some(l) = effective_ws_label(ws.label.as_deref(), Some(&cwd)) {
        params["label"] = json!(l);
    }
    let created = match ctx.client.call("workspace.create", params) {
        Ok(v) => v,
        Err(e) => {
            report.fail(format!("workspace.create failed: {e}"));
            return;
        }
    };
    let new_ws = created
        .pointer("/workspace/workspace_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let placeholder = created
        .pointer("/tab/tab_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let root_pane = created
        .pointer("/root_pane/pane_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut ids = vec![new_ws.clone()];
    ids.extend(placeholder.clone());
    ids.extend(root_pane);
    record_created(ctx, &ids);
    report.created.workspaces.push(new_ws.clone());

    let mut created_tabs: Vec<String> = Vec::new();
    for (i, tab) in ws.tabs.iter().enumerate() {
        let root = strip_pane_ids(&tab.layout);
        let mut params = json!({"root": root, "focus": false});
        if i == 0 {
            if let Some(p) = &placeholder {
                params["tab_id"] = json!(p);
            } else {
                params["workspace_id"] = json!(new_ws);
            }
        } else {
            params["workspace_id"] = json!(new_ws);
            if let Some(l) = effective_tab_label(tab.label.as_deref()) {
                params["tab_label"] = json!(l);
            }
        }
        let Some((new_tab, new_root)) = apply_layout(ctx, params, report) else {
            continue;
        };
        created_tabs.push(new_tab.clone());
        report.created.tabs.push(new_tab.clone());
        if i == 0 {
            if let Some(l) = effective_tab_label(tab.label.as_deref()) {
                let _ = ctx
                    .client
                    .ok("tab.rename", json!({"tab_id": new_tab, "label": l}));
            }
        }
        finish_tab(tab, &new_root, ctx, report);
    }
    // layout.apply APPENDS, so fix the order explicitly.
    for (i, t) in created_tabs.iter().enumerate() {
        let _ = ctx
            .client
            .ok("tab.move", json!({"tab_id": t, "insert_index": i}));
    }
    if ctx.cfg.focus_on_reopen {
        // The remembered active tab, by position among the tabs we just recreated (F14).
        if let Some(active) = &ws.active_tab_id {
            if let Some(i) = ws.tabs.iter().position(|t| &t.tab_id == active) {
                if let Some(new_tab) = created_tabs.get(i) {
                    let _ = ctx.client.ok("tab.focus", json!({ "tab_id": new_tab }));
                }
            }
        }
        let _ = ctx
            .client
            .ok("workspace.focus", json!({"workspace_id": new_ws}));
    }
}

/// `layout.apply` → (new tab id, returned layout root with new pane ids).
fn apply_layout(ctx: &Ctx, params: Value, report: &mut Report) -> Option<(String, LayoutNode)> {
    match ctx.client.call("layout.apply", params) {
        Ok(v) => {
            let tab_id = v
                .pointer("/layout/tab_id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let root: Option<LayoutNode> = v
                .pointer("/layout/root")
                .cloned()
                .and_then(|r| serde_json::from_value(r).ok());
            match root {
                Some(r) => {
                    let mut ids = vec![tab_id.clone()];
                    ids.extend(r.leaf_pane_ids());
                    record_created(ctx, &ids);
                    Some((tab_id, r))
                }
                None => {
                    report.fail("layout.apply returned no usable root".to_string());
                    None
                }
            }
        }
        Err(e) => {
            report.fail(format!("layout.apply failed: {e}"));
            None
        }
    }
}

/// After a tab is rebuilt: map old→new panes and restore each occupant.
fn finish_tab(tab: &TabSnap, new_root: &LayoutNode, ctx: &Ctx, report: &mut Report) {
    let mapping = map_pane_ids(&tab.layout, new_root);
    for new_id in new_root.leaf_pane_ids() {
        report.created.panes.push(new_id);
    }
    for (old, new) in &mapping {
        if let Some(p) = tab.panes.iter().find(|p| &p.pane_id == old) {
            restore_occupant(p, new, ctx, report);
        }
    }
    // The remembered focus is mapped through the same positional map (F14).
    if ctx.cfg.focus_on_reopen {
        if let Some(old) = &tab.focused_pane_id {
            if let Some((_, new)) = mapping.iter().find(|(o, _)| o == old) {
                let _ = ctx.client.ok("pane.focus", json!({ "pane_id": new }));
            }
        }
    }
}

// ---------------------------------------------------------------- tab

fn create_tab_from_layout(ws_id: &str, tab: &TabSnap, ctx: &Ctx, live: &Live, report: &mut Report) {
    let root = strip_pane_ids(&tab.layout);
    let mut params = json!({"root": root, "workspace_id": ws_id, "focus": false});
    if let Some(l) = effective_tab_label(tab.label.as_deref()) {
        params["tab_label"] = json!(l);
    }
    let Some((new_tab, new_root)) = apply_layout(ctx, params, report) else {
        return;
    };
    report.created.tabs.push(new_tab.clone());
    // `live` was fetched before this restore began; the tab we just applied was APPENDED
    // to the workspace, so the true count is one higher (F13).
    let live_count = live.tabs_of(ws_id).len() + 1;
    let _ = ctx.client.ok(
        "tab.move",
        json!({"tab_id": new_tab, "insert_index": insert_index(tab.index, live_count)}),
    );
    finish_tab(tab, &new_root, ctx, report);
    if ctx.cfg.focus_on_reopen {
        let _ = ctx.client.ok("tab.focus", json!({"tab_id": new_tab}));
    }
}

fn restore_tab_granularity(
    ws: &WorkspaceSnap,
    tab: &TabSnap,
    ctx: &Ctx,
    live: &Live,
    report: &mut Report,
) {
    let ws_alive = live.workspace(&ws.workspace_id).is_some_and(|w| {
        same_container(
            &ws.workspace_id,
            ws.label.as_deref(),
            &tab.layout.leaf_cwds(),
            &w.workspace_id,
            w.label.as_deref(),
            &live.ws_cwds(&ws.workspace_id),
        )
    });
    if !ws_alive {
        report.note("workspace is gone; recreating it around this tab");
        restore_workspace(ws, ctx, live, report);
        return;
    }
    let tab_alive = live.tab(&tab.tab_id).is_some_and(|t| {
        same_container(
            &tab.tab_id,
            tab.label.as_deref(),
            &tab.layout.leaf_cwds(),
            &t.tab_id,
            t.label.as_deref(),
            &live.tab_cwds(&tab.tab_id),
        )
    });
    if tab_alive {
        report.note(format!("tab {} is already open; nothing to do", tab.tab_id));
        return;
    }
    create_tab_from_layout(&ws.workspace_id, tab, ctx, live, report);
}

// ---------------------------------------------------------------- pane

fn restore_pane_granularity(entry: &ClosedEntry, ctx: &Ctx, live: &Live, report: &mut Report) {
    let ws = &entry.workspace;
    let Some(tab) = ws.tabs.first() else {
        report.fail("pane entry carries no tab");
        return;
    };
    let Some(pane) = tab.panes.first().cloned() else {
        report.fail("pane entry carries no pane");
        return;
    };

    let tab_alive = live.tab(&tab.tab_id).is_some_and(|t| {
        same_container(
            &tab.tab_id,
            tab.label.as_deref(),
            &tab.layout.leaf_cwds(),
            &t.tab_id,
            t.label.as_deref(),
            &live.tab_cwds(&tab.tab_id),
        )
    });
    if !tab_alive {
        // Escalate to Tab, but only this pane's leaf: the tab's other panes were closed
        // separately and own their own entries.
        report.note("original tab is gone; restoring this pane in a new tab");
        let single = TabSnap {
            layout: LayoutNode::pane(
                Some(pane.pane_id.clone()),
                Some(pane.cwd.clone()),
                pane.label.clone(),
            ),
            panes: vec![pane.clone()],
            ..tab.clone()
        };
        let mut ws2 = ws.clone();
        ws2.tabs = vec![single.clone()];
        restore_tab_granularity(&ws2, &single, ctx, live, report);
        return;
    }

    let live_panes: Vec<String> = live
        .panes_of_tab(&tab.tab_id)
        .iter()
        .map(|p| p.pane_id.clone())
        .collect();
    if live_panes.is_empty() {
        report.fail("tab has no live pane to split from");
        return;
    }

    let located = locate(&tab.layout, &pane.pane_id);
    let (direction, ratio, side, sibling_leaves) = match &located {
        Some(l) => (l.direction, l.ratio, l.side, l.sibling.leaf_pane_ids()),
        None => {
            report.note("no layout record for this pane; splitting the tab's first pane");
            (SplitDir::Right, 0.5, Side::Second, Vec::new())
        }
    };
    let anchor = sibling_leaves
        .iter()
        .find(|id| live_panes.contains(id))
        .cloned()
        .unwrap_or_else(|| live_panes[0].clone());

    let sibling_is_single_live_leaf =
        sibling_leaves.len() == 1 && sibling_leaves.first() == Some(&anchor);

    let split_ratio = if side == Side::First && !sibling_is_single_live_leaf {
        1.0 - ratio
    } else {
        ratio
    };
    let params = json!({
        "target_pane_id": anchor,
        "direction": direction.as_str(),
        "ratio": split_ratio,
        "cwd": pane.cwd,
        "focus": false,
    });
    let new_pane = match ctx.client.call("pane.split", params) {
        Ok(v) => v
            .pointer("/pane/pane_id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        Err(e) => {
            report.fail(format!("pane.split failed: {e}"));
            return;
        }
    };
    if new_pane.is_empty() {
        report.fail("pane.split returned no pane id");
        return;
    }
    record_created(ctx, std::slice::from_ref(&new_pane));
    report.created.panes.push(new_pane.clone());

    if side == Side::First {
        if sibling_is_single_live_leaf {
            // pane.split always puts the new pane `second`; swap them back.
            let _ = ctx.client.ok(
                "pane.swap",
                json!({"source_pane_id": new_pane, "target_pane_id": anchor}),
            );
        } else {
            report.note("pane restored on the opposite side of its original split");
        }
    }

    if let Some(l) = &pane.label {
        let _ = ctx
            .client
            .ok("pane.rename", json!({"pane_id": new_pane, "label": l}));
    }
    restore_occupant(&pane, &new_pane, ctx, report);
    if ctx.cfg.focus_on_reopen {
        let _ = ctx.client.ok("pane.focus", json!({"pane_id": new_pane}));
    }
}

// ---------------------------------------------------------------- occupants

fn restore_occupant(pane: &PaneSnap, new_pane_id: &str, ctx: &Ctx, report: &mut Report) {
    if let Some(kind) = pane.agent.clone() {
        let session = pane
            .agent_session
            .clone()
            .filter(|v| !v.trim().is_empty())
            .filter(|_| pane.agent_session_kind.as_deref() == Some("id"));
        match session {
            Some(id) => match agents::resume_args(ctx.cfg, &kind, &id) {
                Some((args, confidence)) => {
                    match start_agent(&kind, &args, new_pane_id, pane, ctx, report) {
                        Ok(name) => {
                            linfo!("resumed {kind} as '{name}' in {new_pane_id} ({})", confidence.as_str());
                            report.resumed += 1;
                        }
                        Err(e) => {
                            report.fail(format!(
                                "{new_pane_id}: could not resume {kind}: {}",
                                e.message
                            ));
                            if e.safe_to_prefill {
                                let mut argv = vec![kind.clone()];
                                argv.extend(args);
                                prefill(&argv, new_pane_id, ctx, report);
                            } else {
                                report.note(format!(
                                    "{new_pane_id}: not prefilling — herdr may still be starting \
                                     the agent in this pane"
                                ));
                            }
                        }
                    }
                }
                None => report.fail(format!(
                    "{new_pane_id}: cannot resume {kind} (unsupported kind; set [resume.{kind}] in config.toml)"
                )),
            },
            None => report.fail(format!(
                "{new_pane_id}: cannot resume {kind} (no session id captured)"
            )),
        }
        return;
    }
    if let Some(fg) = &pane.foreground {
        match ctx.cfg.rerun.mode {
            RerunMode::Off => {}
            RerunMode::Prefill => prefill(&fg.argv, new_pane_id, ctx, report),
            RerunMode::Run => {
                if rerun_allowed(&ctx.rerun, &fg.argv) {
                    prefill(&fg.argv, new_pane_id, ctx, report);
                    let _ = ctx.client.ok(
                        "pane.send_keys",
                        json!({"pane_id": new_pane_id, "keys": ["Enter"]}),
                    );
                } else {
                    report.note(format!(
                        "{new_pane_id}: '{}' is not on the rerun allow list; prefilled only",
                        fg.argv.first().cloned().unwrap_or_default()
                    ));
                    prefill(&fg.argv, new_pane_id, ctx, report);
                }
            }
        }
    }
}

/// Allow/deny patterns, compiled once.
///
/// A deny pattern that does not compile must FAIL CLOSED: silently skipping it (the
/// previous behaviour) meant a user who typo'd `^(rm|dd` believed `rm` was blocked while
/// `mode = "run"` happily pressed Enter on it (F8).
#[derive(Debug, Default)]
pub struct CompiledRerun {
    pub allow: Vec<regex::Regex>,
    pub deny: Vec<regex::Regex>,
    /// Patterns that did not compile. Any entry here denies everything.
    pub broken_deny: Vec<String>,
}

impl CompiledRerun {
    pub fn new(r: &Rerun) -> CompiledRerun {
        let mut out = CompiledRerun::default();
        for d in &r.deny {
            match regex::Regex::new(d) {
                Ok(re) => out.deny.push(re),
                Err(e) => {
                    lwarn!("deny pattern {d:?} does not compile ({e}); refusing to rerun anything");
                    out.broken_deny.push(d.clone());
                }
            }
        }
        for a in &r.allow {
            match regex::Regex::new(a) {
                Ok(re) => out.allow.push(re),
                Err(e) => lwarn!("allow pattern {a:?} does not compile ({e}); ignoring it"),
            }
        }
        out
    }
}

/// Pure: does this argv pass the allow/deny lists?
///
/// `allow` is matched against `argv[0]` ONLY, so an allowed binary is allowed whatever
/// it is told to do (`^(npm)$` admits every `npm run <script>`). `deny` is matched
/// against both `argv[0]` and the whole command line.
pub fn rerun_allowed(rerun: &CompiledRerun, argv: &[String]) -> bool {
    if !rerun.broken_deny.is_empty() {
        return false;
    }
    let Some(a0) = argv.first() else { return false };
    let a0 = std::path::Path::new(a0)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| a0.clone());
    let joined = argv.join(" ");
    for re in &rerun.deny {
        if re.is_match(&a0) || re.is_match(&joined) {
            return false;
        }
    }
    rerun.allow.iter().any(|re| re.is_match(&a0))
}

fn prefill(argv: &[String], pane_id: &str, ctx: &Ctx, report: &mut Report) {
    if argv.is_empty() {
        return;
    }
    let text = shell_quote(argv);
    match ctx
        .client
        .ok("pane.send_text", json!({"pane_id": pane_id, "text": text}))
    {
        Ok(()) => report.prefilled += 1,
        Err(e) => report.fail(format!("{pane_id}: prefill failed: {e}")),
    }
}

/// Why an `agent.start` gave up, and whether the pane is safe to type into afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFailure {
    pub message: String,
    /// False when herdr may still be starting the agent in that pane — prefilling then
    /// types garbage straight into the agent's own prompt (F2).
    pub safe_to_prefill: bool,
}

/// herdr's server-side startup timeout for `agent.start`, in ms.
pub const AGENT_START_TIMEOUT_MS: u64 = 60_000;
/// Transport timeout for `agent.start`: the server-side budget plus slack. It MUST be
/// larger than `AGENT_START_TIMEOUT_MS`, otherwise the transport gives up first and the
/// `"timeout"` remote-error arm below can never be reached (F2).
pub const AGENT_START_READ_TIMEOUT_MS: u64 = 75_000;
/// How long a freshly created pane is allowed to keep answering `agent_pane_busy`.
pub const AGENT_SETTLE_BUDGET_MS: u64 = 12_000;

/// `agent.start` with the settle-retry loop (a fresh pane is `agent_pane_busy` for
/// ~1-3 s) and the reactive `agent_name_taken` alias ladder.
fn start_agent(
    kind: &str,
    args: &[String],
    pane_id: &str,
    pane: &PaneSnap,
    ctx: &Ctx,
    report: &mut Report,
) -> Result<String, StartFailure> {
    let fail = |m: String| StartFailure {
        message: m,
        safe_to_prefill: true,
    };
    let base = agents::alias(&agents::alias_seed(pane.label.as_deref(), &pane.cwd));
    // `alias()` must produce something herdr accepts; if a future change to it does not,
    // fall back rather than burning a ladder step on a guaranteed rejection (F16).
    let mut candidates = agents::alias_candidates(&base)
        .into_iter()
        .filter(|c| agents::is_valid_alias(c))
        .collect::<Vec<_>>()
        .into_iter();
    let mut name = candidates.next().unwrap_or_else(|| "reopened".to_string());
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(AGENT_SETTLE_BUDGET_MS);
    loop {
        let params = json!({
            "name": name,
            "kind": kind,
            "pane_id": pane_id,
            "args": args,
            "timeout_ms": AGENT_START_TIMEOUT_MS,
        });
        let res = ctx.client.call_with_timeout(
            "agent.start",
            params,
            std::time::Duration::from_millis(AGENT_START_READ_TIMEOUT_MS),
        );
        match res {
            Ok(_) => return Ok(name),
            Err(RpcError::Remote { code, message }) => match code.as_str() {
                "agent_pane_busy" => {
                    if std::time::Instant::now() >= deadline {
                        return Err(fail("pane never settled (agent_pane_busy)".into()));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                "agent_name_taken" | "invalid_agent_name" => match candidates.next() {
                    Some(n) => name = n,
                    None => return Err(fail(format!("{code}: {message}"))),
                },
                "timeout" => {
                    report.note(format!(
                        "{pane_id}: agent.start timed out; the agent may still be starting"
                    ));
                    return Ok(name);
                }
                _ => return Err(fail(format!("{code}: {message}"))),
            },
            Err(e) if e.is_timeout() => {
                return Err(StartFailure {
                    message: format!("transport timed out after {AGENT_START_READ_TIMEOUT_MS}ms"),
                    safe_to_prefill: false,
                })
            }
            Err(e) => return Err(fail(e.to_string())),
        }
    }
}

/// Push a popped entry back after a total failure.
pub fn push_back(store: &Store, entry: &ClosedEntry) {
    let _g = store.lock();
    let mut stack = store.closed();
    if !stack.entries.iter().any(|e| e.id == entry.id) {
        stack.entries.insert(0, entry.clone());
        stack.entries.truncate(crate::STACK_CAP);
        if let Err(e) = store.write_closed(&stack) {
            lwarn!("cannot push entry back: {e}");
        }
    }
}
