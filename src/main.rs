//! `reopen` — undo close for herdr panes, tabs and workspaces.

use clap::{Parser, Subcommand};
use reopen::close;
use reopen::config::Config;
use reopen::daemon;
use reopen::env::HerdrEnv;
use reopen::model::*;
use reopen::restore::{self, Ctx};
use reopen::rpc::{Client, Rpc};
use reopen::snapshot;
use reopen::store::{self, Store};
use reopen::{lerror, linfo, lwarn, now_ms, EXPECTED_PROTOCOL};
use serde_json::json;

#[derive(Parser)]
#[command(name = "reopen", version, about = "Undo close for herdr")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Event hook entrypoint (reads HERDR_PLUGIN_EVENT / _EVENT_JSON).
    Event,
    /// Plugin startup hook.
    Startup,
    /// Background daemon (spawned detached; not for manual use).
    Watch,
    /// Force a full snapshot refresh and print a summary.
    Snapshot,
    /// Restore the newest entry on the undo stack.
    ReopenLast,
    /// Restore a specific entry by id.
    Reopen {
        #[arg(long)]
        id: u64,
    },
    /// Print the undo stack.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Diagnose socket, state, daemon and configuration.
    Doctor,
    /// Popup picker — deferred to v0.2.
    Pick,
}

struct App {
    env: HerdrEnv,
    store: Store,
    client: Client,
    cfg: Config,
}

impl App {
    fn new() -> App {
        let env = HerdrEnv::load();
        let _ = env.ensure_state_dir();
        let store = Store::new(&env.state_dir);
        let client = Client::new(&env.socket_path);
        let cfg = Config::load(env.config_dir.as_deref());
        App {
            env,
            store,
            client,
            cfg,
        }
    }
    fn ctx(&self) -> Ctx<'_> {
        Ctx::new(&self.client, &self.store, &self.cfg)
    }
}

fn main() {
    let cli = Cli::parse();
    let app = App::new();

    // herdr's plugin registry is global: a plugin linked from any session is enabled in
    // every session. `allowed_sockets` (config.toml) / $REOPEN_ALLOWED_SOCKETS scope it
    // back down. Hooks exit silently so an excluded session's plugin log stays clean.
    if !app.cfg.session_allowed(&app.env.socket_path) {
        match cli.cmd {
            Cmd::Event | Cmd::Startup | Cmd::Watch => return,
            _ => {
                println!(
                    "{}",
                    json!({"ok": false,
                           "disabled_for_session": app.env.socket_path,
                           "reason": "this session is not in allowed_sockets (config.toml)"})
                );
                return;
            }
        }
    }

    match cli.cmd {
        Cmd::Event => cmd_event(&app),
        Cmd::Startup => cmd_startup(&app),
        Cmd::Watch => daemon::watch(&app.client, &app.store, &app.cfg),
        Cmd::Snapshot => cmd_snapshot(&app),
        Cmd::ReopenLast => cmd_reopen(&app, None),
        Cmd::Reopen { id } => cmd_reopen(&app, Some(id)),
        Cmd::List { json } => cmd_list(&app, json),
        Cmd::Doctor => cmd_doctor(&app),
        Cmd::Pick => cmd_pick(&app),
    }
}

fn cmd_event(app: &App) {
    let t0 = now_ms();
    let Some(name) = app.env.event.clone() else {
        lwarn!("event: HERDR_PLUGIN_EVENT is unset");
        return;
    };
    let ev_json = app.env.event_json.clone().unwrap_or_default();

    if close::is_close_event(&name) {
        close::on_close(
            &app.store,
            &app.client,
            &app.cfg,
            &name,
            &ev_json,
            app.env.context_json.as_deref(),
            t0,
        );
        return;
    }

    // `pane.focused` / `pane.agent_status_changed` fire constantly (see docs/HERDR_API_NOTES.md) and
    // carry nothing the shadow cache needs: their only job is to keep the daemon armed.
    // Refreshing on every one of them ran a full ~30-call sweep several times a second
    // (F6).
    if matches!(name.as_str(), "pane.focused" | "pane.agent_status_changed") {
        snapshot::refresh_if_stale(&app.client, &app.store, 2_000, app.cfg.process_info_ttl_ms);
        daemon::ensure_running(&app.store);
        return;
    }

    // Non-close event: seed the cache from the payload (free), then keep the daemon armed.
    daemon::recover_once(&app.store, &app.client, &app.cfg);
    seed_from_event(app, &name, &ev_json);
    snapshot::refresh_if_stale(&app.client, &app.store, 150, app.cfg.process_info_ttl_ms);
    daemon::ensure_running(&app.store);
}

