#!/bin/sh
# Install or update the relay on the server. Usage: deploy/deploy.sh root@host
# Build first with `make linux`. The TLS proxy (see deploy/nginx-whiteboard.conf or deploy/Caddyfile.site) is set up once;
# this script never touches it.
#
# The running binary and service file are kept as wb-relay.prev and
# whiteboard.service.prev. If the new relay does not answer /healthz within
# about ten seconds, both are put back, the old relay is restarted and the
# script exits 1. deploy/rollback.sh puts them back by hand.
set -eu
HOST="$1"
BIN=relay/target-linux/release/wb-relay
[ -x "$BIN" ] || { echo "build first: make linux" >&2; exit 1; }
scp "$BIN" "$HOST:/usr/local/bin/wb-relay.new"
scp deploy/whiteboard.service "$HOST:/etc/systemd/system/whiteboard.service.new"
ssh "$HOST" 'set -eu
  B=/usr/local/bin/wb-relay
  S=/etc/systemd/system/whiteboard.service
  healthy() {
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      sleep 1
      systemctl is-active --quiet whiteboard && curl -fsS -o /dev/null http://127.0.0.1:8787/healthz && return 0
    done
    return 1
  }
  chmod 755 $B.new
  [ -e $B ] && cp -p $B $B.prev
  [ -e $S ] && cp -p $S $S.prev
  mv $B.new $B
  mv $S.new $S
  command -v restorecon >/dev/null && restorecon $B $S
  systemctl daemon-reload
  systemctl enable whiteboard >/dev/null 2>&1
  systemctl restart whiteboard
  if healthy; then
    echo "live: the new relay is healthy"
    exit 0
  fi
  echo "the new relay is not healthy: rolling back" >&2
  journalctl -u whiteboard -n 20 --no-pager >&2 || true
  [ -e $B.prev ] || { echo "no previous relay to roll back to" >&2; exit 1; }
  cp -p $B.prev $B
  [ -e $S.prev ] && cp -p $S.prev $S
  command -v restorecon >/dev/null && restorecon $B $S
  systemctl daemon-reload
  systemctl restart whiteboard
  if healthy; then
    echo "rolled back: the previous relay is running" >&2
  else
    echo "rolled back, but the previous relay is not healthy either" >&2
  fi
  exit 1'
