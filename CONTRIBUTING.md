# Contributing

Thanks for helping make undo-close better. This is a small Rust codebase with no
runtime dependencies beyond herdr itself, so most changes are quick to build and test.

## Prerequisites

- Rust stable (see `rust-toolchain.toml`)
- herdr 0.9.1 or newer, running locally

## Build and test

```sh
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Unit tests run against real herdr payloads captured in `tests/fixtures/`. If you change
how a herdr response is parsed, add or update a fixture rather than hand-writing JSON.

## Live testing in an isolated herdr session

The plugin closes and recreates panes, so never develop against the session you work in.
`scripts/qa.sh` runs the full end-to-end scenario suite in a separate named session and
leaves your real session untouched:

```sh
scripts/dev-link.sh      # builds, links the plugin, scopes it to the reopen-qa session
scripts/qa.sh            # 12 live scenarios, exits non-zero on any failure
```

The isolation works through the `allowed_sockets` config key. Because herdr's plugin
registry is global, a linked plugin is visible in every session; `allowed_sockets` is what
keeps it inert outside the QA session while you iterate.

## Project layout

| Path | What lives there |
| --- | --- |
| `src/rpc.rs` | NDJSON client for herdr's Unix socket, one request per connection |
| `src/snapshot.rs` | Shadow snapshot of the workspace, tab and pane tree |
| `src/close.rs` | Close hooks, burst coalescing, granularity inference |
| `src/restore.rs` | Rebuilding a pane, tab or workspace and resuming its occupant |
| `src/agents.rs` | Per-agent resume arguments and alias generation |
| `src/daemon.rs` | Self-healing watch loop that keeps the snapshot fresh |
| `src/store.rs` | Locked, atomic reads and writes of state files |
| `scripts/qa.sh` | Live scenario suite against an isolated session |

## Pull requests

- Keep commits focused and use conventional commit prefixes (`feat:`, `fix:`, `docs:`, `test:`).
- Add a unit test for any change to the pure logic (inference, coalescing, resume args).
- Run `scripts/qa.sh` before opening a PR if you touched capture or restore, and paste
  the summary line in the PR description.
- If you discover herdr behaving differently from what the README's "herdr API notes"
  section says, please update that section; it is the only place those facts are recorded.

## Adding an agent

Resume behaviour lives in a single table in `src/agents.rs`. To add a coding agent, add a
row with its herdr `kind` and the exact resume arguments its CLI expects, mark it as
verified only if you have exercised it end to end, and add a fixture-based test.
