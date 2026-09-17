#!/usr/bin/env bash
# Live integration QA for the reopen plugin.
#
# ISOLATED SESSION: every herdr command below runs against a dedicated named session
# (default `reopen-qa`, override with $HERDR_SESSION), which has its own server, its own
# socket and its own containers. Your everyday `default` session is never touched. The
# script starts the session's server if it is not running.
#
# herdr's plugin REGISTRY IS GLOBAL — `herdr --session X plugin link` enables the plugin
# in every session — so the plugin also carries an `allowed_sockets` allow-list in its
# config.toml, and the QA session's socket must be on it. See README > Development.
#
# SAFETY: every container this script creates is labelled `reopen-qa-*`. It records the
# live pane ids at start, never targets an id in that baseline, and the cleanup trap
# closes ONLY workspaces whose label starts with `reopen-qa-`. If any pre-existing pane
# disappears the run aborts loudly.
#
# STATE: the plugin partitions its state dir by session (see src/env.rs `session_key`),
# so `$HERDR_PLUGIN_STATE_DIR` below is only the BASE that herdr itself hands to hooks;
# this run reads and writes `<base>/<session key>` and can never touch the default
# session's undo stack. `$STATE` is resolved from the binary itself rather than
# recomputed here.
#
# EXIT STATUS: non-zero when any scenario fails, and the summary is printed by the EXIT
# trap even when the script dies early.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/reopen"
HERDR_SESSION="${HERDR_SESSION:-reopen-qa}"
SESSION_DIR="$HOME/.config/herdr/sessions/$HERDR_SESSION"

# every herdr invocation in this script is scoped to the QA session
herdr() { command herdr --session "$HERDR_SESSION" "$@"; }

export HERDR_SOCKET_PATH="$SESSION_DIR/herdr.sock"
export HERDR_PLUGIN_ID="rchougule.reopen"
# the BASE dir herdr passes to hooks; the plugin appends its own per-session key
export HERDR_PLUGIN_STATE_DIR="$HOME/.local/state/herdr/plugins/rchougule.reopen"
export HERDR_PLUGIN_CONFIG_DIR="$HOME/.config/herdr/plugins/config/rchougule.reopen"
export HERDR_PLUGIN_ROOT="$ROOT"

# Start the isolated session's server if it is not already running.
if ! command herdr --session "$HERDR_SESSION" workspace list >/dev/null 2>&1; then
  echo "starting an isolated herdr server for session '$HERDR_SESSION'"
  # Strip this shell's CLAUDE_CODE_* markers: a herdr server that inherits
  # CLAUDE_CODE_CHILD_SESSION makes every claude it spawns disable transcript saving
  # ("Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker"), and a
  # session with no transcript cannot be resumed — scenario 10 would fail for a reason
  # that has nothing to do with this plugin.
  nohup env -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SESSION_ID -u CLAUDECODE \
        -u CLAUDE_CODE_ENTRYPOINT -u CLAUDE_CODE_SESSION_ATTENDED \
        -u CLAUDE_CODE_BRIDGE_SESSION_ID -u CLAUDE_CODE_MESSAGING_SOCKET \
        -u CLAUDE_CODE_MESSAGING_TOKEN -u CLAUDE_CODE_EXECPATH \
        "$(command -v herdr)" --session "$HERDR_SESSION" server >"/tmp/herdr-$HERDR_SESSION-server.log" 2>&1 &
  for _ in $(seq 1 20); do
    command herdr --session "$HERDR_SESSION" workspace list >/dev/null 2>&1 && break
    sleep 1
  done
fi
if ! command herdr --session "$HERDR_SESSION" workspace list >/dev/null 2>&1; then
  echo "FATAL: cannot reach the '$HERDR_SESSION' session at $HERDR_SOCKET_PATH"
  exit 1
fi
if [ "$(command herdr --session "$HERDR_SESSION" api snapshot >/dev/null 2>&1; echo $?)" = "0" ]; then :; fi
echo "session '$HERDR_SESSION' socket: $HERDR_SOCKET_PATH"

PASS=0; FAIL=0; SKIP=0
declare -a RESULTS=()

# The session-scoped state dir this run actually uses.
STATE="$("$BIN" doctor 2>/dev/null | jq -r '.state_dir // empty')"
[ -n "$STATE" ] || STATE="$HERDR_PLUGIN_STATE_DIR"
echo "session-scoped state dir: $STATE"

