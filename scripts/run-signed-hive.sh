#!/bin/sh

set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
PROFILE="${HIVE_PROFILE:-debug}"

case "$PROFILE" in
  release)
    BUILD_ARGS="--release"
    BINARY_PATH="$TARGET_DIR/release/hive"
    ;;
  debug)
    BUILD_ARGS=""
    BINARY_PATH="$TARGET_DIR/debug/hive"
    ;;
  *)
    echo "Unsupported HIVE_PROFILE: $PROFILE" >&2
    echo "Use HIVE_PROFILE=debug or HIVE_PROFILE=release." >&2
    exit 1
    ;;
esac

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required" >&2
  exit 1
fi

if ! command -v codesign >/dev/null 2>&1; then
  echo "codesign is required on macOS to run the signed dev launcher" >&2
  exit 1
fi

cd "$ROOT_DIR"

echo "Building Hive ($PROFILE)..."
if [ -n "$BUILD_ARGS" ]; then
  cargo build $BUILD_ARGS
else
  cargo build
fi

echo "Signing $BINARY_PATH with ad hoc signature..."
/usr/bin/codesign --force --sign - "$BINARY_PATH"

echo "Launching signed Hive..."
exec "$BINARY_PATH" "$@"
