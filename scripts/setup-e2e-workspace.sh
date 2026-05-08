#!/usr/bin/env bash
# Creates the "apiari" workspace config required by worker-lifecycle e2e tests.
# Run this once before `npx playwright test --config playwright.e2e.config.ts`.
# Safe in CI (fresh env). On dev machines, only run if you want to clobber the
# apiari workspace config.
set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG_DIR="${HOME}/.config/apiari"

mkdir -p "$CONFIG_DIR/workspaces"

cat > "$CONFIG_DIR/workspaces/apiari.toml" << EOF
[workspace]
name = "apiari"
root = "$REPO_ROOT"
EOF

echo "E2E workspace config written:"
echo "  $CONFIG_DIR/workspaces/apiari.toml → root = $REPO_ROOT"
