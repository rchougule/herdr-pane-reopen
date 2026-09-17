//! `config.toml` in `$HERDR_PLUGIN_CONFIG_DIR` (`herdr plugin config-dir rchougule.reopen`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RerunMode {
    #[default]
    Prefill,
    Run,
    Off,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rerun {
    #[serde(default)]
    pub mode: RerunMode,
    #[serde(default = "default_allow")]
    pub allow: Vec<String>,
    #[serde(default = "default_deny")]
    pub deny: Vec<String>,
}

impl Default for Rerun {
    fn default() -> Self {
        Rerun {
            mode: RerunMode::default(),
            allow: default_allow(),
            deny: default_deny(),
        }
    }
}

fn default_allow() -> Vec<String> {
    [
        "^(npm|pnpm|yarn|bun)$",
        "^(cargo|go|make|just)$",
        "^(tail|watch|less|htop|btop|top)$",
        "^(vim|nvim|python3?|node|deno)$",
        "^(docker|kubectl)$",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_deny() -> Vec<String> {
    ["^rm$", "^terraform$", "^(shutdown|reboot|dd|mkfs)$"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ResumeOverride {
    pub args: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    /// Session allow-list. herdr's plugin registry is GLOBAL — linking a plugin from one
    /// named session enables it in every session — so this is the only way to scope the
    /// plugin to specific sessions. Empty (the default) means "every session".
    /// Each entry is a `HERDR_SOCKET_PATH`. `$REOPEN_ALLOWED_SOCKETS` (colon-separated)
    /// overrides it.
    #[serde(default)]
    pub allowed_sockets: Vec<String>,
    #[serde(default = "t")]
    pub focus_on_reopen: bool,
    #[serde(default)]
    pub notify_on_close: bool,
    #[serde(default = "t")]
    pub notify_on_reopen: bool,
    #[serde(default = "t")]
    pub capture_exited: bool,
    #[serde(default = "d_ttl")]
    pub entry_ttl_hours: u64,
    #[serde(default = "d_pi")]
    pub process_info_ttl_ms: u64,
    #[serde(default)]
    pub rerun: Rerun,
    /// `[resume.<kind>] args = ["--resume", "{id}"]`
    #[serde(default)]
    pub resume: BTreeMap<String, ResumeOverride>,
}

fn t() -> bool {
    true
}
fn d_ttl() -> u64 {
    24
}
fn d_pi() -> u64 {
    5000
}

impl Default for Config {
    fn default() -> Self {
        Config {
            allowed_sockets: Vec::new(),
            focus_on_reopen: true,
            notify_on_close: false,
            notify_on_reopen: true,
            capture_exited: true,
            entry_ttl_hours: 24,
            process_info_ttl_ms: 5000,
            rerun: Rerun::default(),
            resume: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Never fails: a malformed config logs and yields defaults.
    pub fn load(config_dir: Option<&Path>) -> Self {
        let Some(dir) = config_dir else {
            return Config::default();
        };
        let p = dir.join("config.toml");
        match std::fs::read_to_string(&p) {
            Ok(s) => Self::parse(&s),
            Err(_) => Config::default(),
        }
    }

    /// Is this plugin allowed to act on the session behind `socket_path`?
    pub fn session_allowed(&self, socket_path: &std::path::Path) -> bool {
        let list: Vec<String> = match std::env::var("REOPEN_ALLOWED_SOCKETS") {
            Ok(v) if !v.trim().is_empty() => v.split(':').map(|s| s.trim().to_string()).collect(),
            _ => self.allowed_sockets.clone(),
        };
        if list.is_empty() {
            return true;
        }
        let want = normalize(socket_path);
        list.iter()
            .any(|p| normalize(std::path::Path::new(p)) == want)
    }

    pub fn parse(s: &str) -> Config {
        match toml::from_str::<Config>(s) {
            Ok(c) => c,
            Err(e) => {
                crate::lwarn!("config.toml is invalid ({e}); using defaults");
                Config::default()
            }
        }
    }
}

fn normalize(p: &std::path::Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .to_string()
}
