#!/bin/bash
# ---------------------------------------------------------------------------
# Encode a Lua file as a sequence of `AT+LUAAPP` commands so an operator can
# update the device's `main.lua` over the UART ingress without reflashing the
# firmware (firmware feature `AT+LUAAPP`, ESP32-S3 only).
#
# The AT parser caps a line at 256 bytes, so a real app cannot be sent in one
# command. This script emits:
#
#   AT+LUAAPP=<b64 of chunk 0>      # replaces the stored app
#   AT+LUAAPP=+<b64 of chunk 1>     # appends
#   AT+LUAAPP=+<b64 of chunk 2>     # appends
#   ...
#
# Send them in order. The firmware decodes each chunk as UTF-8 and appends to
# the stored source, so chunks are split on character boundaries (splitting a
# multi-byte character would be rejected) and the file must be valid UTF-8.
#
# Usage:
#   scripts/luaapp-encode.sh path/to/main.lua
#   scripts/luaapp-encode.sh path/to/main.lua > upload.txt
# ---------------------------------------------------------------------------
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 path/to/main.lua" >&2
    exit 2
fi

F="$1"
[ -f "$F" ] || { echo "not a file: $F" >&2; exit 2; }

# 150 source bytes -> 200 base64 chars; with the longest 11-byte prefix
# (`AT+LUAAPP=+`) the line stays at 211 bytes, below the parser's 256 cap.
CHUNK_BYTES=150
MAX_APP_BYTES=16384 # keep in step with `lua_task::MAX_LUA_APP_BYTES`

if ! command -v python3 >/dev/null 2>&1; then
    echo "error: python3 is required (UTF-8-safe chunking)" >&2
    exit 1
fi

python3 - "$F" "$CHUNK_BYTES" "$MAX_APP_BYTES" <<'PY'
import base64
import sys

path, chunk_bytes, max_bytes = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
data = open(path, "rb").read()
if not data:
    sys.exit(f"error: {path} is empty (refusing to emit a clear command)")
if len(data) > max_bytes:
    sys.exit(f"error: {path} is {len(data)} bytes (max {max_bytes})")
try:
    text = data.decode("utf-8")
except UnicodeDecodeError as e:
    sys.exit(f"error: {path} is not valid UTF-8: {e}")

chunks = []
current = []
current_bytes = 0
for ch in text:
    n = len(ch.encode("utf-8"))
    if current_bytes + n > chunk_bytes and current:
        chunks.append("".join(current))
        current = []
        current_bytes = 0
    current.append(ch)
    current_bytes += n
if current:
    chunks.append("".join(current))
if not chunks:
    chunks = [""]

for i, chunk in enumerate(chunks):
    b64 = base64.urlsafe_b64encode(chunk.encode("utf-8")).rstrip(b"=").decode()
    prefix = "AT+LUAAPP=" if i == 0 else "AT+LUAAPP=+"
    print(prefix + b64)

print(f"# {len(chunks)} chunk(s), {len(data)} bytes", file=sys.stderr)
PY
