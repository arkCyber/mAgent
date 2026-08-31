#!/usr/bin/env python3
"""BLE + AT functional self-test for the mAgent ESP32-S3 firmware.

Talks to the S3 over its native USB-Serial/JTAG console (`/dev/cu.usbmodem*`),
waits for boot (BLE auto-advertising), then exercises the `AT+BLE` control
surface end-to-end:

  * `AT`            -> `OK`
  * `AT+BLE?`       -> `+BLE:<state>` (query)
  * `AT+BLE=STATE`  -> `+BLE:<state>`
  * `AT+BLE=OFF`    -> `+BLE:idle`
  * `AT+BLE=ON`     -> `OK`, then advertising resumes
  * `AT+BLE?`       -> `+BLE:advertising` (confirm restart)

Usage:
    python3 tools/s3_ble_test.py [PORT] [--baud 115200]

The firmware log lines and AT replies share the same console; this script
extracts replies by looking for the expected reply markers and key boot log
lines. Requires `pyserial`.
"""
import sys
import time

try:
    import serial
except ImportError:
    print("[FATAL] pyserial not installed: pip3 install pyserial")
    sys.exit(2)

PORT = sys.argv[1] if len(sys.argv) > 1 else "/dev/cu.usbmodem101"
BAUD = 115200
BOOT_WAIT_S = 60          # firmware build can take a moment to boot
REPLY_S = 3.0
BOOT_MARKERS = [
    b"BLE ADVERTISING ACTIVE",
    b"start_advertising() succeeded",
    b"GATT service 0x1850 started",
]

passed = []
failed = []


def check(name, cond, detail=""):
    if cond:
        passed.append(name)
        print(f"  [PASS] {name}")
    else:
        failed.append(name)
        print(f"  [FAIL] {name} {detail}")


def drain(s, secs):
    s.timeout = 0.2
    end = time.time() + secs
    out = b""
    while time.time() < end:
        d = s.read(8192)
        if d:
            out += d
    return out


def send_and_expect(s, cmd, expect, wait=REPLY_S):
    """Send an AT line and return True if `expect` appears in the reply window."""
    s.reset_input_buffer()
    s.write((cmd + "\r\n").encode())
    got = drain(s, wait)
    ok = expect in got
    snippet = got[-300:].decode("utf-8", "replace").replace("\n", "\\n")
    return ok, snippet


def main():
    print(f"[*] opening {PORT} @ {BAUD}")
    s = serial.Serial(PORT, BAUD, timeout=0.2)
    time.sleep(0.3)

    print("[*] draining pre-boot log...")
    s.reset_input_buffer()
    boot = b""
    deadline = time.time() + BOOT_WAIT_S
    booted = False
    while time.time() < deadline:
        d = s.read(8192)
        if d:
            boot += d
            if any(m in boot for m in BOOT_MARKERS):
                booted = True
                break
        else:
            time.sleep(0.1)
    print(f"[*] booted={booted}")
    print("--- boot tail ---")
    print(boot[-800:].decode("utf-8", "replace"))
    print("-----------------")

    check("boot: BLE init + advertising", booted)
    check(
        "boot: GATT service registered",
        b"GATT service 0x1850 started" in boot,
    )

    # Sanity: AT handshake.
    ok, snip = send_and_expect(s, "AT", b"OK")
    check("AT -> OK", ok, snip)

    # Query state (should be advertising after boot auto-start).
    ok, snip = send_and_expect(s, "AT+BLE?", b"+BLE:advertising")
    check("AT+BLE? -> +BLE:advertising", ok, snip)

    ok, snip = send_and_expect(s, "AT+BLE=STATE", b"+BLE:advertising")
    check("AT+BLE=STATE -> +BLE:advertising", ok, snip)

    # OFF -> idle.
    ok, snip = send_and_expect(s, "AT+BLE=OFF", b"+BLE:idle")
    check("AT+BLE=OFF -> +BLE:idle", ok, snip)

    # Confirm stopped.
    ok, snip = send_and_expect(s, "AT+BLE?", b"+BLE:idle")
    check("AT+BLE? (after OFF) -> +BLE:idle", ok, snip)

    # ON -> OK, then advertising.
    ok, snip = send_and_expect(s, "AT+BLE=ON", b"OK")
    check("AT+BLE=ON -> OK", ok, snip)
    time.sleep(1.0)
    ok, snip = send_and_expect(s, "AT+BLE?", b"+BLE:advertising")
    check("AT+BLE? (after ON) -> +BLE:advertising", ok, snip)

    # Malformed verb -> +CMDER:7.
    ok, snip = send_and_expect(s, "AT+BLE=MAYBE", b"+CMDER:7")
    check("AT+BLE=MAYBE -> +CMDER:7 (validated)", ok, snip)

    print()
    print(f"[RESULT] passed={len(passed)} failed={len(failed)}")
    for p in passed:
        print(f"  PASS {p}")
    if failed:
        print("FAILURES:")
        for f in failed:
            print(f"  FAIL {f}")
        sys.exit(1)
    print("[ALL PASS]")


if __name__ == "__main__":
    main()
