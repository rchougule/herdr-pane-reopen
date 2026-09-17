//! Close correlation: event → one global 250 ms coalescing burst → `ClosedEntry`.
//!
//! herdr emits exactly one event per gesture, at the coarsest level affected
//! (see docs/HERDR_API_NOTES.md), so an implicit tab collapse has to be inferred from the
//! shadow cache. Everything here except `on_close` is pure and unit-tested.

use crate::model::*;
use crate::{linfo, lwarn, now_ms, BURST_STALE_MS, BURST_WINDOW_MS};
use serde_json::Value;

pub const CLOSE_EVENTS: &[&str] = &[
    "pane.closed",
    "tab.closed",
    "workspace.closed",
    "pane.exited",
];

pub fn is_close_event(name: &str) -> bool {
    CLOSE_EVENTS.contains(&name)
}

/// Unique per hook process: two hooks landing on the same millisecond is routine
///, so ownership can never be a timestamp comparison.
pub fn make_token(t0: u64, pid: u32, nonce: u64) -> String {
    format!("{t0}-{pid}-{nonce}")
}

pub fn nonce() -> u64 {
    // Address entropy + nanoseconds; no rand dependency needed.
    let x = Box::new(0u8);
    let addr = &*x as *const u8 as u64;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    addr ^ (nanos << 17) ^ (std::process::id() as u64)
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
}

/// Parse `HERDR_PLUGIN_EVENT_JSON` (+ `HERDR_PLUGIN_CONTEXT_JSON`) into a `BurstEvent`.
/// Never panics: malformed input yields `None` and a log line at the call site.
pub fn parse_event(
    name: &str,
    event_json: &str,
    ctx_json: Option<&str>,
    t0: u64,
) -> Option<BurstEvent> {
    let v: Value = serde_json::from_str(event_json).ok()?;
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let ctx: Value = ctx_json
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or(Value::Null);
    let ws_obj = data.get("workspace").cloned().unwrap_or(Value::Null);
    Some(BurstEvent {
        t0,
        name: name.to_string(),
        pane_id: s(&data, "pane_id"),
        tab_id: s(&data, "tab_id").or_else(|| s(&ctx, "tab_id")),
        workspace_id: s(&data, "workspace_id").or_else(|| s(&ctx, "workspace_id")),
        workspace_label: s(&ws_obj, "label").or_else(|| s(&ctx, "workspace_label")),
        ctx_cwd: s(&ctx, "focused_pane_cwd").or_else(|| s(&ctx, "workspace_cwd")),
    })
}

