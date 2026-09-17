# How it works

- **State is per herdr session.** herdr keys `HERDR_PLUGIN_STATE_DIR` on the plugin id
  only, so every session is handed the same directory; reopen appends a key derived from
  the canonicalized `HERDR_SOCKET_PATH` and keeps all of its files under
  `~/.local/state/herdr/plugins/rchougule.reopen/<session>-<hash>/`
  (`default-…` for your everyday session, `reopen-qa-…` for the QA one). `reopen doctor`
  prints the exact path in `state_dir`. Two sessions can never share an undo stack, a
  shadow snapshot or a daemon lock.
- A **shadow snapshot** (`snapshot.json` in that state dir) mirrors every workspace,
  tab, split tree and pane, because `pane.closed` carries only `{pane_id, workspace_id}` —
  no cwd, no tab, no label, no agent — and the pane is already gone by hook time.
- Create events carry the full object, so they are merged into the snapshot for free; a
  full refresh costs ~3 ms and runs on a 150 ms TTL.
- Close hooks join **one global 250 ms coalescing burst**. The hook that wrote last (by a
  unique `{timestamp, pid, nonce}` token, not a timestamp comparison — concurrent hooks
  routinely share a millisecond) finalizes the burst into exactly one undo entry.
- Granularity is inferred: a pane that was the last in its tab becomes a *tab* entry,
  `pane.closed` + `workspace.closed` for the **same workspace** in one burst becomes a
  single *workspace* entry, and a `workspace close --group` burst becomes one
  *workspace-group* entry. The burst is a time window, not a gesture, so it is first
  partitioned by workspace — two unrelated closes 100 ms apart stay two entries.
- Restores are top-down: `layout.apply` rebuilds a whole tab in one call, then an explicit
  ascending `tab.move` pass fixes the order (an applied tab always lands last).
- Everything reopen creates is recorded for 5 s so a failed restore cleaning itself up is
  never captured as a new close. A remembered id is only trusted when its label or one of
  its cwds still matches, because herdr reuses ids.
- A small `watch` daemon subscribes to the socket for the events plugin hooks never
  receive (`layout.updated`, `pane.updated`, `pane.moved`, `tab.moved`). It is re-armed by
  the next hook if it ever dies.

**Pane-granularity caveat.** When the closed pane's sibling was itself a split, reopen
puts the pane back on the *opposite* side of that split. Rebuilding it exactly would need
a destructive `layout.apply`, which would kill the surviving panes' processes. The restore
report says so when it happens.

