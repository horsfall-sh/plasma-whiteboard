# Instructions for Claude sessions in this repo

This repository is PUBLIC. Anything committed here is visible to everyone and stays in history.

Never commit: real host IPs, instance or firewall ids, real hostnames or domains, ssh targets, personal or employer
email addresses, LAN or home network details, secret paths or values, keys or tokens.

- Real deploy details go only in gitignored files: `deploy/*.local.md`, `.scrub-patterns.local`, `.env*`.
- Use the `HOST` and `DOMAIN` variables and `example.com` placeholders in anything tracked.
- Commit identity must be the owner's public identity: check `git config user.email` before committing.
- Run `sh scripts/install-hooks.sh` once in each clone (git hooks are not pushed). It installs a pre-commit hook
  that runs `scripts/scrub-check.sh`.
- Before every commit run `sh scripts/scrub-check.sh` (it checks the staged diff). Put the real IP, ids and domain
  in the untracked `.scrub-patterns.local` (one extended regex per line) so the check catches them.
