#!/bin/sh
# Install the pre-commit scrub check into this clone.
cd "$(git rev-parse --show-toplevel)" || exit 1
printf '#!/bin/sh\nexec sh scripts/scrub-check.sh\n' > .git/hooks/pre-commit
chmod +x .git/hooks/pre-commit scripts/scrub-check.sh
echo "installed .git/hooks/pre-commit"