/// Pick the snapshot that still remembers what this event names. The daemon may have
/// refreshed `snapshot.json` past the close before this hook process even started, which
/// is why `snapshot.prev.json` exists. Pure.
pub fn best_snapshot<'a>(snaps: &'a [&'a Snapshot], ev: &BurstEvent) -> &'a Snapshot {
    let knows = |s: &Snapshot| -> bool {
        if let Some(p) = &ev.pane_id {
            if s.pane(p).is_some() {
                return true;
            }
        }
        if let Some(t) = &ev.tab_id {
            if s.tab(t).is_some() {
                return true;
            }
        }
        if ev.pane_id.is_none() && ev.tab_id.is_none() {
            if let Some(w) = &ev.workspace_id {
                return s.workspace(w).is_some_and(|w| !w.tabs.is_empty());
            }
        }
        false
    };
    snaps.iter().find(|s| knows(s)).copied().unwrap_or(snaps[0])
}

/// The frozen subtrees this event needs: the workspace it names, copied out of the
/// snapshot before the daemon can overwrite it.
pub fn freeze_for(snap: &Snapshot, ev: &BurstEvent) -> Vec<WorkspaceSnap> {
    let mut out = Vec::new();
    let mut want: Vec<String> = Vec::new();
    if let Some(w) = &ev.workspace_id {
        want.push(w.clone());
    }
    if let Some(p) = &ev.pane_id {
        if let Some(ps) = snap.pane(p) {
            want.push(ps.workspace_id.clone());
        }
    }
    if let Some(t) = &ev.tab_id {
        if let Some(ws) = snap.workspace_of_tab(t) {
            want.push(ws.workspace_id.clone());
        }
    }
    for id in want {
        if out.iter().any(|w: &WorkspaceSnap| w.workspace_id == id) {
            continue;
        }
        if let Some(w) = snap.workspace(&id) {
            out.push(w.clone());
        }
    }
    out
}

/// Add an event to the global burst. Returns the burst that must be finalized *now*
/// (because the window had already elapsed), plus the updated `Pending`.
/// Pure — the caller owns the lock and the I/O.
pub fn push_event(
    mut pending: Pending,
    ev: BurstEvent,
    token: &str,
    frozen: Vec<WorkspaceSnap>,
) -> (Pending, Option<Burst>) {
    let t0 = ev.t0;
    let mut expired: Option<Burst> = None;
    if let Some(b) = pending.burst.take() {
        if t0.saturating_sub(b.last_ms) > BURST_WINDOW_MS {
            expired = Some(b);
        } else if t0.saturating_sub(b.first_ms) > BURST_STALE_MS {
            lwarn!("dropping crashed-hook burst from {}", b.first_ms);
        } else {
            pending.burst = Some(b);
        }
    }
    let mut burst = pending.burst.take().unwrap_or(Burst {
        first_ms: t0,
        last_ms: t0,
        last_token: String::new(),
        events: Vec::new(),
        frozen: Vec::new(),
    });
    burst.events.push(ev);
    burst.last_ms = burst.last_ms.max(t0); // MONOTONIC — never move back
    burst.last_token = token.to_string();
    if burst.first_ms == 0 {
        burst.first_ms = t0;
    }
    for w in frozen {
        if !burst
            .frozen
            .iter()
            .any(|x| x.workspace_id == w.workspace_id)
        {
            burst.frozen.push(w);
        }
    }
    pending.burst = Some(burst);
    (pending, expired)
}

/// True when this hook owns finalization of the burst it wrote to.
pub fn owns_finalization(pending: &Pending, token: &str) -> bool {
    matches!(&pending.burst, Some(b) if b.last_token == token)
}

// ---------------------------------------------------------------- inference

fn find_pane<'a>(
    frozen: &'a [WorkspaceSnap],
    pane_id: &str,
) -> Option<(&'a WorkspaceSnap, &'a TabSnap, &'a PaneSnap)> {
    for w in frozen {
        for t in &w.tabs {
            if let Some(p) = t.panes.iter().find(|p| p.pane_id == pane_id) {
                return Some((w, t, p));
            }
        }
    }
    None
}

fn find_tab<'a>(
    frozen: &'a [WorkspaceSnap],
    tab_id: &str,
) -> Option<(&'a WorkspaceSnap, &'a TabSnap)> {
    for w in frozen {
        if let Some(t) = w.tabs.iter().find(|t| t.tab_id == tab_id) {
            return Some((w, t));
        }
    }
    None
}

fn ws_shell(w: &WorkspaceSnap, tabs: Vec<TabSnap>) -> WorkspaceSnap {
    WorkspaceSnap {
        workspace_id: w.workspace_id.clone(),
        label: w.label.clone(),
        index: w.index,
        number: w.number,
        cwd: w.cwd.clone(),
        active_tab_id: w.active_tab_id.clone(),
        tabs,
    }
}

fn summarize(g: Granularity, ws: &WorkspaceSnap) -> String {
    let panes: Vec<&PaneSnap> = ws.tabs.iter().flat_map(|t| t.panes.iter()).collect();
    let name = match g {
        Granularity::Pane => panes
            .first()
            .and_then(|p| p.label.clone())
            .or_else(|| panes.first().map(|p| base(&p.cwd)))
            .unwrap_or_else(|| "pane".into()),
        Granularity::Tab => ws
            .tabs
            .first()
            .and_then(|t| t.label.clone())
            .or_else(|| {
                ws.tabs
                    .first()
                    .and_then(|t| t.panes.first().map(|p| base(&p.cwd)))
            })
            .unwrap_or_else(|| "tab".into()),
        _ => ws
            .label
            .clone()
            .or_else(|| ws.first_cwd().map(|c| base(&c)))
            .unwrap_or_else(|| "workspace".into()),
    };
    let agents: Vec<String> = panes.iter().filter_map(|p| p.agent.clone()).collect();
    let mut parts = vec![g.as_str().to_string(), name];
    if !agents.is_empty() {
        parts.push(agents.join("+"));
    }
    parts.push(format!("{} pane(s)", panes.len().max(1)));
    parts.join(" · ")
}

