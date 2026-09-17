//! Agent resume: argument table, config override, alias generation.
//! Pure — no socket.

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Checked against the installed CLI's own `--help`.
    Verified,
    /// Not installed anywhere we could check; best effort.
    Assumed,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Verified => "verified",
            Confidence::Assumed => "assumed",
        }
    }
}

/// Built-in table. There is no machine-readable source: the agent-detection manifests
/// carry no resume metadata, so this is hand-maintained.
pub fn builtin_resume_args(kind: &str, id: &str) -> Option<(Vec<String>, Confidence)> {
    use Confidence::*;
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    Some(match kind {
        // ---- VERIFIED against the installed CLI's --help ----
        "claude" => (v(&["--resume", id]), Verified),
        "codex" => (v(&["resume", id]), Verified),
        "cursor" => (v(&["--resume", id]), Verified),
        "hermes" => (v(&["--resume", id]), Verified),
        "opencode" => (v(&["--session", id]), Verified),
        // ---- ASSUMED: CLI not installed here, never observed ----
        "grok" | "devin" | "droid" | "qodercli" | "qwen" => (v(&["--resume", id]), Assumed),
        "omp" | "copilot" => (vec![format!("--resume={id}")], Assumed),
        _ => return None,
    })
}

/// Config override wins and is treated as `Verified`; `{id}` is substituted verbatim.
pub fn resume_args(cfg: &Config, kind: &str, id: &str) -> Option<(Vec<String>, Confidence)> {
    if let Some(o) = cfg.resume.get(kind) {
        let args = o.args.iter().map(|a| a.replace("{id}", id)).collect();
        return Some((args, Confidence::Verified));
    }
    builtin_resume_args(kind, id)
}

/// herdr: `agent name must start with a lowercase letter and contain only lowercase
/// letters, digits, '-' or '_' (1-32 characters)` → `^[a-z][a-z0-9_-]{0,31}$`.
pub fn is_valid_alias(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 32 {
        return false;
    }
    if !(b[0].is_ascii_lowercase()) {
        return false;
    }
    b.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// Sanitize a label (or cwd basename) into a legal agent alias, truncated to 28 so the
/// `-2`, `-3`… collision ladder still fits inside 32.
pub fn alias(seed: &str) -> String {
    let lower = seed.to_ascii_lowercase();
    let mut out = String::new();
    let mut last_dash = false;
    for ch in lower.chars() {
        let c = if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
            ch
        } else {
            '-'
        };
        if c == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(c);
    }
    let out = out.trim_matches('-').to_string();
    let mut out = if out.is_empty() || !out.as_bytes()[0].is_ascii_lowercase() {
        format!("re-{out}")
    } else {
        out
    };
    out.truncate(28);
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "reopened".to_string()
    } else {
        out
    }
}

/// The `agent_name_taken` retry ladder: base, base-2 … base-5, always ≤ 32 chars.
pub fn alias_candidates(base: &str) -> Vec<String> {
    let mut out = vec![base.to_string()];
    for n in 2..=5 {
        let suffix = format!("-{n}");
        let mut b = base.to_string();
        if b.len() + suffix.len() > 32 {
            b.truncate(32 - suffix.len());
        }
        out.push(format!("{b}{suffix}"));
    }
    out
}

/// Seed for the alias: pane label, else cwd basename.
pub fn alias_seed(label: Option<&str>, cwd: &str) -> String {
    match label {
        Some(l) if !l.trim().is_empty() => l.to_string(),
        _ => std::path::Path::new(cwd)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "reopened".to_string()),
    }
}
