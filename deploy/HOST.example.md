# Host notes (template)

Copy this to `deploy/HOST.local.md` (gitignored) and fill in the real values there. Never put real values in this file.

## Host
- Provider / instance: `<provider>`, id `<instance-id>`
- OS: Oracle Linux 9 (FIPS mode), SELinux enforcing
- Address: `<HOST_IP>`; ssh target `root@<HOST_IP>` (the `HOST` make variable)
- Services: nginx (80/443) in front of `wb-relay` on 127.0.0.1:8787, systemd unit `whiteboard`

## Firewall
- Cloud firewall id: `<firewall-id>`; allow 22 (your admin address only), 80, 443. Nothing else.
- Host firewall: same ports.

## Key
- ssh key used: `<path-to-private-key>` (keep it out of the repo)

## Certificate
- Domain: `whiteboard.example.com` (the `DOMAIN` make variable); DNS A record (and no stray AAAA) points at `<HOST_IP>`
- Issue with `DOMAIN=<domain> sh deploy/issue-cert.sh` (Let's Encrypt, certbot, renewal timer)

## Console
- Provider console / recovery access: `<how to reach it>`