fn base(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string())
}

/// Which workspace does this event belong to? The event carries it directly for every
/// close herdr emits; the frozen subtrees are the fallback for a pane/tab-only payload.
fn workspace_of(frozen: &[WorkspaceSnap], e: &BurstEvent) -> String {
    if let Some(w) = &e.workspace_id {
        return w.clone();
    }
    if let Some(p) = &e.pane_id {
        if let Some((w, _, _)) = find_pane(frozen, p) {
            return w.workspace_id.clone();
        }
    }
    if let Some(t) = &e.tab_id {
        if let Some((w, _)) = find_tab(frozen, t) {
            return w.workspace_id.clone();
        }
    }
    String::new()
}

/// Turn one finalized burst into undo-stack entries.
///
/// The burst is a GLOBAL 250 ms time window, not one gesture: two independent gestures
/// that land inside it share a burst. herdr's "coarsest level wins" precedence
/// (workspace > tab > pane) is only correct WITHIN one workspace, so the burst is
/// partitioned by workspace first — otherwise closing workspace A and, 100 ms later, a
/// pane in workspace B silently drops B's entry (F4).
pub fn infer_entries(
    burst: &Burst,
    capture_exited: bool,
    mut next_id: u64,
    protocol: u32,
) -> Vec<ClosedEntry> {
    let mut out: Vec<ClosedEntry> = Vec::new();
    let names: Vec<String> = burst.events.iter().map(|e| e.name.clone()).collect();
    let closed_at = burst
        .events
        .iter()
        .map(|e| e.t0)
        .min()
        .unwrap_or(burst.first_ms);

    // --- workspace.closed: still coalesced across the whole burst, because closing N
    //     workspaces at once is one gesture that must undo as one entry ---
    let ws_events: Vec<&BurstEvent> = burst
        .events
        .iter()
        .filter(|e| e.name == "workspace.closed")
        .collect();
    let mut closed_ws: Vec<String> = Vec::new();
    if !ws_events.is_empty() {
        let mut snaps: Vec<WorkspaceSnap> = Vec::new();
        for e in &ws_events {
            let id = e.workspace_id.clone().unwrap_or_default();
            closed_ws.push(id.clone());
            let mut w = burst
                .frozen
                .iter()
                .find(|w| w.workspace_id == id)
                .cloned()
                .unwrap_or(WorkspaceSnap {
                    workspace_id: id.clone(),
                    label: None,
                    index: 0,
                    number: 0,
                    cwd: e.ctx_cwd.clone(),
                    active_tab_id: None,
                    tabs: Vec::new(),
                });
            if w.label.is_none() {
                w.label = e.workspace_label.clone();
            }
            if w.cwd.is_none() {
                w.cwd = w.first_cwd().or_else(|| e.ctx_cwd.clone());
            }
            if !snaps.iter().any(|x| x.workspace_id == w.workspace_id) {
                snaps.push(w);
            }
        }
        if !snaps.is_empty() {
            let g = if snaps.len() == 1 {
                Granularity::Workspace
            } else {
                Granularity::WorkspaceGroup
            };
            let first = snaps.remove(0);
            let summary = if g == Granularity::WorkspaceGroup {
                format!("{} · {} workspaces", g.as_str(), snaps.len() + 1)
            } else {
                summarize(g, &first)
            };
            out.push(ClosedEntry {
                id: next_id,
                closed_at_ms: closed_at,
                granularity: g,
                reason: CloseReason::Closed,
                events: names,
                workspace: first,
                group: snaps,
                summary,
                protocol,
            });
            next_id += 1;
        }
    }

    // --- everything else, one partition per workspace. Events inside a workspace that
    //     was closed whole are already covered by the entry above. ---
    let mut order: Vec<String> = Vec::new();
    let mut groups: Vec<(String, Vec<&BurstEvent>)> = Vec::new();
    for e in burst.events.iter() {
        if e.name == "workspace.closed" {
            continue;
        }
        let ws = workspace_of(&burst.frozen, e);
        if closed_ws.contains(&ws) && !ws.is_empty() {
            continue;
        }
        match order.iter().position(|x| x == &ws) {
            Some(i) => groups[i].1.push(e),
            None => {
                order.push(ws.clone());
                groups.push((ws, vec![e]));
            }
        }
    }
    for (_, events) in groups {
        let entries =
            infer_one_workspace(&events, &burst.frozen, capture_exited, next_id, protocol);
        next_id += entries.len() as u64;
        out.extend(entries);
    }
    out
}

