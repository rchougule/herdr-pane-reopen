//! Shadow cache: assemble a `Snapshot` from herdr's raw list/export payloads, pick the
//! foreground command of a non-agent pane, and merge create-event payloads for free.
//!
//! `assemble` and `pick_foreground` are pure and unit-tested against real fixtures.

use crate::model::*;
use crate::rpc::Rpc;
use crate::store::Store;
use crate::{linfo, lwarn, now_ms};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

// ---------------------------------------------------------------- raw payloads

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RawAgentSession {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RawPane {
    pub pane_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_session: Option<RawAgentSession>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RawTab {
    pub tab_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RawWorkspace {
    pub workspace_id: String,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub active_tab_id: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RawLayout {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    pub root: LayoutNode,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RawProcess {
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub argv0: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RawProcessInfo {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub shell_pid: i64,
    #[serde(default)]
    pub foreground_processes: Vec<RawProcess>,
}

// ---------------------------------------------------------------- pure assembly

fn pane_snap(p: &RawPane, now: u64) -> PaneSnap {
    let (kind, value) = match &p.agent_session {
        Some(s) => (s.kind.clone(), s.value.clone()),
        None => (None, None),
    };
    PaneSnap {
        pane_id: p.pane_id.clone(),
        tab_id: p.tab_id.clone(),
        workspace_id: p.workspace_id.clone(),
        cwd: p
            .foreground_cwd
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| p.cwd.clone()),
        label: p.label.clone(),
        agent: p.agent.clone(),
        agent_session: value,
        agent_session_kind: kind,
        agent_status: p.agent_status.clone(),
        terminal_title: p.terminal_title.clone(),
        foreground: None,
        updated_at_ms: now,
    }
}

/// Build a `Snapshot`. Workspace/tab order is the ARRAY order of the list responses —
/// `number` is a creation ordinal, never a position.
pub fn assemble(
    workspaces: &[RawWorkspace],
    tabs: &[RawTab],
    panes: &[RawPane],
    layouts: &BTreeMap<String, RawLayout>,
    now: u64,
) -> Snapshot {
    let mut out = Snapshot {
        taken_at_ms: now,
        workspaces: Vec::new(),
    };
    for (wi, w) in workspaces.iter().enumerate() {
        let mut ws = WorkspaceSnap {
            workspace_id: w.workspace_id.clone(),
            label: w.label.clone(),
            index: wi,
            number: w.number,
            cwd: None,
            active_tab_id: w.active_tab_id.clone(),
            tabs: Vec::new(),
        };
        for (ti, t) in tabs
            .iter()
            .filter(|t| t.workspace_id == w.workspace_id)
            .enumerate()
        {
            let layout = layouts.get(&t.tab_id);
            let root = layout.map(|l| l.root.clone()).unwrap_or_else(|| {
                // No export available: fall back to a flat single-leaf tree.
                let first = panes.iter().find(|p| p.tab_id == t.tab_id);
                LayoutNode::pane(
                    first.map(|p| p.pane_id.clone()),
                    first.map(|p| p.cwd.clone()),
                    None,
                )
            });
            // leaf order defines pane order
            let leaf_ids = root.leaf_pane_ids();
            let mut tab_panes: Vec<PaneSnap> = Vec::new();
            for id in &leaf_ids {
                if let Some(p) = panes.iter().find(|p| &p.pane_id == id) {
                    tab_panes.push(pane_snap(p, now));
                }
            }
            for p in panes.iter().filter(|p| p.tab_id == t.tab_id) {
                if !tab_panes.iter().any(|q| q.pane_id == p.pane_id) {
                    tab_panes.push(pane_snap(p, now));
                }
            }
            ws.tabs.push(TabSnap {
                tab_id: t.tab_id.clone(),
                workspace_id: t.workspace_id.clone(),
                label: t.label.clone(),
                index: ti,
                number: t.number,
                layout: root,
                focused_pane_id: layout.and_then(|l| l.focused_pane_id.clone()),
                panes: tab_panes,
            });
        }
        ws.cwd = ws.first_cwd();
        out.workspaces.push(ws);
    }
    out
}

const SHELLS: &[&str] = &[
    "zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu",
];
/// Command wrappers, plus the transient children a shell spawns for its own prompt and
/// startup — observed live: `starship prompt`, `path_helper`, `locale`. Capturing one of
/// those as "what the pane was running" would prefill nonsense.
const WRAPPERS: &[&str] = &[
    "caffeinate",
    "script",
    "nohup",
    "time",
    "env",
    "stdbuf",
    "direnv",
    "ssh-agent",
    "starship",
    "path_helper",
    "locale",
    "gitstatusd",
    "powerline",
    "powerline-daemon",
    "compinit",
    "security",
];

/// Short-lived file/text utilities. A shell startup (oh-my-zsh, compinit, nvm, asdf…)
/// spawns a burst of these, and a `pane.process_info` that lands inside that burst
/// captures one of them as "the pane's command" — observed live as
/// `mkdir -p ~/.oh-my-zsh/cache/completions` for a pane that was running `tail -f`.
///
/// None of these is ever something a user would want re-run on reopen, and none of them
/// is a long-running foreground job, so skipping them is safe. Deliberately NOT here:
/// `tail`, `watch`, `less`, `find`, `grep`, `sed`, `awk`, `sort` — all of which a user
/// can legitimately sit in front of.
const TRANSIENT: &[&str] = &[
    "mkdir",
    "rmdir",
    "rm",
    "cp",
    "mv",
    "ln",
    "touch",
    "chmod",
    "chown",
    "cat",
    "echo",
    "printf",
    "test",
    "[",
    "true",
    "false",
    "pwd",
    "basename",
    "dirname",
    "uname",
    "id",
    "whoami",
    "date",
    "tty",
    "stty",
    "tput",
    "expr",
    "sleep",
    "which",
    "command",
    "type",
    "hash",
    "unlink",
    "mktemp",
    "readlink",
    "realpath",
    "dircolors",
    "getopt",
];

fn base_name(s: &str) -> &str {
    s.trim_start_matches('-')
        .rsplit('/')
        .next()
        .unwrap_or(s)
        .trim_start_matches('-')
}

/// True for a cwd that only a shell/tool's own bookkeeping runs in — a dotted directory
/// whose path names a cache. A user's foreground job effectively never lives there, and
/// the observed false capture (`~/.oh-my-zsh/cache/completions`) did.
pub fn is_housekeeping_cwd(cwd: &str) -> bool {
    let mut dotted = false;
    let mut cachey = false;
    for part in cwd.split('/') {
        if part.len() > 1 && part.starts_with('.') {
            dotted = true;
        }
        let lower = part.to_ascii_lowercase();
        if lower.contains("cache") || lower == "completions" {
            cachey = true;
        }
    }
    dotted && cachey
}

fn argv0_of(p: &RawProcess) -> &str {
    let a0 = if p.argv0.is_empty() {
        p.argv.first().map(|s| s.as_str()).unwrap_or("")
    } else {
        p.argv0.as_str()
    };
    base_name(a0)
}

/// Is this process a plausible "what the pane was running"?
fn is_candidate(p: &RawProcess, shell_pid: i64) -> bool {
    if p.pid == shell_pid {
        return false;
    }
    let a0 = argv0_of(p);
    if a0.is_empty() || SHELLS.contains(&a0) || WRAPPERS.contains(&a0) || TRANSIENT.contains(&a0) {
        return false;
    }
    !is_housekeeping_cwd(&p.cwd)
}

/// Pick the command a non-agent pane is running. Match on `argv0`/`argv[0]` — never on
/// `name`, which is the process *title* (claude rewrites it; see docs/HERDR_API_NOTES.md).
/// The oldest remaining process (smallest pid) is the shell's direct child.
pub fn pick_foreground(info: &RawProcessInfo, now: u64) -> Option<ForegroundCmd> {
    let mut candidates: Vec<&RawProcess> = info
        .foreground_processes
        .iter()
        .filter(|p| is_candidate(p, info.shell_pid))
        .collect();
    candidates.sort_by_key(|p| p.pid);
    let p = candidates.first()?;
    let argv = if p.argv.is_empty() {
        vec![p.argv0.clone()]
    } else {
        p.argv.clone()
    };
    Some(ForegroundCmd {
        argv,
        cwd: p.cwd.clone(),
        pid: p.pid,
        captured_at_ms: now,
    })
}

/// `pane.process_info` carries no process start time, so "is this child long-lived?"
/// can only be answered by sampling twice. True when the command picked from the first
/// sample is still running, as the same pid, in the second (F-A).
pub fn confirms(second: &RawProcessInfo, cmd: &ForegroundCmd) -> bool {
    second
        .foreground_processes
        .iter()
        .any(|p| p.pid == cmd.pid && Some(argv0_of(p)) == cmd.argv.first().map(|a| base_name(a)))
}

/// Shell-quote an argv for `pane.send_text` prefill.
pub fn shell_quote(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,%".contains(c))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- event seeding

/// Merge a `pane.created` payload straight into the snapshot — zero socket calls
///.
pub fn merge_pane_created(snap: &mut Snapshot, p: &RawPane, now: u64) {
    let ps = pane_snap(p, now);
    let ws = match snap
        .workspaces
        .iter_mut()
        .find(|w| w.workspace_id == p.workspace_id)
    {
        Some(w) => w,
        None => {
            snap.workspaces.push(WorkspaceSnap {
                workspace_id: p.workspace_id.clone(),
                label: None,
                index: snap.workspaces.len(),
                number: 0,
                cwd: Some(ps.cwd.clone()),
                active_tab_id: Some(p.tab_id.clone()),
                tabs: Vec::new(),
            });
            snap.workspaces.last_mut().unwrap()
        }
    };
    let idx = ws.tabs.len();
    let tab = match ws.tabs.iter_mut().find(|t| t.tab_id == p.tab_id) {
        Some(t) => t,
        None => {
            ws.tabs.push(TabSnap {
                tab_id: p.tab_id.clone(),
                workspace_id: p.workspace_id.clone(),
                label: None,
                index: idx,
                number: 0,
                layout: LayoutNode::pane(Some(ps.pane_id.clone()), Some(ps.cwd.clone()), None),
                focused_pane_id: None,
                panes: Vec::new(),
            });
            ws.tabs.last_mut().unwrap()
        }
    };
    if let Some(existing) = tab.panes.iter_mut().find(|q| q.pane_id == ps.pane_id) {
        *existing = ps;
        return;
    }
    if !tab.layout.leaf_pane_ids().contains(&ps.pane_id) {
        // Approximate geometry until the next full refresh (~150 ms away).
        let old = tab.layout.clone();
        tab.layout = LayoutNode::Split {
            direction: SplitDir::Right,
            ratio: 0.5,
            first: Box::new(old),
            second: Box::new(LayoutNode::pane(
                Some(ps.pane_id.clone()),
                Some(ps.cwd.clone()),
                ps.label.clone(),
            )),
        };
    }
    tab.panes.push(ps);
}

pub fn merge_tab_created(snap: &mut Snapshot, t: &RawTab) {
    let Some(ws) = snap
        .workspaces
        .iter_mut()
        .find(|w| w.workspace_id == t.workspace_id)
    else {
        return;
    };
    if ws.tabs.iter().any(|x| x.tab_id == t.tab_id) {
        return;
    }
    let index = ws.tabs.len();
    ws.tabs.push(TabSnap {
        tab_id: t.tab_id.clone(),
        workspace_id: t.workspace_id.clone(),
        label: t.label.clone(),
        index,
        number: t.number,
        layout: LayoutNode::pane(None, None, None),
        focused_pane_id: None,
        panes: Vec::new(),
    });
}

pub fn merge_workspace_created(snap: &mut Snapshot, w: &RawWorkspace) {
    if snap
        .workspaces
        .iter()
        .any(|x| x.workspace_id == w.workspace_id)
    {
        return;
    }
    let index = snap.workspaces.len();
    snap.workspaces.push(WorkspaceSnap {
        workspace_id: w.workspace_id.clone(),
        label: w.label.clone(),
        index,
        number: w.number,
        cwd: None,
        active_tab_id: w.active_tab_id.clone(),
        tabs: Vec::new(),
    });
}

// ---------------------------------------------------------------- live refresh

fn list<T: serde::de::DeserializeOwned>(c: &dyn Rpc, method: &str, key: &str) -> Vec<T> {
    match c.call(method, json!({})) {
        Ok(v) => match v.get(key) {
            Some(arr) => serde_json::from_value(arr.clone()).unwrap_or_else(|e| {
                lwarn!("{method}: {e}");
                Vec::new()
            }),
            None => Vec::new(),
        },
        Err(e) => {
            lwarn!("{method} failed: {e}");
            Vec::new()
        }
    }
}

/// Wall-clock budget for one refresh. Nothing else caps a hook's runtime, and a herdr
/// that accepts connections but answers slowly would otherwise block it for
/// `per-call timeout × number of panes` (F7).
pub const REFRESH_BUDGET_MS: u64 = 5_000;
/// Gap between the two `pane.process_info` samples used to confirm a newly appeared
/// foreground command.
pub const CONFIRM_DELAY_MS: u64 = 120;
/// At most this many panes are re-sampled per refresh, so the confirmation pass can
/// never turn into a second full sweep.
pub const CONFIRM_MAX_PANES: usize = 4;

fn process_info_of(c: &dyn Rpc, pane_id: &str) -> Option<RawProcessInfo> {
    match c.call("pane.process_info", json!({ "pane_id": pane_id })) {
        Ok(v) => Some(
            v.get("process_info")
                .cloned()
                .and_then(|p| serde_json::from_value(p).ok())
                .unwrap_or_default(),
        ),
        Err(_) => None,
    }
}

/// Full refresh: 3 list calls + one `layout.export` per tab + `process_info` for
/// non-agent panes past their TTL. Measured ~3 ms on a 26-pane session.
pub fn refresh(client: &dyn Rpc, store: &Store, process_info_ttl_ms: u64) -> Snapshot {
    let started = std::time::Instant::now();
    let over_budget =
        move || started.elapsed() > std::time::Duration::from_millis(REFRESH_BUDGET_MS);
    let now = now_ms();
    let workspaces: Vec<RawWorkspace> = list(client, "workspace.list", "workspaces");
    let tabs: Vec<RawTab> = list(client, "tab.list", "tabs");
    let panes: Vec<RawPane> = list(client, "pane.list", "panes");

    let mut layouts: BTreeMap<String, RawLayout> = BTreeMap::new();
    for t in &tabs {
        if over_budget() {
            lwarn!("refresh budget exhausted before layout.export {}", t.tab_id);
            break;
        }
        match client.call("layout.export", json!({ "tab_id": t.tab_id })) {
            Ok(v) => match v.get("layout").cloned().map(serde_json::from_value) {
                Some(Ok(l)) => {
                    layouts.insert(t.tab_id.clone(), l);
                }
                Some(Err(e)) => lwarn!("layout.export {}: {e}", t.tab_id),
                None => {}
            },
            Err(e) => lwarn!("layout.export {} failed: {e}", t.tab_id),
        }
    }

    let mut snap = assemble(&workspaces, &tabs, &panes, &layouts, now);
    // Unlocked read: only a hint for the process_info TTL. The `lost_containers`
    // decision below re-reads it under the lock (F5).
    let hint = store.snapshot();

    // ---- pass 1: sample every non-agent pane whose cached command aged out ----
    let mut confirm: Vec<(String, ForegroundCmd)> = Vec::new();
    for ws in snap.workspaces.iter_mut() {
        for tab in ws.tabs.iter_mut() {
            for pane in tab.panes.iter_mut() {
                if pane.agent.is_some() {
                    continue;
                }
                let prev_fg = hint.pane(&pane.pane_id).and_then(|p| p.foreground.clone());
                let fresh = prev_fg
                    .as_ref()
                    .map(|f| now.saturating_sub(f.captured_at_ms) < process_info_ttl_ms)
                    .unwrap_or(false);
                if fresh || over_budget() {
                    pane.foreground = prev_fg;
                    continue;
                }
                match process_info_of(client, &pane.pane_id) {
                    // A successful sample is authoritative: an idle pane has NO
                    // foreground command, and keeping the last one seen would make a
                    // stale capture permanently sticky.
                    Some(info) => {
                        let picked = pick_foreground(&info, now);
                        let is_new = match (&picked, &prev_fg) {
                            (Some(p), Some(q)) => p.argv != q.argv,
                            (Some(_), None) => true,
                            _ => false,
                        };
                        if is_new {
                            if let Some(c) = &picked {
                                confirm.push((pane.pane_id.clone(), c.clone()));
                            }
                        }
                        pane.foreground = picked;
                    }
                    // The call itself failed: keep what we had rather than forgetting.
                    None => pane.foreground = prev_fg,
                }
            }
        }
    }

    // ---- pass 2: a command we have not seen before must survive a second sample ----
    if !confirm.is_empty() && !over_budget() {
        confirm.truncate(CONFIRM_MAX_PANES);
        std::thread::sleep(std::time::Duration::from_millis(CONFIRM_DELAY_MS));
        for (pane_id, cmd) in &confirm {
            if over_budget() {
                break;
            }
            let Some(second) = process_info_of(client, pane_id) else {
                continue; // pane gone or call failed: keep the first sample
            };
            if confirms(&second, cmd) {
                continue;
            }
            linfo!(
                "{pane_id}: '{}' did not survive a {}ms re-sample; treating it as a transient \
                 shell child",
                cmd.argv.join(" "),
                CONFIRM_DELAY_MS
            );
            // Whatever is running now, as long as it is not the same transient again.
            let replacement = pick_foreground(&second, now_ms()).filter(|c| c.argv != cmd.argv);
            for ws in snap.workspaces.iter_mut() {
                for tab in ws.tabs.iter_mut() {
                    for pane in tab.panes.iter_mut() {
                        if &pane.pane_id == pane_id {
                            pane.foreground = replacement.clone();
                        }
                    }
                }
            }
        }
    }

    {
        let _g = store.lock();
        // Re-read under the lock: a concurrent refresh (daemon + hook run at the same
        // time routinely) may have written a NEWER snapshot while we were sampling.
        let prev = store.snapshot();
        if prev.taken_at_ms > snap.taken_at_ms {
            lwarn!(
                "a newer snapshot ({} > {}) won; not overwriting it",
                prev.taken_at_ms,
                snap.taken_at_ms
            );
            return prev;
        }
        // A close hook is a separate process and may start AFTER this refresh has already
        // dropped the pane it needs. Keep the last snapshot that still had it.
        if lost_containers(&prev, &snap) {
            if let Err(e) = store.write_prev_snapshot(&prev) {
                lwarn!("cannot write previous snapshot: {e}");
            }
        }
        if let Err(e) = store.write_snapshot(&snap) {
            lwarn!("cannot write snapshot: {e}");
        }
    }
    snap
}

/// True when anything present in `before` is missing from `after`. Pure.
pub fn lost_containers(before: &Snapshot, after: &Snapshot) -> bool {
    fn pane_ids(s: &Snapshot) -> Vec<&str> {
        s.workspaces
            .iter()
            .flat_map(|w| w.tabs.iter())
            .flat_map(|t| t.panes.iter())
            .map(|p| p.pane_id.as_str())
            .collect()
    }
    if before.workspaces.is_empty() {
        return false;
    }
    let after_ids = pane_ids(after);
    pane_ids(before).iter().any(|id| !after_ids.contains(id))
        || before
            .workspaces
            .iter()
            .any(|w| after.workspace(&w.workspace_id).is_none())
}

/// Refresh only if the cached snapshot is older than `ttl_ms`.
pub fn refresh_if_stale(client: &dyn Rpc, store: &Store, ttl_ms: u64, process_info_ttl_ms: u64) {
    let snap = store.snapshot();
    if now_ms().saturating_sub(snap.taken_at_ms) < ttl_ms {
        return;
    }
    refresh(client, store, process_info_ttl_ms);
}
