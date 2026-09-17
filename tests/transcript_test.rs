//! The Claude Code transcript pre-check, over tempdir fixtures.
//!
//! The encoding asserted here (`[^A-Za-z0-9] -> '-'`) was derived empirically from a
//! real `~/.claude/projects`: every directory whose transcript recorded a `cwd` matched
//! it exactly, except git worktrees, whose transcripts record the MAIN worktree's cwd —
//! the case the any-directory scan exists for.

use reopen::transcript::{check, claude_transcript, encode_cwd, Transcript};
use std::path::Path;

fn seed(dir: &Path, project: &str, id: &str) {
    let d = dir.join(project);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(format!("{id}.jsonl")), "{\"cwd\":\"/x\"}\n").unwrap();
}

#[test]
fn encoding_replaces_every_non_alphanumeric_with_a_dash() {
    assert_eq!(
        encode_cwd("/Users/chaugs/Personal/herdr-pane-reopen"),
        "-Users-chaugs-Personal-herdr-pane-reopen"
    );
    // A dot is not special: `github.com` becomes `github-com`, and `/.claude/` produces
    // the double dash seen in every worktree directory on the reference machine.
    assert_eq!(
        encode_cwd("/Users/x/go/src/github.com/acme/core/.claude/worktrees/w1"),
        "-Users-x-go-src-github-com-acme-core--claude-worktrees-w1"
    );
    assert_eq!(encode_cwd("/private/tmp"), "-private-tmp");
    assert_eq!(encode_cwd("/tmp/a_b c"), "-tmp-a-b-c");
}

#[test]
fn a_transcript_in_the_panes_own_project_directory_is_found() {
    let d = tempfile::tempdir().unwrap();
    let cwd = "/Users/x/Personal/proj";
    seed(
        d.path(),
        &encode_cwd(cwd),
        "48e1f320-aaaa-bbbb-cccc-000000000001",
    );
    let got = claude_transcript(d.path(), cwd, "48e1f320-aaaa-bbbb-cccc-000000000001");
    assert!(matches!(got, Transcript::Found(_)), "{got:?}");
}

#[test]
fn a_transcript_under_some_other_project_directory_is_still_found() {
    // The pane's cwd moved (or it is a worktree): the id must still be located.
    let d = tempfile::tempdir().unwrap();
    seed(d.path(), "-somewhere-else-entirely", "id-1234");
    let got = claude_transcript(d.path(), "/Users/x/Personal/proj", "id-1234");
    assert!(matches!(got, Transcript::Found(_)), "{got:?}");
}

#[test]
fn a_session_with_no_transcript_anywhere_is_missing() {
    let d = tempfile::tempdir().unwrap();
    seed(d.path(), "-Users-x-Personal-proj", "another-id");
    assert_eq!(
        claude_transcript(d.path(), "/Users/x/Personal/proj", "id-1234"),
        Transcript::Missing
    );
}

#[test]
fn an_empty_store_is_missing_not_unknown() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        claude_transcript(d.path(), "/Users/x/Personal/proj", "id-1234"),
        Transcript::Missing
    );
}

#[test]
fn a_store_that_does_not_exist_is_unknown_so_resume_is_left_alone() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        claude_transcript(&d.path().join("no-such-dir"), "/x", "id-1234"),
        Transcript::Unknown
    );
}

#[test]
fn an_id_that_is_not_a_safe_path_component_is_unknown() {
    let d = tempfile::tempdir().unwrap();
    for bad in ["", "../../etc/passwd", "a/b", "id.with.dots"] {
        assert_eq!(
            claude_transcript(d.path(), "/x", bad),
            Transcript::Unknown,
            "{bad:?}"
        );
    }
}

#[test]
fn a_directory_named_like_the_transcript_is_not_a_transcript() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("-x").join("id-1234.jsonl")).unwrap();
    assert_eq!(
        claude_transcript(d.path(), "/x", "id-1234"),
        Transcript::Missing
    );
}

#[test]
fn only_claude_is_checked_every_other_kind_is_unknown() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        check(Some(d.path()), "codex", "/x", "id-1234"),
        Transcript::Unknown
    );
    assert_eq!(
        check(Some(d.path()), "opencode", "/x", "id-1234"),
        Transcript::Unknown
    );
    assert_eq!(
        check(Some(d.path()), "claude", "/x", "id-1234"),
        Transcript::Missing
    );
    // No store configured at all: never interfere.
    assert_eq!(check(None, "claude", "/x", "id-1234"), Transcript::Unknown);
}
