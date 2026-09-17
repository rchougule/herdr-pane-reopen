# Development

```sh
cargo test                 # unit tests over captured herdr payloads in tests/fixtures/
cargo clippy --all-targets -- -D warnings
cargo fmt --check
./scripts/dev-link.sh      # build + link the working tree
./scripts/qa.sh            # live integration QA, in an ISOLATED herdr session
```

## Develop against an isolated session, never your real one

Live QA closes panes, tabs and workspaces. Do that in a **named herdr session** with its
own server, socket and containers — never in the session you work in:

```sh
# start an isolated server (CLAUDE_CODE_* markers stripped: a herdr server that inherits
# CLAUDE_CODE_CHILD_SESSION makes every claude it spawns disable transcript saving, and a
# session with no transcript cannot be resumed)
env -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SESSION_ID -u CLAUDECODE \
    herdr --session reopen-qa server &

herdr --session reopen-qa plugin link "$PWD"
HERDR_SESSION=reopen-qa ./scripts/qa.sh          # reopen-qa is also the default
herdr session stop reopen-qa                     # when you are done
```

Its socket is `~/.config/herdr/sessions/reopen-qa/herdr.sock`; `herdr session list --json`
prints it.

**The plugin registry is global.** `herdr --session X plugin link` enables the plugin in
*every* session, including your real one, and there is no per-session unlink. Keep it
inert outside the QA session with the allow-list:

```toml
# <herdr plugin config-dir rchougule.reopen>/config.toml
allowed_sockets = ["/Users/you/.config/herdr/sessions/reopen-qa/herdr.sock"]
```

With a non-empty list, hooks and the daemon exit immediately in any other session (no
state written, no log noise) and the user-facing commands say `disabled_for_session`.
`$REOPEN_ALLOWED_SOCKETS` (colon-separated) overrides the file. Remove the setting to run
everywhere.

`scripts/qa.sh` records the session's live pane ids first, only ever touches containers it
created (`reopen-qa-*`), keeps a `qa-anchor` workspace so the session never empties, and
aborts if any pre-existing pane disappears. CI runs build, test, clippy and fmt on macOS
and Linux; the live QA needs a running herdr and is not part of CI.

