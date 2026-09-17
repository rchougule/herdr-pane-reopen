#!/usr/bin/env bash
# Build and link the working tree as a local plugin. `herdr plugin link` never runs
# [[build]], so the binary must exist first.
#
# By default this links into the isolated `reopen-qa` session (HERDR_SESSION), because
# live development closes panes. Note that herdr's plugin REGISTRY IS GLOBAL: linking
# from any session enables the plugin everywhere, so keep `allowed_sockets` in the
# plugin's config.toml pointed at the sessions you actually want it in.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
HERDR_SESSION="${HERDR_SESSION:-reopen-qa}"

cargo build --release
herdr --session "$HERDR_SESSION" plugin link "$root" >/dev/null
echo "linked into session '$HERDR_SESSION':"
herdr --session "$HERDR_SESSION" plugin list | grep -i reopen || true
cat <<MSG

reminder: the plugin registry is global. To keep this plugin inert outside the QA
session, put its socket in the allow-list:

  \$(herdr plugin config-dir rchougule.reopen)/config.toml
  allowed_sockets = ["\$HOME/.config/herdr/sessions/$HERDR_SESSION/herdr.sock"]
MSG