/// Precedence WITHIN one workspace: an explicit `tab.closed` wins over the pane closes
/// herdr does not emit for it; otherwise every pane close is its own entry.
fn infer_one_workspace(
    events: &[&BurstEvent],
    frozen: &[WorkspaceSnap],
    capture_exited: bool,
    mut next_id: u64,
    protocol: u32,
) -> Vec<ClosedEntry> {
    let mut out: Vec<ClosedEntry> = Vec::new();

    let tab_events: Vec<&&BurstEvent> = events.iter().filter(|e| e.name == "tab.closed").collect();
    if !tab_events.is_empty() {
        for e in tab_events {
            let Some(tab_id) = e.tab_id.clone() else {
                lwarn!("tab.closed without tab_id; skipping");
                continue;
            };
            let Some((w, t)) = find_tab(frozen, &tab_id) else {
                lwarn!("tab {tab_id} not in shadow cache; cannot offer undo");
                continue;
            };
            let ws = ws_shell(w, vec![t.clone()]);
            out.push(ClosedEntry {
                id: next_id,
                closed_at_ms: e.t0,
                granularity: Granularity::Tab,
                reason: CloseReason::Closed,
                events: vec![e.name.clone()],
                summary: summarize(Granularity::Tab, &ws),
                workspace: ws,
                group: Vec::new(),
                protocol,
            });
            next_id += 1;
        }
        return out;
    }

    for e in events.iter() {
        if e.name != "pane.closed" && e.name != "pane.exited" {
            continue;
        }
        if e.name == "pane.exited" && !capture_exited {
            continue;
        }
        let reason = if e.name == "pane.exited" {
            CloseReason::Exited
        } else {
            CloseReason::Closed
        };
        let Some(pane_id) = e.pane_id.clone() else {
            continue;
        };
        let (ws, granularity) = match find_pane(frozen, &pane_id) {
            Some((w, t, p)) => {
                if t.panes.len() == 1 {
                    // implicit tab collapse — the tab dies silently (see docs/HERDR_API_NOTES.md)
                    (ws_shell(w, vec![t.clone()]), Granularity::Tab)
                } else {
                    let mut tab = t.clone();
                    tab.panes = vec![p.clone()]; // layout kept whole for sibling lookup
                    (ws_shell(w, vec![tab]), Granularity::Pane)
                }
            }
            None => {
                // minimal hydration from the event context
                let cwd = e.ctx_cwd.clone().unwrap_or_else(|| "~".to_string());
                let tab_id = e.tab_id.clone().unwrap_or_default();
                let pane = PaneSnap {
                    pane_id: pane_id.clone(),
                    tab_id: tab_id.clone(),
                    workspace_id: e.workspace_id.clone().unwrap_or_default(),
                    cwd: cwd.clone(),
                    updated_at_ms: e.t0,
                    ..Default::default()
                };
                let tab = TabSnap {
                    tab_id,
                    workspace_id: e.workspace_id.clone().unwrap_or_default(),
                    label: None,
                    index: 0,
                    number: 0,
                    layout: LayoutNode::pane(Some(pane_id.clone()), Some(cwd.clone()), None),
                    focused_pane_id: None,
                    panes: vec![pane],
                };
                (
                    WorkspaceSnap {
                        workspace_id: e.workspace_id.clone().unwrap_or_default(),
                        label: e.workspace_label.clone(),
                        index: 0,
                        number: 0,
                        cwd: Some(cwd),
                        active_tab_id: None,
                        tabs: vec![tab],
                    },
                    Granularity::Pane,
                )
            }
        };
        out.push(ClosedEntry {
            id: next_id,
            closed_at_ms: e.t0,
            granularity,
            reason,
            events: vec![e.name.clone()],
            summary: summarize(granularity, &ws),
            workspace: ws,
            group: Vec::new(),
            protocol,
        });
        next_id += 1;
    }
    out
}

