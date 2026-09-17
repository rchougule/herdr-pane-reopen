# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-09-18

### Added

- `reopen-last` action: restores the most recently closed pane, tab or workspace in its
  original workspace, tab and position, with the original working directory and split layout.
- Close cascades (last pane of a tab, last tab of a workspace, whole-container closes) are
  coalesced into a single undo entry at the correct granularity.
- Coding-agent sessions (Claude Code, Codex, Cursor, OpenCode and others) are resumed in
  the restored pane through the agent's own resume command.
- Non-agent foreground commands are captured and prefilled in the restored pane without
  executing them; an opt-in `rerun` mode with allow and deny lists can run them.
- `list` and `doctor` actions for inspecting the undo stack and plugin health.
- Self-healing background snapshot daemon that survives `herdr server reload-config`.
- Per-session state so several herdr sessions never share an undo stack.
- Live scenario suite (`scripts/qa.sh`) that runs against an isolated herdr session.

### Known limitations

- A closed pane's process is gone. Working directory, layout and agent conversations come
  back; running servers, editors, REPL state and scrollback do not.
- Resume arguments for agents other than Claude Code are taken from their documentation and
  can be overridden in the plugin config.

[0.1.0]: https://github.com/rchougule/herdr-pane-reopen/releases/tag/v0.1.0
