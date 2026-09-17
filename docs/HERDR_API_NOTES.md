# herdr API notes

Behaviour of herdr 0.9.1 (socket protocol 22) that this plugin depends on. Several of these
contradict the obvious reading of the herdr docs, and all were verified against a live server.
If you observe herdr behaving differently, please update this file in the same PR.

1. `pane.closed` and `tab.closed` carry **only** `{pane_id|tab_id, workspace_id}`. Only
   `workspace.closed` carries a final snapshot. A shadow cache is mandatory.
2. herdr emits **one event per gesture, at the coarsest level**: closing a tab emits zero
   `pane.closed`; closing a workspace emits neither `tab.closed` nor `pane.closed`;
   emptying a tab destroys it with **no `tab.closed` at all**.
3. A shell that exits emits `pane.exited`, never `pane.closed` — and its context is rich
   where `pane.closed`'s is empty.
4. `tab.number` is a **creation ordinal, not a position**. Display order is the array
   order of `tab.list`; `tab.move insert_index` indexes that array.
5. `layout.apply {tab_id}` is a destructive replace that **appends**: new tab id, last
   position, auto-relabelled.
6. The socket is strictly **one request per connection**, and a connection with an active
   `events.subscribe` cannot serve requests. Subscription events are bare
   `{"event","data"}` lines with no `id`.
7. `pane.send_text` appends no newline; `pane.send_keys ["Enter"]` executes.
8. Agent alias rule is exactly `^[a-z][a-z0-9_-]{0,31}$`; a collision returns
   `agent_name_taken`, and `agent.list` entries carry no `name` unless started with an
   alias, so collisions must be handled reactively.
9. `agent.start` on a freshly created pane returns `agent_pane_busy` for 1–3 s.
10. `layout.updated` and `pane.updated` are **not** dispatched to plugin hooks (`plugin
    link` warns `unknown event`); they exist only as socket subscriptions. And
    `pane.agent_status_changed`, `pane.scroll_changed`, `pane.output_matched` require a
    `pane_id` *in the subscription*, so they cannot be subscribed globally.
11. `herdr api` has no `call` subcommand, `herdr tab move` does not exist in the CLI, and
    `herdr <sub> --help` is not a thing — the bare subcommand group prints usage.
12. `pane.process_info`'s `name` is the process *title* (claude shows `"2.1.274"`), so
    matching must use `argv0`.

13. `pane.process_info` on a pane running an agent lists the agent among
    `foreground_processes` with `argv0` set to the binary (`claude`), even though its
    `name` is the version string. A pane whose only foreground process is its own shell
    is genuinely idle — which is how "the agent I just started has already exited" is
    detected. `agent_status` cannot answer that: its enum is
    `idle|working|blocked|done|unknown`, with no "starting" and no "gone".
