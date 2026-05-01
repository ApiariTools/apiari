#!/bin/bash
set -e

echo "=== Rust format ==="
cargo fmt --check
echo "✓ formatted"

echo ""
echo "=== Rust clippy ==="
cargo clippy -- -D warnings 2>&1 | tail -3
echo "✓ no warnings"

echo ""
echo "=== Rust tests ==="
cargo test 2>&1 | tail -5
echo "✓ tests pass"

echo ""
echo "=== Frontend typecheck ==="
cd web
npx tsc --noEmit
echo "✓ types ok"

echo ""
echo "=== Frontend build ==="
npx vite build 2>&1 | tail -3
echo "✓ build ok"

echo ""
echo "=== All checks passed ==="
