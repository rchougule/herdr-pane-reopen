//! Environment herdr injects into plugin processes (verified against herdr 0.9.1).
//!
//! State is SESSION-SCOPED. `HERDR_PLUGIN_STATE_DIR` is keyed on the plugin id only, so
//! every herdr session (`herdr --session X`) is handed the same directory. Two sessions
//! sharing one `closed.json` / `snapshot.json` / `watch.lock` corrupt each other's undo
//! stack, so `load()` appends a key derived from the canonicalized `HERDR_SOCKET_PATH`
//! (F1).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct HerdrEnv {
    pub socket_path: PathBuf,
    /// `<base>/<session key>` — never the bare base directory.
    pub state_dir: PathBuf,
    pub config_dir: Option<PathBuf>,
    pub plugin_id: String,
    pub bin_path: Option<PathBuf>,
    pub event: Option<String>,
    pub event_json: Option<String>,
    pub context_json: Option<String>,
}

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// `${XDG_CONFIG_HOME:-$HOME/.config}/herdr/herdr.sock`
pub fn default_socket_path() -> PathBuf {
    let base = var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(var("HOME").unwrap_or_default()).join(".config"));
    base.join("herdr").join("herdr.sock")
}

/// The directory herdr itself would hand us — shared by every session.
pub fn base_state_dir(plugin_id: &str) -> PathBuf {
    PathBuf::from(var("HOME").unwrap_or_default())
        .join(".local/state/herdr/plugins")
        .join(plugin_id)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0193);
    }
    h
}

fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let out = out.trim_matches('-').to_string();
    if out.len() > 24 {
        out[..24].trim_end_matches('-').to_string()
    } else {
        out
    }
}

/// A short, stable, human-recognisable directory name for one herdr session.
///
/// `~/.config/herdr/herdr.sock`                  → `default-<hash>`
/// `~/.config/herdr/sessions/reopen-qa/herdr.sock` → `reopen-qa-<hash>`
///
/// The hash is over the canonicalized path, so two different sessions can never collide
/// even when their directory names match.
pub fn session_key(socket_path: &Path) -> String {
    let canon = std::fs::canonicalize(socket_path).unwrap_or_else(|_| socket_path.to_path_buf());
    let canon_s = canon.to_string_lossy().to_string();
    let parent = canon
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let name = if parent.is_empty() || parent == "herdr" {
        "default".to_string()
    } else {
        sanitize(&parent)
    };
    let name = if name.is_empty() {
        "session".to_string()
    } else {
        name
    };
    format!("{name}-{:08x}", fnv1a64(canon_s.as_bytes()) & 0xffff_ffff)
}

impl HerdrEnv {
    pub fn load() -> Self {
        let plugin_id = var("HERDR_PLUGIN_ID").unwrap_or_else(|| "rchougule.reopen".to_string());
        let socket_path = var("HERDR_SOCKET_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(default_socket_path);
        // herdr hands every session the SAME value, so scope it ourselves.
        let base = var("HERDR_PLUGIN_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base_state_dir(&plugin_id));
        let state_dir = base.join(session_key(&socket_path));
        HerdrEnv {
            socket_path,
            state_dir,
            config_dir: var("HERDR_PLUGIN_CONFIG_DIR").map(PathBuf::from),
            plugin_id,
            bin_path: var("HERDR_BIN_PATH").map(PathBuf::from),
            event: var("HERDR_PLUGIN_EVENT"),
            event_json: var("HERDR_PLUGIN_EVENT_JSON"),
            context_json: var("HERDR_PLUGIN_CONTEXT_JSON"),
        }
    }

    pub fn ensure_state_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.state_dir)
    }
}
