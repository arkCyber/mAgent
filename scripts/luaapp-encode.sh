#!/bin/bash
# ---------------------------------------------------------------------------
# Encode a Lua file as an `AT+LUAAPP=<url-safe-base64>` command so an operator
# can update the device's `main.lua` over the UART ingress without reflashing
# the firmware (firmware feature `AT+LUAAPP`).
#
# Usage:
#   scripts/luaapp-encode.sh path/to/main.lua
#   # prints: AT+LUAAPP=<url-safe-base64-of-the-file>
#   # paste that line into the device console (or BLE AT bridge).
# ---------------------------------------------------------------------------
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 path/to/main.lua" >&2
    exit 2
fi

F="$1"
[ -f "$F" ] || { echo "not a file: $F" >&2; exit 2; }

# URL-safe base64 without padding (no '+' or '/'), matching the firmware's
# URL_SAFE_NO_PAD decoder. Cross-platform: prefer python3, fall back to base64.
if command -v python3 >/dev/null 2>&1; then
    B64=$(python3 -c 'import base64,sys; print(base64.urlsafe_b64encode(open(sys.argv[1],"rb").read()).rstrip(b"=").decode())' "$F")
else
    B64=$(base64 -w0 "$F" | tr '+/' '-_' | tr -d '=')
fi

echo "AT+LUAAPP=$B64"