/// `pane.created` / `tab.created` / `workspace.created` carry the full object — merging
/// it costs zero socket calls and closes the create-then-close-inside-one-TTL gap.
fn seed_from_event(app: &App, name: &str, ev_json: &str) {
    // Decide BEFORE taking the exclusive lock and deserializing the whole snapshot (F6).
    if !matches!(name, "pane.created" | "tab.created" | "workspace.created") {
        return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(ev_json) else {
        return;
    };
    let data = match v.get("data") {
        Some(d) => d.clone(),
        None => return,
    };
    let _g = app.store.lock();
    let mut snap = app.store.snapshot();
    let now = now_ms();
    match name {
        "pane.created" => {
            if let Some(p) = data
                .get("pane")
                .cloned()
                .and_then(|p| serde_json::from_value::<snapshot::RawPane>(p).ok())
            {
                snapshot::merge_pane_created(&mut snap, &p, now);
            }
        }
        "tab.created" => {
            if let Some(t) = data
                .get("tab")
                .cloned()
                .and_then(|t| serde_json::from_value::<snapshot::RawTab>(t).ok())
            {
                snapshot::merge_tab_created(&mut snap, &t);
            }
        }
        "workspace.created" => {
            if let Some(w) = data
                .get("workspace")
                .cloned()
                .and_then(|w| serde_json::from_value::<snapshot::RawWorkspace>(w).ok())
            {
                snapshot::merge_workspace_created(&mut snap, &w);
            }
        }
        _ => return,
    }
    let _ = app.store.write_snapshot(&snap);
}

fn cmd_startup(app: &App) {
    check_protocol(app);
    daemon::recover(&app.store, &app.client, &app.cfg);
    snapshot::refresh(&app.client, &app.store, app.cfg.process_info_ttl_ms);
    daemon::ensure_running(&app.store);
    linfo!("startup complete (state {})", app.env.state_dir.display());
}

fn check_protocol(app: &App) -> Option<u32> {
    match app.client.ping() {
        Ok((v, p)) => {
            if p != EXPECTED_PROTOCOL {
                lwarn!("herdr {v} speaks socket protocol {p}, this build expects {EXPECTED_PROTOCOL}: running degraded");
            }
            Some(p)
        }
        Err(e) => {
            lerror!(
                "cannot reach the herdr socket at {}: {e}",
                app.env.socket_path.display()
            );
            None
        }
    }
}

fn cmd_snapshot(app: &App) {
    let s = snapshot::refresh(&app.client, &app.store, app.cfg.process_info_ttl_ms);
    let tabs: usize = s.workspaces.iter().map(|w| w.tabs.len()).sum();
    let panes: usize = s.workspaces.iter().map(|w| w.pane_count()).sum();
    println!(
        "{}",
        json!({"workspaces": s.workspaces.len(), "tabs": tabs, "panes": panes,
               "taken_at_ms": s.taken_at_ms, "state_dir": app.env.state_dir})
    );
}

fn cmd_list(app: &App, as_json: bool) {
    // Any user-facing action arms the daemon: a fresh install has seen no event yet.
    daemon::ensure_running(&app.store);
    let stack = app.store.closed();
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&stack).unwrap_or_default()
        );
        return;
    }
    if stack.entries.is_empty() {
        println!("nothing closed recently");
        return;
    }
    for e in &stack.entries {
        let age = now_ms().saturating_sub(e.closed_at_ms) / 1000;
        let exited = if e.reason == CloseReason::Exited {
            " (exited)"
        } else {
            ""
        };
        let cmds: Vec<String> = e
            .workspace
            .tabs
            .iter()
            .flat_map(|t| t.panes.iter())
            .filter_map(|p| p.foreground.as_ref().map(|f| f.argv.join(" ")))
            .collect();
        let extra = if cmds.is_empty() {
            String::new()
        } else {
            format!("  [{}]", cmds.join("; "))
        };
        println!(
            "#{:<3} {:>5}s ago  {}{}{}",
            e.id, age, e.summary, exited, extra
        );
    }
}

