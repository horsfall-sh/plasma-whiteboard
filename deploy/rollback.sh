#!/bin/sh
# Put the previous relay back by hand. Usage: deploy/rollback.sh root@host
# deploy.sh keeps the last binary and service file as .prev; this restores
# them and restarts. Run it once: a second run restores the same files again.
set -eu
HOST="$1"
ssh "$HOST" 'set -eu
  B=/usr/local/bin/wb-relay
  S=/etc/systemd/system/whiteboard.service
  [ -e $B.prev ] || { echo "no previous relay on this server" >&2; exit 1; }
  cp -p $B.prev $B
  [ -e $S.prev ] && cp -p $S.prev $S
  command -v restorecon >/dev/null && restorecon $B $S
  systemctl daemon-reload
  systemctl restart whiteboard
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    if systemctl is-active --quiet whiteboard && curl -fsS -o /dev/null http://127.0.0.1:8787/healthz; then
      echo "rolled back: the previous relay is healthy"
      exit 0
    fi
  done
  echo "rolled back, but the relay is not healthy" >&2
  exit 1'
