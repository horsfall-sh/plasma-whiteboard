#!/bin/sh
# Install or update the relay on the server. Usage: deploy/deploy.sh root@host
# Build first with `make linux`. The TLS proxy (deploy/HOST.md) is set up once;
# this script never touches it.
set -eu
HOST="$1"
BIN=relay/target-linux/release/wb-relay
[ -x "$BIN" ] || { echo "build first: make linux" >&2; exit 1; }
scp "$BIN" "$HOST:/usr/local/bin/wb-relay.new"
scp deploy/whiteboard.service "$HOST:/etc/systemd/system/whiteboard.service"
ssh "$HOST" 'set -e
  chmod 755 /usr/local/bin/wb-relay.new
  mv /usr/local/bin/wb-relay.new /usr/local/bin/wb-relay
  command -v restorecon >/dev/null && restorecon /usr/local/bin/wb-relay /etc/systemd/system/whiteboard.service
  systemctl daemon-reload
  systemctl enable whiteboard >/dev/null 2>&1
  systemctl restart whiteboard
  sleep 1
  systemctl is-active whiteboard
  curl -fsS http://127.0.0.1:8787/healthz && echo'