/// Ids named by this event that make it a self-restore artefact if recently created.
pub fn event_ids(ev: &BurstEvent) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(x) = &ev.pane_id {
        v.push(x.clone());
    }
    if let Some(x) = &ev.tab_id {
        v.push(x.clone());
    }
    if ev.name == "workspace.closed" {
        if let Some(x) = &ev.workspace_id {
            v.push(x.clone());
        }
    }
    v
}

// ---------------------------------------------------------------- live hook path

use crate::config::Config;
use crate::rpc::{Client, Rpc};
use crate::store::{self, Store};

/// The `pane.closed` / `tab.closed` / `workspace.closed` / `pane.exited` hook.
pub fn on_close(
    store: &Store,
    client: &Client,
    cfg: &Config,
    name: &str,
    event_json: &str,
    ctx_json: Option<&str>,
    t0: u64,
) {
    let Some(ev) = parse_event(name, event_json, ctx_json, t0) else {
        lwarn!("{name}: unparseable HERDR_PLUGIN_EVENT_JSON; ignoring");
        return;
    };
    if name == "pane.closed" && ev.pane_id.is_none() {
        return; // popup: no pane id
    }
    let ids = event_ids(&ev);
    if ids.iter().any(|id| store.is_self_created(id, t0)) {
        linfo!("{name}: id created by a restore <5 s ago; ignoring");
        return;
    }
    // NOTE: `capture_exited` is deliberately NOT checked here. `infer_entries` owns that
    // rule (and is what the tests exercise); checking it twice made the two copies able
    // to disagree.

    let token = make_token(t0, std::process::id(), nonce());
    {
        let _g = store.lock();
        let snap = store.snapshot();
        let prev = store.prev_snapshot();
        let candidates = [&snap, &prev];
        let chosen = best_snapshot(&candidates, &ev);
        let frozen = freeze_for(chosen, &ev);
        let (pending, expired) = push_event(store.pending(), ev, &token, frozen);
        if let Some(b) = expired {
            finalize_locked(store, client, cfg, &b);
        }
        let _ = store.write_pending(&pending);
    }

    std::thread::sleep(std::time::Duration::from_millis(BURST_WINDOW_MS));

    {
        let _g = store.lock();
        let mut pending = store.pending();
        if owns_finalization(&pending, &token) {
            if let Some(b) = pending.burst.take() {
                finalize_locked(store, client, cfg, &b);
                let _ = store.write_pending(&pending);
            }
        }
    }

    crate::snapshot::refresh(client, store, cfg.process_info_ttl_ms);
    // Closes are events too: a session where nothing but closes happen must still keep
    // the daemon armed (F20.3).
    crate::daemon::ensure_running(store);
}

/// Caller must hold the lock.
pub fn finalize_locked(store: &Store, client: &Client, cfg: &Config, burst: &Burst) {
    let mut stack = store.closed();
    store::prune_entries(&mut stack, now_ms(), cfg.entry_ttl_hours, crate::STACK_CAP);
    let next = store::next_entry_id(&stack);
    let entries = infer_entries(burst, cfg.capture_exited, next, crate::EXPECTED_PROTOCOL);
    if entries.is_empty() {
        return;
    }
    for e in entries.iter().rev() {
        linfo!(
            "captured #{} {} ({})",
            e.id,
            e.summary,
            e.granularity.as_str()
        );
        stack.entries.insert(0, e.clone());
    }
    stack.entries.truncate(crate::STACK_CAP);
    stack.next_id = entries.iter().map(|e| e.id).max().unwrap_or(next) + 1;
    if let Err(e) = store.write_closed(&stack) {
        lwarn!("cannot write closed.json: {e}");
    }
    if cfg.notify_on_close {
        if let Some(first) = entries.first() {
            let _ = client.ok(
                "notification.show",
                serde_json::json!({"title": format!("Closed: {}", first.summary),
                                   "body": "prefix+u to reopen"}),
            );
        }
    }
}