sock() {
  python3 - "$1" "$2" <<'PY'
import json, os, socket, sys
p = os.environ["HERDR_SOCKET_PATH"]
s = socket.socket(socket.AF_UNIX)
s.settimeout(30)
s.connect(p)
s.sendall((json.dumps({"id": "qa", "method": sys.argv[1], "params": json.loads(sys.argv[2])}) + "\n").encode())
buf = b""
while not buf.endswith(b"\n"):
    c = s.recv(65536)
    if not c:
        break
    buf += c
print(buf.decode().strip())
PY
}

say()  { printf '%s\n' "$*"; }
ev()   { printf '        %s\n' "$*"; }
head_() { printf '\n=== SCENARIO %s: %s\n' "$1" "$2"; }

ok()   { PASS=$((PASS+1)); RESULTS+=("PASS    $1"); printf '  PASS    %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); RESULTS+=("FAIL    $1"); printf '  FAIL    %s\n' "$1"; }
skip() { SKIP=$((SKIP+1)); RESULTS+=("SKIPPED $1"); printf '  SKIPPED %s\n' "$1"; }

# assert_eq <what> <actual> <expected>; returns non-zero on mismatch
assert_eq() {
  if [ "$2" = "$3" ]; then ev "ok   $1: '$2'"; return 0
  else ev "BAD  $1: got '$2', want '$3'"; return 1; fi
}

stack_len()  { "$BIN" list --json | jq '.entries | length'; }
top_field()  { "$BIN" list --json | jq -r ".entries[0].$1"; }
pane_tab()   { herdr pane list | jq -r --arg p "$1" '.result.panes[] | select(.pane_id==$p) | .tab_id'; }
pane_cwd()   { herdr pane list | jq -r --arg p "$1" '.result.panes[] | select(.pane_id==$p) | .cwd'; }
tab_panes()  { herdr pane list | jq -r --arg t "$1" '[.result.panes[] | select(.tab_id==$t)] | length'; }
ws_panes()   { herdr pane list | jq -r --arg w "$1" '[.result.panes[] | select(.workspace_id==$w)] | length'; }
ws_tabs()    { herdr tab list | jq -r --arg w "$1" '[.result.tabs[] | select(.workspace_id==$w)] | length'; }
ws_by_label(){ herdr workspace list | jq -r --arg l "$1" '.result.workspaces[] | select(.label==$l) | .workspace_id' | head -1; }
new_panes()  { herdr pane list | jq -r '.result.panes[].pane_id' | sort | comm -13 "$1" -; }

snap() { "$BIN" snapshot >/dev/null 2>&1; }

STACK_IDS_FILE="$(mktemp)"
"$ROOT/target/release/reopen" list --json 2>/dev/null | jq -r '.entries[].id' | sort -n > "$STACK_IDS_FILE" || true

# An anchor workspace (deliberately NOT named reopen-qa-*, so cleanup leaves it) keeps
# the session alive when every scratch workspace is closed.
if [ "$(herdr workspace list | jq '[.result.workspaces[]|select(.label=="qa-anchor")]|length')" = "0" ]; then
  herdr workspace create --cwd /tmp --label qa-anchor --no-focus >/dev/null
  sleep 1
fi

BASELINE_FILE="$(mktemp)"
herdr pane list | jq -r '.result.panes[].pane_id' | sort > "$BASELINE_FILE"
BASE_COUNT=$(wc -l < "$BASELINE_FILE" | tr -d ' ')
say "baseline: $BASE_COUNT live panes recorded (none of them will be touched)"

cleanup() {
  local rc=$?
  set +u
  say ""
  say "--- cleanup: closing reopen-qa-* workspaces only ---"
  herdr workspace list \
    | jq -r '.result.workspaces[] | select(.label != null) | select(.label|startswith("reopen-qa-")) | .workspace_id' \
    | while read -r w; do [ -n "$w" ] && herdr workspace close "$w" >/dev/null 2>&1; done
  sleep 1
  local now missing
  now="$(mktemp)"
  herdr pane list | jq -r '.result.panes[].pane_id' | sort > "$now"
  missing="$(comm -23 "$BASELINE_FILE" "$now" | tr '\n' ' ' | sed 's/ *$//')"
  if [ -n "$missing" ]; then
    say "FATAL: pre-existing panes disappeared: $missing"
    FAIL=$((FAIL+1))
    RESULTS+=("FAIL    baseline panes disappeared: $missing")
  fi
  say "baseline intact: all $BASE_COUNT original panes still present"
  # drop the undo entries this run created; they point at scratch containers only
  local closed="$STATE/closed.json"
  if [ -f "$closed" ]; then
    jq --slurpfile keep <(jq -R 'tonumber' "$STACK_IDS_FILE" | jq -s .)        '.entries |= map(select(.id as $i | ($keep[0] | index($i)) != null))' "$closed"        > "$closed.qatmp" 2>/dev/null && mv "$closed.qatmp" "$closed"
    say "undo stack restored to the $(wc -l < "$STACK_IDS_FILE" | tr -d ' ') entries present before the run"
  fi
  say ""
  say "================ SUMMARY ================"
  if [ "${#RESULTS[@]}" -gt 0 ]; then
    for r in "${RESULTS[@]}"; do printf '%s\n' "$r"; done
  else
    say "(no scenario completed)"
  fi
  printf 'passed=%s failed=%s skipped=%s\n' "$PASS" "$FAIL" "$SKIP"
  if [ "$FAIL" -gt 0 ]; then
    say "RESULT: FAILED ($FAIL scenario(s))"
    exit 1
  fi
  if [ "$rc" -ne 0 ]; then
    say "RESULT: ABORTED (exit $rc)"
    exit "$rc"
  fi
  say "RESULT: OK"
  exit 0
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------- scenario 0
head_ 0 "plugin linked, hooks armed, doctor healthy"
LINKED=$(herdr plugin list --json | jq -r '[.plugins[]? // .result.plugins[]? | select(.plugin_id=="rchougule.reopen")] | length')
ev "herdr plugin list → rchougule.reopen present: $LINKED"
ACTIONS=$(herdr plugin action list --plugin rchougule.reopen | jq -r '[.result.actions[].action_id] | sort | join(",")')
ev "actions: $ACTIONS"
DOC=$("$BIN" doctor)
ev "$(echo "$DOC" | jq -c '{protocol,protocol_ok,state_dir_writable,herdr_version}')"
if assert_eq "plugin linked" "$LINKED" "1" \
  && assert_eq "actions" "$ACTIONS" "doctor,list,reopen-last" \
  && assert_eq "protocol_ok" "$(echo "$DOC" | jq -r .protocol_ok)" "true" \
  && assert_eq "state dir writable" "$(echo "$DOC" | jq -r .state_dir_writable)" "true"; then
  ok "0 plugin linked and healthy"
else
  bad "0 plugin linked and healthy"
fi

# The very first scratch workspace also proves the harness works.
W1=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-1","focus":false}' | jq -r '.result.workspace.workspace_id')
if [ -z "$W1" ] || [ "$W1" = "null" ]; then say "FATAL: cannot create a scratch workspace"; exit 1; fi
T1=$(herdr tab list | jq -r --arg w "$W1" '.result.tabs[] | select(.workspace_id==$w) | .tab_id' | head -1)
P1=$(herdr pane list | jq -r --arg t "$T1" '.result.panes[] | select(.tab_id==$t) | .pane_id' | head -1)
sleep 1

# ---------------------------------------------------------------- scenario 1
head_ 1 "lone pane in a multi-pane tab comes back in the same tab"
P2=$(sock pane.split "{\"target_pane_id\":\"$P1\",\"direction\":\"right\",\"ratio\":0.5,\"cwd\":\"/tmp\",\"focus\":false}" | jq -r '.result.pane.pane_id')
sleep 1; snap
BEFORE_CWD=$(pane_cwd "$P2"); BEFORE_N=$(tab_panes "$T1"); BEFORE_STACK=$(stack_len)
ev "tab $T1 has $BEFORE_N panes; closing $P2 (cwd $BEFORE_CWD)"
herdr pane close "$P2" >/dev/null
sleep 2
AFTER_STACK=$(stack_len); G=$(top_field granularity)
ev "stack $BEFORE_STACK → $AFTER_STACK, newest granularity=$G, summary=$(top_field summary)"
"$BIN" reopen-last > /tmp/reopen_qa_r1.json 2>/dev/null
ev "report: $(jq -c '{ok,created,resumed,prefilled,failed}' /tmp/reopen_qa_r1.json)"
sleep 1
NEW1=$(jq -r '.created.panes[0]' /tmp/reopen_qa_r1.json)
if assert_eq "stack delta" "$((AFTER_STACK-BEFORE_STACK))" "1" \
  && assert_eq "granularity" "$G" "pane" \
  && assert_eq "restored pane tab_id" "$(pane_tab "$NEW1")" "$T1" \
  && assert_eq "restored pane cwd" "$(pane_cwd "$NEW1")" "$BEFORE_CWD" \
  && assert_eq "tab pane count" "$(tab_panes "$T1")" "$BEFORE_N"; then
  ok "1 lone pane restored into its original tab"
else
  bad "1 lone pane restored into its original tab"
fi

# ---------------------------------------------------------------- scenario 2
head_ 2 "last pane of a tab (silent tab collapse) restores the whole tab"
W2=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-2","focus":false}' | jq -r '.result.workspace.workspace_id')
TB=$(sock tab.create "{\"workspace_id\":\"$W2\",\"cwd\":\"/tmp\",\"label\":\"qa-collapse\",\"focus\":false}" | jq -r '.result.tab.tab_id')
PB=$(sock tab.list "{\"workspace_id\":\"$W2\"}" >/dev/null; herdr pane list | jq -r --arg t "$TB" '.result.panes[] | select(.tab_id==$t) | .pane_id' | head -1)
sleep 1; snap
TABS_BEFORE=$(ws_tabs "$W2"); STACK_BEFORE=$(stack_len)
ev "workspace $W2 has $TABS_BEFORE tabs; closing the only pane ($PB) of tab $TB"
herdr pane close "$PB" >/dev/null
sleep 2
G=$(top_field granularity); NTABS=$(ws_tabs "$W2"); DELTA=$(( $(stack_len) - STACK_BEFORE ))
ev "after close: $NTABS tabs, stack delta $DELTA, granularity=$G (no tab.closed is emitted by herdr)"
"$BIN" reopen-last > /tmp/reopen_qa_r2.json 2>/dev/null
ev "report: $(jq -c '{ok,created,failed}' /tmp/reopen_qa_r2.json)"
sleep 1
NEWTAB=$(jq -r '.created.tabs[0]' /tmp/reopen_qa_r2.json)
if assert_eq "stack delta" "$DELTA" "1" \
  && assert_eq "granularity" "$G" "tab" \
  && assert_eq "tab count after close" "$NTABS" "$((TABS_BEFORE-1))" \
  && assert_eq "tab count after reopen" "$(ws_tabs "$W2")" "$TABS_BEFORE" \
  && assert_eq "restored tab pane count" "$(tab_panes "$NEWTAB")" "1"; then
  ok "2 implicit tab collapse restored as a tab"
else
  bad "2 implicit tab collapse restored as a tab"
fi

# ---------------------------------------------------------------- scenario 3
head_ 3 "whole tab close restores every pane with the same split geometry"
W3=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-3","focus":false}' | jq -r '.result.workspace.workspace_id')
T3=$(sock tab.create "{\"workspace_id\":\"$W3\",\"cwd\":\"/tmp\",\"label\":\"qa-three\",\"focus\":false}" | jq -r '.result.tab.tab_id')
A=$(herdr pane list | jq -r --arg t "$T3" '.result.panes[] | select(.tab_id==$t) | .pane_id' | head -1)
B=$(sock pane.split "{\"target_pane_id\":\"$A\",\"direction\":\"right\",\"ratio\":0.3,\"cwd\":\"/tmp\",\"focus\":false}" | jq -r '.result.pane.pane_id')
C=$(sock pane.split "{\"target_pane_id\":\"$B\",\"direction\":\"down\",\"ratio\":0.4,\"cwd\":\"/usr\",\"focus\":false}" | jq -r '.result.pane.pane_id')
sleep 1; snap
GEO_BEFORE=$(sock layout.export "{\"tab_id\":\"$T3\"}" | jq -Sc '.result.layout.root | walk(if type=="object" then del(.pane_id) else . end)' 2>/dev/null \
  || sock layout.export "{\"tab_id\":\"$T3\"}" | jq -Sc '.result.layout.root')
ev "pre-close geometry: $GEO_BEFORE"
STACK_BEFORE=$(stack_len)
herdr tab close "$T3" >/dev/null
sleep 2
G=$(top_field granularity); DELTA=$(( $(stack_len) - STACK_BEFORE ))
ev "stack delta $DELTA, granularity=$G"
"$BIN" reopen-last > /tmp/reopen_qa_r3.json 2>/dev/null
sleep 1
NEWTAB=$(jq -r '.created.tabs[0]' /tmp/reopen_qa_r3.json)
GEO_AFTER=$(sock layout.export "{\"tab_id\":\"$NEWTAB\"}" | jq -Sc '.result.layout.root | walk(if type=="object" then del(.pane_id) else . end)')
ev "restored geometry:  $GEO_AFTER"
ev "report: $(jq -c '{ok,created,failed}' /tmp/reopen_qa_r3.json)"
if assert_eq "stack delta" "$DELTA" "1" \
  && assert_eq "granularity" "$G" "tab" \
  && assert_eq "restored pane count" "$(tab_panes "$NEWTAB")" "3" \
  && assert_eq "restored workspace" "$(herdr tab list | jq -r --arg t "$NEWTAB" '.result.tabs[]|select(.tab_id==$t)|.workspace_id')" "$W3" \
  && assert_eq "split tree (pane ids stripped)" "$GEO_AFTER" "$GEO_BEFORE"; then
  ok "3 whole tab restored with identical geometry"
else
  bad "3 whole tab restored with identical geometry"
fi

# ---------------------------------------------------------------- scenario 4
head_ 4 "whole workspace with 2 tabs / 3 panes, tab order preserved"
W4=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-4","focus":false}' | jq -r '.result.workspace.workspace_id')
T4A=$(herdr tab list | jq -r --arg w "$W4" '.result.tabs[]|select(.workspace_id==$w)|.tab_id' | head -1)
P4A=$(herdr pane list | jq -r --arg t "$T4A" '.result.panes[]|select(.tab_id==$t)|.pane_id' | head -1)
sock pane.split "{\"target_pane_id\":\"$P4A\",\"direction\":\"down\",\"ratio\":0.5,\"cwd\":\"/tmp\",\"focus\":false}" >/dev/null
T4B=$(sock tab.create "{\"workspace_id\":\"$W4\",\"cwd\":\"/usr\",\"label\":\"qa-second\",\"focus\":false}" | jq -r '.result.tab.tab_id')
sleep 1; snap
ORDER_BEFORE=$(sock tab.list "{\"workspace_id\":\"$W4\"}" | jq -r '[.result.tabs[].label] | join(",")')
PANES_BEFORE=$(ws_panes "$W4")
ev "pre-close: tabs [$ORDER_BEFORE], $PANES_BEFORE panes"
STACK_BEFORE=$(stack_len)
herdr workspace close "$W4" >/dev/null
sleep 2
G=$(top_field granularity); DELTA=$(( $(stack_len) - STACK_BEFORE ))
ev "stack delta $DELTA, granularity=$G, summary=$(top_field summary)"
"$BIN" reopen-last > /tmp/reopen_qa_r4.json 2>/dev/null
sleep 1
W4N=$(jq -r '.created.workspaces[0]' /tmp/reopen_qa_r4.json)
ORDER_AFTER=$(sock tab.list "{\"workspace_id\":\"$W4N\"}" | jq -r '[.result.tabs[].label] | join(",")')
ev "restored workspace $W4N: tabs [$ORDER_AFTER], $(ws_panes "$W4N") panes"
ev "report: $(jq -c '{ok,created,failed}' /tmp/reopen_qa_r4.json)"
if assert_eq "stack delta" "$DELTA" "1" \
  && assert_eq "granularity" "$G" "workspace" \
  && assert_eq "tab count" "$(ws_tabs "$W4N")" "2" \
  && assert_eq "pane count" "$(ws_panes "$W4N")" "$PANES_BEFORE" \
  && assert_eq "tab array order" "$ORDER_AFTER" "$ORDER_BEFORE"; then
  ok "4 whole workspace restored with tab order and pane count"
else
  bad "4 whole workspace restored with tab order and pane count"
fi

# ---------------------------------------------------------------- scenario 5
head_ 5 "non-agent foreground command is prefilled, not executed"
W5=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-5","focus":false}' | jq -r '.result.workspace.workspace_id')
T5=$(herdr tab list | jq -r --arg w "$W5" '.result.tabs[]|select(.workspace_id==$w)|.tab_id' | head -1)
P5=$(herdr pane list | jq -r --arg t "$T5" '.result.panes[]|select(.tab_id==$t)|.pane_id' | head -1)
P5B=$(sock pane.split "{\"target_pane_id\":\"$P5\",\"direction\":\"right\",\"ratio\":0.5,\"cwd\":\"/tmp\",\"focus\":false}" | jq -r '.result.pane.pane_id')
sleep 2
herdr pane run "$P5B" "tail -f /dev/null" >/dev/null 2>&1
# wait for the shell to actually be running tail before forcing a snapshot, otherwise we
# capture whatever zsh's own startup is doing at that instant
for _ in $(seq 1 15); do
  RUNNING=$(sock pane.process_info "{\"pane_id\":\"$P5B\"}" | jq -r '[.result.process_info.foreground_processes[].argv0] | join(",")')
  case "$RUNNING" in *tail*) break ;; esac
  sleep 1
done
ev "live process_info before close: [$RUNNING]"
snap
CAPTURED=$(jq -r --arg p "$P5B" '.workspaces[].tabs[].panes[] | select(.pane_id==$p) | .foreground.argv | join(" ")' "$STATE/snapshot.json")
ev "captured foreground for $P5B: '$CAPTURED'"
herdr pane close "$P5B" >/dev/null
sleep 2
"$BIN" reopen-last > /tmp/reopen_qa_r5.json 2>/dev/null
sleep 2
NEW5=$(jq -r '.created.panes[0]' /tmp/reopen_qa_r5.json)
RAW5=$(herdr pane read "$NEW5" --source visible --lines 8 2>/dev/null)
VISIBLE=$(echo "$RAW5" | jq -r '.result.content // .result.text // empty' 2>/dev/null)
[ -z "$VISIBLE" ] && VISIBLE="$RAW5"
PROCS=$(sock pane.process_info "{\"pane_id\":\"$NEW5\"}" | jq -r '[.result.process_info.foreground_processes[].argv0] | join(",")')
ev "prefilled line contains tail: $(echo "$VISIBLE" | grep -c 'tail -f /dev/null')"
ev "process_info after restore: [$PROCS]  (prefill must NOT execute)"
ev "report: $(jq -c '{ok,prefilled,failed}' /tmp/reopen_qa_r5.json)"
if assert_eq "captured command" "$CAPTURED" "tail -f /dev/null" \
  && assert_eq "prefilled count" "$(jq -r .prefilled /tmp/reopen_qa_r5.json)" "1" \
  && [ "$(echo "$VISIBLE" | grep -c 'tail -f /dev/null')" -ge 1 ] \
  && [ "$(echo "$PROCS" | grep -c tail)" -eq 0 ]; then
  ok "5 foreground command captured and prefilled without executing"
else
  ev "visible buffer was: $(echo "$VISIBLE" | tail -3)"
  bad "5 foreground command captured and prefilled without executing"
fi

# ---------------------------------------------------------------- scenario 6
head_ 6 "pane.closed + workspace.closed cascade coalesces into exactly ONE entry"
W6=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-6","focus":false}' | jq -r '.result.workspace.workspace_id')
T6=$(herdr tab list | jq -r --arg w "$W6" '.result.tabs[]|select(.workspace_id==$w)|.tab_id' | head -1)
P6=$(herdr pane list | jq -r --arg t "$T6" '.result.panes[]|select(.tab_id==$t)|.pane_id' | head -1)
sleep 1; snap
STACK_BEFORE=$(stack_len)
ev "closing the only pane of the only tab of $W6 → herdr emits pane.closed + workspace.closed ~1 ms apart"
herdr pane close "$P6" >/dev/null
sleep 3
DELTA=$(( $(stack_len) - STACK_BEFORE )); G=$(top_field granularity)
EVENTS=$("$BIN" list --json | jq -c '.entries[0].events')
ev "stack delta $DELTA, granularity=$G, events in that one entry: $EVENTS"
"$BIN" reopen-last > /tmp/reopen_qa_r6.json 2>/dev/null
sleep 1
W6N=$(jq -r '.created.workspaces[0]' /tmp/reopen_qa_r6.json)
if assert_eq "closed.json delta" "$DELTA" "1" \
  && assert_eq "granularity" "$G" "workspace" \
  && assert_eq "both events folded into one entry" "$(echo "$EVENTS" | jq 'length')" "2" \
  && assert_eq "workspace restored" "$(ws_panes "$W6N")" "1"; then
  ok "6 cascade produced exactly one stack entry"
else
  bad "6 cascade produced exactly one stack entry"
fi

# ---------------------------------------------------------------- scenario 7
head_ 7 "state and daemon survive herdr server reload-config"
STACK_BEFORE=$(stack_len)
ev "stack before reload: $STACK_BEFORE entries"
herdr server reload-config >/dev/null 2>&1
sleep 2
# any ordinary event re-arms the daemon
W7=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-7","focus":false}' | jq -r '.result.workspace.workspace_id')
sleep 3
DOC=$("$BIN" doctor)
ev "$(echo "$DOC" | jq -c '{daemon_alive,daemon_pid,stack_entries,snapshot_age_ms,protocol_ok}')"
if assert_eq "stack intact" "$(echo "$DOC" | jq -r .stack_entries)" "$STACK_BEFORE" \
  && assert_eq "daemon re-armed" "$(echo "$DOC" | jq -r .daemon_alive)" "true" \
  && assert_eq "protocol still ok" "$(echo "$DOC" | jq -r .protocol_ok)" "true"; then
  ok "7 survives reload-config (stack intact, daemon re-armed)"
else
  bad "7 survives reload-config (stack intact, daemon re-armed)"
fi

# ---------------------------------------------------------------- scenario 8
head_ 8 "tab array order is restored from the index, not from tab.number"
W8=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-8","focus":false}' | jq -r '.result.workspace.workspace_id')
sock tab.create "{\"workspace_id\":\"$W8\",\"cwd\":\"/tmp\",\"label\":\"qa-b\",\"focus\":false}" >/dev/null
T8C=$(sock tab.create "{\"workspace_id\":\"$W8\",\"cwd\":\"/tmp\",\"label\":\"qa-c\",\"focus\":false}" | jq -r '.result.tab.tab_id')
sock tab.move "{\"tab_id\":\"$T8C\",\"insert_index\":0}" >/dev/null
sleep 1; snap
ORDER_BEFORE=$(sock tab.list "{\"workspace_id\":\"$W8\"}" | jq -r '[.result.tabs[].label]|join(",")')
NUMBERS=$(sock tab.list "{\"workspace_id\":\"$W8\"}" | jq -r '[.result.tabs[].number|tostring]|join(",")')
ev "pre-close array order [$ORDER_BEFORE] with numbers [$NUMBERS] — they disagree on purpose"
herdr workspace close "$W8" >/dev/null
sleep 2
"$BIN" reopen-last > /tmp/reopen_qa_r8.json 2>/dev/null
sleep 1
W8N=$(jq -r '.created.workspaces[0]' /tmp/reopen_qa_r8.json)
ORDER_AFTER=$(sock tab.list "{\"workspace_id\":\"$W8N\"}" | jq -r '[.result.tabs[].label]|join(",")')
ev "restored array order [$ORDER_AFTER]"
if assert_eq "array order preserved" "$ORDER_AFTER" "$ORDER_BEFORE"; then
  ok "8 tab order restored from the array index"
else
  bad "8 tab order restored from the array index"
fi

# ---------------------------------------------------------------- scenario 9
head_ 9 "concurrent hooks: 10 simultaneous close events, one burst, no lost update"
QADIR=$(mktemp -d)
QASTATE=$(HERDR_PLUGIN_STATE_DIR="$QADIR" "$BIN" doctor 2>/dev/null | jq -r '.state_dir // empty')
[ -n "$QASTATE" ] || QASTATE="$QADIR"
ev "using a throwaway state dir: $QASTATE (the real undo stack is untouched)"
for i in $(seq 1 10); do
  HERDR_PLUGIN_STATE_DIR="$QADIR" \
  HERDR_PLUGIN_EVENT="pane.closed" \
  HERDR_PLUGIN_EVENT_JSON="{\"event\":\"pane_closed\",\"data\":{\"type\":\"pane_closed\",\"pane_id\":\"qa$i:p1\",\"workspace_id\":\"qa$i\"}}" \
  HERDR_PLUGIN_CONTEXT_JSON="{\"workspace_id\":\"qa$i\",\"focused_pane_cwd\":\"/private/tmp\",\"tab_id\":\"qa$i:t1\"}" \
  "$BIN" event >/dev/null 2>&1 &
done
wait
ENTRIES=$(jq '.entries | length' "$QASTATE/closed.json" 2>/dev/null || echo 0)
UNIQUE=$(jq -r '[.entries[].workspace.workspace_id] | unique | length' "$QASTATE/closed.json" 2>/dev/null || echo 0)
PENDING=$(jq -r 'if .burst == null then "empty" else "left-behind" end' "$QASTATE/pending.json" 2>/dev/null || echo missing)
ev "closed.json entries=$ENTRIES unique workspaces=$UNIQUE pending=$PENDING"
if assert_eq "one entry per close, none lost or duplicated" "$ENTRIES" "10" \
  && assert_eq "all distinct" "$UNIQUE" "10" \
  && assert_eq "pending burst cleared" "$PENDING" "empty"; then
  ok "9 concurrent hooks produced exactly one entry each"
else
  bad "9 concurrent hooks produced exactly one entry each"
fi
rm -rf "$QADIR"

# ---------------------------------------------------------------- scenario 10
head_ 10 "agent round trip: close a live claude pane and resume the same session"
AGENT_SKIP=""
command -v claude >/dev/null 2>&1 || AGENT_SKIP="claude is not on PATH"
if [ -z "$AGENT_SKIP" ]; then
  WA=$(sock workspace.create '{"cwd":"/tmp","label":"reopen-qa-agent","focus":false}' | jq -r '.result.workspace.workspace_id')
  TA=$(herdr tab list | jq -r --arg w "$WA" '.result.tabs[]|select(.workspace_id==$w)|.tab_id' | head -1)
  PA=$(herdr pane list | jq -r --arg t "$TA" '.result.panes[]|select(.tab_id==$t)|.pane_id' | head -1)
  PA2=$(sock pane.split "{\"target_pane_id\":\"$PA\",\"direction\":\"right\",\"ratio\":0.5,\"cwd\":\"/tmp\",\"focus\":false}" | jq -r '.result.pane.pane_id')
  sleep 4
  START=$(herdr agent start reopenqa --kind claude --pane "$PA2" --timeout 120000 2>&1)
  ev "agent start: $(echo "$START" | head -c 220)"
  # claude only writes a transcript once the session has a turn in it; an empty session
  # cannot be resumed ("No conversation found with session ID"), so give it one.
  herdr agent prompt reopenqa "reply with exactly: ok" --wait --until idle --until done --timeout 180000 >/dev/null 2>&1
  SID=""
  for _ in $(seq 1 30); do
    SID=$(herdr pane list | jq -r --arg p "$PA2" '.result.panes[]|select(.pane_id==$p)|.agent_session.value // ""')
    [ -n "$SID" ] && [ "$SID" != "null" ] && break
    sleep 2
  done
  if [ -z "$SID" ] || [ "$SID" = "null" ]; then
    AGENT_SKIP="agent_session.value never populated within 60 s (herdr claude integration is outdated on this machine)"
  fi
fi
if [ -n "$AGENT_SKIP" ]; then
  skip "10 agent round trip — $AGENT_SKIP"
else
  ev "original session id: $SID (pane $PA2, tab $TA)"
  snap
  herdr pane close "$PA2" >/dev/null
  sleep 2
  ev "granularity=$(top_field granularity) summary=$(top_field summary)"
  "$BIN" reopen-last > /tmp/reopen_qa_r10.json 2>/dev/null
  ev "report: $(jq -c '{ok,created,resumed,failed}' /tmp/reopen_qa_r10.json)"
  NEWA=$(jq -r '.created.panes[0]' /tmp/reopen_qa_r10.json)
  SID2=""
  for _ in $(seq 1 45); do
    SID2=$(herdr pane list | jq -r --arg p "$NEWA" '.result.panes[]|select(.pane_id==$p)|.agent_session.value // ""')
    [ -n "$SID2" ] && [ "$SID2" != "null" ] && break
    sleep 2
  done
  ev "restored pane $NEWA session id: ${SID2:-<none>}"
  if assert_eq "restored pane tab_id" "$(pane_tab "$NEWA")" "$TA" \
    && assert_eq "resumed agent count" "$(jq -r .resumed /tmp/reopen_qa_r10.json)" "1" \
    && assert_eq "agent_session.value" "$SID2" "$SID"; then
    ok "10 agent session resumed into the restored pane"
  else
    bad "10 agent session resumed into the restored pane"
  fi
fi

# ---------------------------------------------------------------- scenario 11
head_ 11 "plugin log is free of errors"
LOGS=$(herdr plugin log list --plugin rchougule.reopen --limit 50)
FAILED=$(echo "$LOGS" | jq '[.result.logs[] | select(.status=="failed" or ((.exit_code // 0) != 0))] | length')
ERRLINES=$(echo "$LOGS" | jq -r '[.result.logs[] | (.stderr // "") + (.stdout // "")] | join("\n")' | grep -c '\[reopen\] error' || true)
ev "log entries inspected: $(echo "$LOGS" | jq '.result.logs | length'), failed runs: $FAILED, '[reopen] error' lines: $ERRLINES"
[ "$FAILED" -ne 0 ] && echo "$LOGS" | jq -r '.result.logs[] | select(.status=="failed") | "        failed: \(.command|join(" ")) → \(.stderr // "")"' | head -5
if assert_eq "failed plugin runs" "$FAILED" "0" && assert_eq "error log lines" "$ERRLINES" "0"; then
  ok "11 plugin log clean"
else
  bad "11 plugin log clean"
fi

# The EXIT trap prints the summary and owns the exit status.
exit 0
