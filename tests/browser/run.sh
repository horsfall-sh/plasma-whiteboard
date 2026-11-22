#!/bin/bash
# Run one browser check against a local relay on a throwaway database.
#   tests/browser/run.sh smoke|panel|h2|g6
# Needs node with playwright-core (set NODE_PATH to its node_modules if it is not
# installed here) and Playwright's Chromium and Firefox. The relay serves web/ from
# disk, so run `make wasm` first. WB_TEST_PORT picks the port (default 8799).
set -u
name=${1:?usage: run.sh smoke|panel|h2|g6}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
test -f "$here/$name.mjs" || { echo "no such check: $name" >&2; exit 2; }
port=${WB_TEST_PORT:-8799}
tmp=$(mktemp -d) || exit 2
trap 'kill "${rp:-}" 2>/dev/null; wait "${rp:-}" 2>/dev/null; rm -rf -- "$tmp"' EXIT
(cd "$root/relay" && cargo build -q --release) || exit 2
(cd "$root/relay" && WB_ADDR=127.0.0.1:$port WB_DB="$tmp/wb.db" WB_WEB_DIR=../web \
  exec ./target/release/wb-relay) > "$tmp/relay.log" 2>&1 &
rp=$!
for _ in $(seq 1 50); do curl -fsS -o /dev/null "http://127.0.0.1:$port/healthz" 2>/dev/null && break; sleep 0.2; done
node "$here/$name.mjs" "http://127.0.0.1:$port"
rc=$?
test $rc = 0 || { echo "relay log:"; tail -20 "$tmp/relay.log"; }
exit $rc
