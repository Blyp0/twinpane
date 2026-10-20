#!/bin/sh
# Build a practice folder, start the browser preview on it, run the UI tests, stop the preview.
# Needs: cargo, node/npm (npm install once in this folder), python3 + pillow, and a Chrome/Chromium
# (set CHROME=/path/to/chrome).
set -e
cd "$(dirname "$0")"
SANDBOX="${SANDBOX:-/tmp/tp-sandbox}"; PORT="${PORT:-8731}"
rm -rf "/tmp/twinpane-preview-$PORT"   # forget undo history from earlier runs
python3 make_sandbox.py "$SANDBOX"
cargo build -q -p twinpane-dev
../target/debug/twinpane-dev --root "$SANDBOX" --port "$PORT" > /tmp/twinpane-dev.log 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null' EXIT
sleep 1.5
SANDBOX="$SANDBOX" URL="http://127.0.0.1:$PORT/" node ui.test.mjs