fn cmd_reopen(app: &App, id: Option<u64>) {
    check_protocol(app);
    daemon::ensure_running(&app.store);
    // Pop under the lock so two invocations cannot restore the same entry.
    let entry = {
        let _g = app.store.lock();
        let mut stack = app.store.closed();
        store::prune_entries(
            &mut stack,
            now_ms(),
            app.cfg.entry_ttl_hours,
            reopen::STACK_CAP,
        );
        let idx = match id {
            Some(want) => stack.entries.iter().position(|e| e.id == want),
            None => (!stack.entries.is_empty()).then_some(0),
        };
        match idx {
            Some(i) => {
                let e = stack.entries.remove(i);
                let _ = app.store.write_closed(&stack);
                Some(e)
            }
            None => None,
        }
    };
    let Some(entry) = entry else {
        println!("{}", json!({"ok": false, "error": "nothing to reopen"}));
        return;
    };

    let report = restore::run(&entry, &app.ctx());
    if !report.ok && report.created_nothing() {
        restore::push_back(&app.store, &entry);
    }
    println!("{}", serde_json::to_string(&report).unwrap_or_default());

    if app.cfg.notify_on_reopen {
        let mut body = format!("{} pane(s)", report.created.panes.len());
        if report.resumed > 0 {
            body.push_str(&format!(" · {} agent(s) resumed", report.resumed));
        }
        if report.started_fresh > 0 {
            body.push_str(&format!(
                " · {} started fresh (nothing to resume)",
                report.started_fresh
            ));
        }
        if !report.failed.is_empty() {
            body.push_str(&format!(" · {} could not resume", report.failed.len()));
        }
        let _ = app.client.ok(
            "notification.show",
            json!({"title": format!("Reopened {}", entry.summary), "body": body}),
        );
    }
    // The restore changed the world; refresh the shadow cache immediately.
    snapshot::refresh(&app.client, &app.store, app.cfg.process_info_ttl_ms);
}

/// The `[[panes]] picker` popup entrypoint. Interactive selection ships in v0.2; until
/// then it renders the stack read-only rather than showing a bare "coming soon" (F20.5).
fn cmd_pick(app: &App) {
    let stack = app.store.closed();
    println!("  reopen — recently closed");
    println!("  ------------------------");
    if stack.entries.is_empty() {
        println!("  nothing closed recently");
    } else {
        for e in stack.entries.iter().take(10) {
            let age = now_ms().saturating_sub(e.closed_at_ms) / 1000;
            println!("  #{:<3} {:>5}s ago  {}", e.id, age, e.summary);
        }
    }
    println!();
    println!("  prefix+u reopens the newest; `reopen reopen --id N` picks one.");
    println!("  (interactive selection ships in v0.2)");
}

