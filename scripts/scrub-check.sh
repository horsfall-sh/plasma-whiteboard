#!/bin/sh
# Scan the staged diff for things that must not go into a public repo. Exits non-zero on any hit.
# Extra patterns (one extended regex per line) go in the gitignored .scrub-patterns.local.
cd "$(git rev-parse --show-toplevel)" || exit 2
diff=$(git diff --cached -U0 --no-color | grep -E '^\+' | grep -vE '^\+\+\+ ')
fail=0
hit() { echo "scrub-check: $1" >&2; fail=1; }
# IPv4 addresses other than 127.0.0.1 and 0.0.0.0 (version-like and example values are not special-cased)
printf '%s\n' "$diff" | grep -oE '([0-9]{1,3}\.){3}[0-9]{1,3}' | grep -vxE '127\.0\.0\.1|0\.0\.0\.0' | grep -q . && hit "IPv4 address in staged changes"
printf '%s\n' "$diff" | grep -qiE '@akamai\.com' && hit "employer email address"
printf '%s\n' "$diff" | grep -qE 'BEGIN [A-Z ]*PRIVATE KEY' && hit "private key block"
printf '%s\n' "$diff" | grep -qE 'ssh-(ed25519|rsa) [A-Za-z0-9+/=]{20,}' && hit "ssh key body"
if [ -f .scrub-patterns.local ]; then
    grep -vE '^[[:space:]]*(#|$)' .scrub-patterns.local > "${TMPDIR:-/tmp}/scrub-pats.$$"
    if [ -s "${TMPDIR:-/tmp}/scrub-pats.$$" ] && printf '%s\n' "$diff" | grep -qE -f "${TMPDIR:-/tmp}/scrub-pats.$$"; then
        hit "matches a pattern from .scrub-patterns.local"
    fi
    rm -f "${TMPDIR:-/tmp}/scrub-pats.$$"
fi
[ "$fail" = 0 ] && echo "scrub-check: clean"
exit $fail
