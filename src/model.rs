//! Data model: the shadow snapshot and the undo stack.
//!
//! Every field herdr may omit is `#[serde(default)]` — herdr leaves out `label`,
//! `agent`, `agent_session`, `terminal_title` when unset (see docs/HERDR_API_NOTES.md).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------- layout tree

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SplitDir {
    Right,
    Down,
}

impl SplitDir {
    pub fn as_str(&self) -> &'static str {
        match self {
            SplitDir::Right => "right",
            SplitDir::Down => "down",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LayoutNode {
    Pane {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    Split {
        direction: SplitDir,
        #[serde(default = "half")]
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

fn half() -> f64 {
    0.5
}

impl LayoutNode {
    pub fn pane(pane_id: Option<String>, cwd: Option<String>, label: Option<String>) -> Self {
        LayoutNode::Pane {
            pane_id,
            cwd,
            label,
            command: None,
            env: None,
        }
    }

    /// Depth-first leaf order (first, then second) — the order `layout.apply` fills
    /// `pane_id`s back in, so old→new mapping is by position.
    pub fn leaves(&self) -> Vec<&LayoutNode> {
        let mut out = Vec::new();
        self.walk_leaves(&mut out);
        out
    }

    fn walk_leaves<'a>(&'a self, out: &mut Vec<&'a LayoutNode>) {
        match self {
            LayoutNode::Pane { .. } => out.push(self),
            LayoutNode::Split { first, second, .. } => {
                first.walk_leaves(out);
                second.walk_leaves(out);
            }
        }
    }

    pub fn leaf_pane_ids(&self) -> Vec<String> {
        self.leaves()
            .into_iter()
            .filter_map(|l| match l {
                LayoutNode::Pane { pane_id, .. } => pane_id.clone(),
                _ => None,
            })
            .collect()
    }

    pub fn leaf_cwds(&self) -> Vec<String> {
        self.leaves()
            .into_iter()
            .filter_map(|l| match l {
                LayoutNode::Pane { cwd, .. } => cwd.clone(),
                _ => None,
            })
            .collect()
    }

    pub fn first_leaf_cwd(&self) -> Option<String> {
        self.leaf_cwds().into_iter().next()
    }

    pub fn leaf_count(&self) -> usize {
        self.leaves().len()
    }
}

// ---------------------------------------------------------------- snapshot

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ForegroundCmd {
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    /// The sampled pid, so a second `pane.process_info` can tell "still the same
    /// process" from "a new process with the same name".
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub captured_at_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PaneSnap {
    pub pane_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_session: Option<String>,
    #[serde(default)]
    pub agent_session_kind: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub foreground: Option<ForegroundCmd>,
    #[serde(default)]
    pub updated_at_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TabSnap {
    pub tab_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// 0-based position in this workspace's `tab.list` array — the display order.
    #[serde(default)]
    pub index: usize,
    /// `tab.list[].number`: a stable creation ordinal, NOT a position.
    #[serde(default)]
    pub number: u32,
    pub layout: LayoutNode,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub panes: Vec<PaneSnap>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorkspaceSnap {
    pub workspace_id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// 0-based position in the `workspace.list` array.
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub active_tab_id: Option<String>,
    #[serde(default)]
    pub tabs: Vec<TabSnap>,
}

impl WorkspaceSnap {
    pub fn pane_count(&self) -> usize {
        self.tabs.iter().map(|t| t.panes.len()).sum()
    }
    pub fn first_cwd(&self) -> Option<String> {
        self.cwd.clone().or_else(|| {
            self.tabs.iter().find_map(|t| {
                t.layout
                    .first_leaf_cwd()
                    .or_else(|| t.panes.first().map(|p| p.cwd.clone()))
            })
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    #[serde(default)]
    pub taken_at_ms: u64,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSnap>,
}

impl Snapshot {
    pub fn pane(&self, pane_id: &str) -> Option<&PaneSnap> {
        self.workspaces
            .iter()
            .flat_map(|w| w.tabs.iter())
            .flat_map(|t| t.panes.iter())
            .find(|p| p.pane_id == pane_id)
    }
    pub fn tab(&self, tab_id: &str) -> Option<&TabSnap> {
        self.workspaces
            .iter()
            .flat_map(|w| w.tabs.iter())
            .find(|t| t.tab_id == tab_id)
    }
    pub fn workspace(&self, ws_id: &str) -> Option<&WorkspaceSnap> {
        self.workspaces.iter().find(|w| w.workspace_id == ws_id)
    }
    pub fn workspace_of_tab(&self, tab_id: &str) -> Option<&WorkspaceSnap> {
        self.workspaces
            .iter()
            .find(|w| w.tabs.iter().any(|t| t.tab_id == tab_id))
    }
}

// ---------------------------------------------------------------- undo stack

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Pane,
    Tab,
    Workspace,
    WorkspaceGroup,
}

impl Granularity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Granularity::Pane => "pane",
            Granularity::Tab => "tab",
            Granularity::Workspace => "workspace",
            Granularity::WorkspaceGroup => "workspace-group",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Closed,
    Exited,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClosedEntry {
    pub id: u64,
    pub closed_at_ms: u64,
    pub granularity: Granularity,
    pub reason: CloseReason,
    #[serde(default)]
    pub events: Vec<String>,
    pub workspace: WorkspaceSnap,
    #[serde(default)]
    pub group: Vec<WorkspaceSnap>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub protocol: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ClosedStack {
    #[serde(default)]
    pub entries: Vec<ClosedEntry>,
    /// Monotonic id allocator. Derived ids restart at 1 whenever a TTL sweep empties
    /// the stack, which makes `reopen --id N` ambiguous against a listing the user read
    /// a minute ago (F15).
    #[serde(default)]
    pub next_id: u64,
}

// ---------------------------------------------------------------- pending burst

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BurstEvent {
    pub t0: u64,
    /// dotted event name, e.g. `pane.closed`
    pub name: String,
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// `workspace.closed` carries a final workspace snapshot; keep its label.
    #[serde(default)]
    pub workspace_label: Option<String>,
    #[serde(default)]
    pub ctx_cwd: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Burst {
    pub first_ms: u64,
    pub last_ms: u64,
    pub last_token: String,
    #[serde(default)]
    pub events: Vec<BurstEvent>,
    /// The pre-close subtrees, frozen when the first hook saw them.
    #[serde(default)]
    pub frozen: Vec<WorkspaceSnap>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Pending {
    #[serde(default)]
    pub burst: Option<Burst>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SelfCreated {
    pub id: String,
    pub created_at_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SelfCreatedFile {
    #[serde(default)]
    pub ids: Vec<SelfCreated>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct WatchLock {
    pub pid: i32,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}
