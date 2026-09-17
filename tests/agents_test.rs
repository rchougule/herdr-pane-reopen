mod common;

use reopen::agents::*;
use reopen::config::{Config, ResumeOverride};

#[test]
fn the_verified_rows_match_the_installed_clis() {
    let v = |k: &str| resume_args(&Config::default(), k, "S1").unwrap();
    assert_eq!(
        v("claude"),
        (vec!["--resume".into(), "S1".into()], Confidence::Verified)
    );
    assert_eq!(
        v("codex"),
        (vec!["resume".into(), "S1".into()], Confidence::Verified)
    );
    assert_eq!(
        v("cursor"),
        (vec!["--resume".into(), "S1".into()], Confidence::Verified)
    );
    assert_eq!(
        v("hermes"),
        (vec!["--resume".into(), "S1".into()], Confidence::Verified)
    );
    // opencode takes --session, NOT --resume
    assert_eq!(
        v("opencode"),
        (vec!["--session".into(), "S1".into()], Confidence::Verified)
    );
}

#[test]
fn the_assumed_rows_are_labelled_as_such() {
    let c = Config::default();
    for k in ["grok", "devin", "droid", "qodercli", "qwen"] {
        let (args, conf) = resume_args(&c, k, "S1").unwrap();
        assert_eq!(args, ["--resume", "S1"]);
        assert_eq!(conf, Confidence::Assumed);
    }
    let (args, conf) = resume_args(&c, "copilot", "S1").unwrap();
    assert_eq!(args, ["--resume=S1"]);
    assert_eq!(conf, Confidence::Assumed);
    assert!(resume_args(&c, "nonesuch", "S1").is_none());
}

#[test]
fn a_config_override_wins_and_counts_as_verified() {
    let mut c = Config::default();
    c.resume.insert(
        "grok".into(),
        ResumeOverride {
            args: vec!["--session-id".into(), "{id}".into(), "--yes".into()],
        },
    );
    assert_eq!(
        resume_args(&c, "grok", "abc"),
        Some((
            vec!["--session-id".into(), "abc".into(), "--yes".into()],
            Confidence::Verified
        ))
    );
}

#[test]
fn alias_sanitization_obeys_the_exact_herdr_rule() {
    // ^[a-z][a-z0-9_-]{0,31}$ — probed directly against agent.start.
    assert!(is_valid_alias("ok-name_1"));
    assert!(!is_valid_alias("Bad Name"));
    assert!(!is_valid_alias("9lead"));
    assert!(!is_valid_alias("UPPER"));
    assert!(!is_valid_alias("dot.name"));
    assert!(!is_valid_alias(&"a".repeat(40)));
    assert!(!is_valid_alias(""));

    for seed in [
        "Bad Name",
        "9lead",
        "UPPER",
        "dot.name",
        &"a".repeat(40),
        "backend-caching-plan",
        "!!!",
        "",
    ] {
        let a = alias(seed);
        assert!(is_valid_alias(&a), "{seed:?} produced {a:?}");
        assert!(a.len() <= 28, "{a:?} leaves no room for the retry suffix");
    }
    assert_eq!(alias("Bad Name"), "bad-name");
    assert_eq!(alias("9lead"), "re-9lead");
    assert_eq!(alias("dot.name"), "dot-name");
}

#[test]
fn the_agent_name_taken_ladder_stays_inside_32_chars() {
    let c = alias_candidates("study");
    assert_eq!(c, ["study", "study-2", "study-3", "study-4", "study-5"]);
    let long = alias(&"z".repeat(40));
    for cand in alias_candidates(&long) {
        assert!(cand.len() <= 32 && is_valid_alias(&cand), "{cand}");
    }
}

#[test]
fn alias_seed_prefers_the_label_then_the_cwd_basename() {
    assert_eq!(alias_seed(Some("flow"), "/tmp/x"), "flow");
    assert_eq!(alias_seed(None, "/Users/chaugs/Personal/study"), "study");
    assert_eq!(
        alias_seed(Some("  "), "/Users/chaugs/Personal/study"),
        "study"
    );
    assert_eq!(
        alias(&alias_seed(None, "/Users/chaugs/Personal/study")),
        "study"
    );
}
