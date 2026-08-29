#!/bin/bash
# ---------------------------------------------------------------------------
# One-command regression gate for the host-validated Lua application host
# (`host/lua-app`, crate `magent-lua`).
#
# Runs: clippy (-D warnings), the full test suite (both the default `mlua` engine
# and the pure-Rust `piccolo` engine the S3 firmware uses), a pure-piccolo lib
# build (proves the no-`mlua` path compiles), the developer CLI, and a
# best-effort fmt check. This is the "is the Lua host still green?" signal
# before flashing S3 firmware.
#
# Usage:  scripts/check-lua.sh
# ---------------------------------------------------------------------------
set -euo pipefail

# Resolve the workspace root regardless of where the script is invoked from.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CRATE="magent-lua"

echo "==> [$CRATE] clippy (all targets, deny warnings)"
cargo clippy -p "$CRATE" --all-targets -- -D warnings

echo "==> [$CRATE] tests (${CRATE}-only)"
cargo test -p "$CRATE"

echo "==> [$CRATE] build developer CLI (bin lua-run)"
cargo build -p "$CRATE" --bin lua-run

# The S3 firmware uses the pure-Rust `piccolo` engine (no `mlua`/vendored C),
# so both engine configurations must stay green.
echo "==> [$CRATE] pure-piccolo build (no mlua; S3 firmware engine)"
cargo build -p "$CRATE" --no-default-features --features piccolo --lib

echo "==> [$CRATE] tests (mlua + piccolo engines)"
cargo test -p "$CRATE" --features piccolo

echo "==> [$CRATE] fmt check (best-effort)"
# The workspace has pre-existing formatting drift, so this is informational
# and non-blocking (mirrors the CI `host` job's stance).
if cargo fmt -p "$CRATE" -- --check; then
    echo "    fmt: clean"
else
    echo "    note: fmt drift in $CRATE (non-blocking)"
fi

echo "==> $CRATE: all checks green"
