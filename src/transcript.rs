//! Does the agent session we are about to resume still have a transcript on disk?
//!
//! `claude --resume <id>` against an id Claude Code never persisted prints
//! `No conversation found with session ID: <id>` and **exits to the shell**, so the
//! restored pane comes back empty. That is exactly what happens to a session with zero
//! turns: Claude Code only writes `~/.claude/projects/<encoded-cwd>/<id>.jsonl` once the
//! conversation has something in it (F21).
//!
//! The encoding was verified empirically against a real `~/.claude/projects` on macOS:
//! **every non-alphanumeric byte of the absolute cwd becomes `-`** (so the leading `/`
//! produces a leading `-`, `.` in `github.com` and `/.claude/` become `-` as well).
//! 28 of 28 directories that carry a transcript with a `cwd` field matched that rule
//! exactly; the six that did not were git worktrees, whose transcripts record the *main*
//! worktree's cwd while the directory is named after the worktree path — which is
//! precisely why the check also globs every project directory for the id.
//!
//! Pure except for `std::fs` reads: `projects_dir` is injected, so tests use a tempdir.

use std::path::{Path, PathBuf};

/// Outcome of the pre-resume transcript lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transcript {
    /// A transcript file exists; `--resume` will work.
    Found(PathBuf),
    /// The store is readable and the id is definitely not in it; `--resume` would fail.
    Missing,
    /// We cannot tell (no store on this machine, an unusable id, an unreadable dir).
    /// Never a reason to change behaviour — resume as before.
    Unknown,
}

impl Transcript {
    pub fn is_missing(&self) -> bool {
        matches!(self, Transcript::Missing)
    }
}

/// `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects` when that is set.
pub fn default_projects_dir() -> Option<PathBuf> {
    let base = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".claude"))
        })?;
    Some(base.join("projects"))
}

/// Claude Code's project-directory name for a working directory: every non-alphanumeric
/// character replaced by `-`, nothing else (no trimming, no collapsing of runs).
pub fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A session id we are willing to turn into a path component. Claude Code ids are
/// UUIDs; anything with a separator or a dot is refused rather than sanitized, because
/// guessing wrong here would silently drop a resumable session.
fn usable_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Is there a Claude Code transcript for `session_id`?
///
/// `cwd` is the closed pane's working directory: it gives the fast path. The scan over
/// every project directory is the fallback, because a session started in one directory
/// can be recorded under another (git worktrees, `cd` before the first turn).
pub fn claude_transcript(projects_dir: &Path, cwd: &str, session_id: &str) -> Transcript {
    if !usable_id(session_id) {
        return Transcript::Unknown;
    }
    if !projects_dir.is_dir() {
        return Transcript::Unknown;
    }
    let file = format!("{session_id}.jsonl");
    if !cwd.is_empty() {
        let direct = projects_dir.join(encode_cwd(cwd)).join(&file);
        if direct.is_file() {
            return Transcript::Found(direct);
        }
    }
    let Ok(entries) = std::fs::read_dir(projects_dir) else {
        return Transcript::Unknown;
    };
    for e in entries.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let candidate = e.path().join(&file);
        if candidate.is_file() {
            return Transcript::Found(candidate);
        }
    }
    Transcript::Missing
}

/// The same check for whichever agent kinds we know how to verify. Every other kind is
/// `Unknown`: there is no second transcript layout we have confirmed, and a wrong guess
/// would drop a perfectly good resume.
pub fn check(projects_dir: Option<&Path>, kind: &str, cwd: &str, session_id: &str) -> Transcript {
    if kind != "claude" {
        return Transcript::Unknown;
    }
    match projects_dir {
        Some(d) => claude_transcript(d, cwd, session_id),
        None => Transcript::Unknown,
    }
}