fn cmd_doctor(app: &App) {
    // Doctor is what people run right after installing: make sure the snapshot exists and
    // the daemon is armed before reporting on them, instead of waiting for the first event.
    snapshot::refresh_if_stale(&app.client, &app.store, 2_000, app.cfg.process_info_ttl_ms);
    daemon::ensure_running(&app.store);
    let mut out = serde_json::Map::new();
    out.insert("plugin_id".into(), json!(app.env.plugin_id));
    out.insert("state_dir".into(), json!(app.env.state_dir));
    out.insert("socket_path".into(), json!(app.env.socket_path));
    out.insert("expected_protocol".into(), json!(EXPECTED_PROTOCOL));

    match app.client.ping() {
        Ok((v, p)) => {
            out.insert("herdr_version".into(), json!(v));
            out.insert("protocol".into(), json!(p));
            out.insert("protocol_ok".into(), json!(p == EXPECTED_PROTOCOL));
        }
        Err(e) => {
            out.insert("socket_error".into(), json!(e.to_string()));
            out.insert("protocol_ok".into(), json!(false));
        }
    }

    let writable = store::write_atomic(&app.store.dir, "doctor.probe", b"ok").is_ok();
    let _ = std::fs::remove_file(app.store.path("doctor.probe"));
    out.insert("state_dir_writable".into(), json!(writable));
    out.insert(
        "state_lock_acquirable".into(),
        json!(app.store.lock().is_some()),
    );

    match daemon::daemon_status(&app.store) {
        Some((pid, alive)) => {
            out.insert("daemon_pid".into(), json!(pid));
            out.insert("daemon_alive".into(), json!(alive));
        }
        None => {
            out.insert("daemon_pid".into(), json!(null));
            out.insert("daemon_alive".into(), json!(false));
        }
    }

    let snap = app.store.snapshot();
    out.insert(
        "snapshot_age_ms".into(),
        json!(now_ms().saturating_sub(snap.taken_at_ms)),
    );
    out.insert("snapshot_workspaces".into(), json!(snap.workspaces.len()));
    let stack = app.store.closed();
    out.insert("stack_entries".into(), json!(stack.entries.len()));

    // Which agent kinds on the stack have only an ASSUMED resume flag?
    let mut assumed: Vec<String> = Vec::new();
    let mut kinds: Vec<String> = Vec::new();
    for e in &stack.entries {
        for p in e.workspace.tabs.iter().flat_map(|t| t.panes.iter()) {
            if let Some(k) = &p.agent {
                if !kinds.contains(k) {
                    kinds.push(k.clone());
                }
                if matches!(
                    reopen::agents::resume_args(&app.cfg, k, "x"),
                    Some((_, reopen::agents::Confidence::Assumed))
                ) && !assumed.contains(k)
                {
                    assumed.push(k.clone());
                }
            }
        }
    }
    out.insert("agent_kinds_on_stack".into(), json!(kinds));
    out.insert("assumed_resume_kinds".into(), json!(assumed));

    // Integration status for those kinds (informational).
    if let Some(bin) = &app.env.bin_path {
        if let Ok(o) = std::process::Command::new(bin)
            .arg("integration")
            .arg("status")
            .output()
        {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            let mut lines: Vec<String> = Vec::new();
            for k in &kinds {
                for l in text.lines() {
                    if l.starts_with(&format!("{k}:")) {
                        lines.push(l.trim().to_string());
                    }
                }
            }
            out.insert("integration_status".into(), json!(lines));
        }
    }

    // Read-only check for a keybinding (never edits the user's config).
    // The session's own config.toml, not always the default session's (F20.4).
    let cfgp = app
        .env
        .socket_path
        .parent()
        .map(|p| p.join("config.toml"))
        .filter(|p| p.exists())
        .or_else(|| {
            reopen::env::default_socket_path()
                .parent()
                .map(|p| p.join("config.toml"))
        });
    let bound = cfgp
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.contains("rchougule.reopen."))
        .unwrap_or(false);
    out.insert("keybinding_present".into(), json!(bound));
    if !bound {
        out.insert(
            "keybinding_hint".into(),
            json!("add [[keys.command]] key=\"prefix+u\" type=\"plugin_action\" command=\"rchougule.reopen.reopen-last\" to ~/.config/herdr/config.toml, then `herdr server reload-config`"),
        );
    }

    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}
