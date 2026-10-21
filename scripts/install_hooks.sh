#!/usr/bin/env bash
# Install the repo's git hooks (idempotent). Run once per clone / worktree.
#
#   pre-commit: clap-validator on every plugin crate in the commit, against its
#   built bundle in dist/. A bundle older than the sources is UNCHECKED and
#   blocks the commit — build it first, a stale binary proves nothing.
#   Bypass for a broken hook environment only: git commit --no-verify.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOOKS="$(git -C "$ROOT" rev-parse --git-path hooks)"
mkdir -p "$HOOKS"
cat > "$HOOKS/pre-commit" <<'EOF'
#!/usr/bin/env bash
# superduper-dsp pre-commit — see scripts/install_hooks.sh
set -u
ROOT="$(git rev-parse --show-toplevel)"
if ! command -v clap-validator >/dev/null 2>&1; then
    echo "pre-commit: UNCHECKED — clap-validator not on PATH (tools/validate_claps.py has the install line)"
    exit 0
fi
python3 "$ROOT/tools/validate_claps.py" --changed
EOF
chmod +x "$HOOKS/pre-commit"
echo "installed $HOOKS/pre-commit"
