#!/usr/bin/env python3
"""T4 — TLS handshake soak harness for the mAgent ESP32 firmware.

Repeatedly drives `AT+HTTPGET=<https-url>` over the ingress UART and watches
for TLS memory errors / hangs / panics. This is the acceptance gate for
REQ-NET-001 (docs/TIER6_VALIDATION.md): repeated verified HTTPS handshakes
with no memory error and no hang.

IMPORTANT — physical UART0 required: the firmware's ingress gateway binds
UART0 (the physical GPIO TX/RX pins, e.g. GPIO43/44 on the ESP32-S3), NOT the
native USB-serial console. Connect a USB-UART adapter to those pins and pass
its device here. Sending on the USB console will NOT reach the ingress.

Usage:
    python3 tools/s3_tls_soak.py <PORT> [--iterations N] [--url URL]
                                  [--interval S] [--timeout S]

Exit code 0 = all handshakes succeeded; 1 = failures/crashes observed.
"""
import argparse
import re
import sys
import time

import serial

# A public TLS endpoint (DeepSeek). Overridable with --url.
DEFAULT_URL = "https://api.deepseek.com"
# Markers that indicate a crash / watchdog / memory failure.
CRASH = ("Guru", "Prohibited", "Double exception", "stack overflow",
         "ESP_ERR_NO_MEM", "failed to allocate", "assert failed")
# Markers that indicate a successful HTTP/AT round-trip.
SUCCESS = ("+HTTPGET:", "HTTP/", "status code", "OK", "code=")
# A marker that the AT line was at least accepted by the ingress.
DISPATCH = ("httpget", "HTTPGET", "[at]")
BAUD = 115200


def read_until_quiet(s: serial.Serial, quiet_s: float, max_s: float) -> bytes:
    """Read until `quiet_s` of silence or `max_s` elapses; return bytes."""
    buf = b""
    end = time.time() + max_s
    while time.time() < end:
        d = s.read(8192)
        if d:
            buf += d
            # reset the quiet window on any traffic
            end = max(end, time.time() + quiet_s)
    return buf


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("port", help="serial device for physical UART0")
    ap.add_argument("--iterations", type=int, default=5)
    ap.add_argument("--url", default=DEFAULT_URL)
    ap.add_argument("--interval", type=float, default=2.0, help="s between sends")
    ap.add_argument("--timeout", type=float, default=15.0, help="per-iter read timeout s")
    args = ap.parse_args()

    s = serial.Serial(args.port, BAUD, timeout=0.2)
    time.sleep(0.5)
    s.reset_input_buffer()
    print(f"[T4] soak start: {args.iterations}x AT+HTTPGET={args.url}")

    fails = 0
    for i in range(1, args.iterations + 1):
        # Drain any residual console chatter before firing.
        s.reset_input_buffer()
        s.write(f"AT+HTTPGET={args.url}\r\n".encode())
        s.flush()
        raw = read_until_quiet(s, quiet_s=1.0, max_s=args.timeout)
        txt = raw.decode("utf-8", "replace")
        crash = [c for c in CRASH if c in txt]
        ok = [m for m in SUCCESS if m in txt]
        dispatched = any(m in txt.lower() for m in DISPATCH)
        status = "OK" if ok and not crash else ("CRASH" if crash else "no-reply")
        if crash or not ok:
            fails += 1
        print(f"  [{i}/{args.iterations}] {status}  "
              f"(dispatch={dispatched} crash={crash or '-'} ok={ok or '-'})")
        time.sleep(args.interval)

    s.close()
    print(f"[T4] done: {args.iterations - fails}/{args.iterations} ok")
    return 0 if fails == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
