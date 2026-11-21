#!/bin/sh
# Obtain the Let's Encrypt certificate for whiteboard.example.com and turn on
# the nginx 443 site. Run once DNS points at this host. Safe to run again:
# certbot keeps an existing valid cert, the TLS site is copied only if changed.
set -eu
D=whiteboard.example.com
STAGED=/etc/nginx/whiteboard-tls.conf.staged
LIVE=/etc/nginx/conf.d/whiteboard-tls.conf

# 1. DNS must point here, or HTTP-01 fails (and burns LE rate limit).
me4=$(ip -4 -o addr show dev eth0 scope global | awk '{print $4}' | cut -d/ -f1 | head -1)
dns4=$(getent ahostsv4 "$D" | awk 'NR==1{print $1}')
if [ "$dns4" != "$me4" ] && [ "${FORCE:-0}" != 1 ]; then
    echo "DNS for $D is '$dns4', this host is '$me4'. Move DNS first (or FORCE=1)." >&2
    exit 1
fi
# Let's Encrypt prefers IPv6: an AAAA record, if any, must point here too.
me6=$(ip -6 -o addr show dev eth0 scope global | awk '{print $4}' | cut -d/ -f1 | head -1)
dns6=$(getent ahostsv6 "$D" | awk '$1 !~ /^::ffff:/ {print $1; exit}')
if [ -n "$dns6" ] && [ "$dns6" != "$me6" ] && [ "${FORCE:-0}" != 1 ]; then
    echo "AAAA for $D is '$dns6', this host is '$me6'. Fix or remove the AAAA record first." >&2
    exit 1
fi

# 2. Port 80 site must be up to answer the challenge.
nginx -t
systemctl is-active --quiet nginx || systemctl start nginx

# 3. Certificate (ECDSA P-256), webroot challenge; reload nginx on renewals.
certbot certonly --webroot -w /var/www/acme -d "$D" \
    --key-type ecdsa --elliptic-curve secp256r1 \
    --non-interactive --agree-tos --register-unsafely-without-email \
    --keep-until-expiring \
    --deploy-hook 'systemctl reload nginx'

# 4. Enable the 443 site.
if ! cmp -s "$STAGED" "$LIVE"; then
    install -m 644 -o root -g root "$STAGED" "$LIVE"
    restorecon "$LIVE"
fi
nginx -t
systemctl reload nginx

# 5. Renewal timer and a renewal rehearsal.
systemctl enable --now certbot-renew.timer
certbot renew --dry-run

curl -fsS -o /dev/null -w "https://$D -> %{http_code}\n" --resolve "$D:443:127.0.0.1" "https://$D/healthz" || \
    echo "TLS is up; /healthz failed (is wb-relay deployed and running?)" >&2
