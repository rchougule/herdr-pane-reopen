//! State partitioning per herdr session (review F1).
//!
//! herdr keys `HERDR_PLUGIN_STATE_DIR` on the plugin id ONLY, so every session is handed
//! the same directory. Sharing `closed.json` / `snapshot.json` / `watch.lock` across
//! sessions means one session's undo pops another's entry, the shadow snapshot describes
//! whichever session refreshed last, and the second session never spawns a daemon.

use reopen::env::{base_state_dir, session_key};
use std::path::{Path, PathBuf};

#[test]
fn the_default_session_and_a_named_session_get_different_state_dirs() {
    let default = Path::new("/Users/x/.config/herdr/herdr.sock");
    let named = Path::new("/Users/x/.config/herdr/sessions/reopen-qa/herdr.sock");

    let a = session_key(default);
    let b = session_key(named);
    assert_ne!(a, b);
    assert!(a.starts_with("default-"), "{a}");
    assert!(b.starts_with("reopen-qa-"), "{b}");

    let base: PathBuf = base_state_dir("rchougule.reopen");
    assert_ne!(base.join(&a), base.join(&b));
    // the base dir itself is never written to directly
    assert!(base.join(&a).starts_with(&base));
}

#[test]
fn the_key_is_stable_and_collision_resistant() {
    let a = Path::new("/Users/x/.config/herdr/sessions/work/herdr.sock");
    let b = Path::new("/Users/y/.config/herdr/sessions/work/herdr.sock");
    // stable across calls
    assert_eq!(session_key(a), session_key(a));
    // same session NAME under a different root is still a different session
    assert_ne!(session_key(a), session_key(b));
    assert!(session_key(a).starts_with("work-"));
}

#[test]
fn an_unusual_session_name_still_produces_a_safe_directory_name() {
    for p in [
        "/tmp/../tmp/herdr.sock",
        "/Users/x/.config/herdr/sessions/A Weird Name!/herdr.sock",
        "/herdr.sock",
    ] {
        let k = session_key(Path::new(p));
        assert!(!k.is_empty(), "{p}");
        assert!(
            k.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'),
            "{p} → {k}"
        );
        assert!(!k.contains('/'), "{p} → {k}");
        assert!(!k.starts_with('.'), "{p} → {k}");
    }
}
