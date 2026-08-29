#!/bin/bash
# ---------------------------------------------------------------------------
# One-command test-coverage report for the Lua application host (host/lua-app,
# crate `magent-lua`), using `cargo-llvm-cov`.
#
# Reports line/function coverage for BOTH engines (the default `mlua` engine and
# the pure-Rust `piccolo` engine the ESP32-S3 firmware runs). This is the "how
# well are the Lua host's production paths tested?" signal.
#
# Usage:  scripts/check-coverage.sh
# Requires: cargo-llvm-cov (cargo install cargo-llvm-cov) + rustup llvm-tools.
# ---------------------------------------------------------------------------
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CRATE="magent-lua"

echo "==> [$CRATE] coverage (mlua engine)"
cargo llvm-cov -p "$CRATE" --summary-only 2>/dev/null || \
    cargo llvm-cov -p "$CRATE"

echo ""
echo "==> [$CRATE] coverage (mlua + piccolo engines — the S3 firmware engine)"
cargo llvm-cov -p "$CRATE" --features piccolo --summary-only 2>/dev/null || \
    cargo llvm-cov -p "$CRATE" --features piccolo
