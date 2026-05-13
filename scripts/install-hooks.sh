#!/bin/sh
set -e

HOOKS_DIR="$(git rev-parse --git-dir)/hooks"

cat > "$HOOKS_DIR/pre-push" << 'EOF'
#!/bin/sh
set -e

echo "Running pre-push checks..."

cargo fmt --all --check
cargo clippy --workspace -- -D warnings -A clippy::too_many_arguments
cargo test

echo "All checks passed."
EOF

chmod +x "$HOOKS_DIR/pre-push"
echo "Installed pre-push hook."
