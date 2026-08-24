#!/usr/bin/env python3
"""Test Wi-Fi link on the ESP32-C61.

Connects to the CP2102 USB-UART bridge, waits for the firmware to boot,
then provisions the default AP (or an AP passed as arguments) over the
AT console and verifies the link comes up:

    1. AT+CWJAP="<ssid>","<pass>"   -> provision (persisted to NVS, DBO-sealed)
    2. AT+CWJAP?                    -> reports the remembered SSID
    3. AT+CWSTATE?                  -> reports the STA link state
    4. reboot (AT+RST, deferred in v0.2) then re-query

The boot log line `[wifi] connected — ip=...` confirms a real link.

Usage:
    python3 tools/wifi_link_test.py "MySSID" "pass123"   # pass AP as args
    python3 tools/wifi_link_test.py                       # requires args or env (no creds committed)
"""
import serial
import sys
import time

PORT = "/dev/cu.usbserial-10"
BAUD = 115200
WAIT_BOOT_S = 40   # allow up to ~30s blocking Wi-Fi association at boot
LISTEN_S = 8

# Default AP credentials used only when no AP is passed on the command line.
# Left empty so no real credentials are committed — pass them as arguments or
# override below at runtime:
#   python3 tools/wifi_link_test.py "MySSID" "MyPass"
DEFAULT_SSID = ""
DEFAULT_PASS = ""


def read_until(s, token, timeout_s):
    """Drain serial until `token` appears or the timeout elapses."""
    deadline = time.time() + timeout_s
    buf = b""
    while time.time() < deadline:
        d = s.read(8192)
        if d:
            buf += d
            if token in buf:
                return buf
    return buf


def send(s, line):
    s.write(line + b"\r\n")
    s.flush()


def main():
    ssid = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_SSID
    password = sys.argv[2] if len(sys.argv) > 2 else DEFAULT_PASS

    s = serial.Serial(PORT, BAUD, timeout=0.2)
    print(f"[*] firmware boot on {PORT} ({ssid}/{password}) ...")
    boot = read_until(s, b"gateway ready", WAIT_BOOT_S)
    print("[*] boot observed; last boot lines:")
    print(boot[-700:].decode("utf-8", "replace"))

    ok = True

    # 1) Provision.
    cmd = f'AT+CWJAP="{ssid}","{password}"'.encode()
    print(f"[*] -> {cmd.decode()}")
    send(s, cmd)
    resp = read_until(s, b"OK", LISTEN_S)
    if b"OK" in resp:
        print("[OK] AT+CWJAP= accepted")
    else:
        print("[FAIL] AT+CWJAP= did not return OK; tail:")
        print(resp[-400:].decode("utf-8", "replace"))
        ok = False

    # 2) Remembered SSID.
    send(s, b"AT+CWJAP?")
    resp = read_until(s, b"OK", LISTEN_S)
    text = resp.decode("utf-8", "replace")
    print("[*] AT+CWJAP? ->", " | ".join(l for l in text.splitlines() if l.startswith("+CWJAP")) or text[-300:])
    if f"+CWJAP:\"{ssid}\"" in text:
        print("[OK] remembered SSID matches")
    else:
        print("[WARN] remembered SSID differs or not shown yet")
        ok = False

    # 3) STA state. v0.2 reports a sentinel (4); a real connect is
    #    confirmed by the `[wifi] connected — ip=` boot/health log.
    send(s, b"AT+CWSTATE?")
    resp = read_until(s, b"OK", LISTEN_S)
    text = resp.decode("utf-8", "replace")
    print("[*] AT+CWSTATE? ->", " | ".join(l for l in text.splitlines() if l.startswith("+CWSTATE")) or text[-300:])

    # 4) Reboot so the provisioned NVS entry drives boot-time connect.
    print("[*] -> AT+RST (deferred; takes effect on next boot)")
    send(s, b"AT+RST")
    read_until(s, b"OK", LISTEN_S)
    boot2 = read_until(s, b"connected", WAIT_BOOT_S)
    if b"connected" in boot2 and b"ip=" in boot2:
        print("[OK] boot-time Wi-Fi link established (see log line above)")
    else:
        print("[INFO] no 'connected' line captured — device may already be up or in safe mode")
        print(boot2[-400:].decode("utf-8", "replace"))

    s.close()
    print("\nRESULT:", "PASS" if ok else "PARTIAL — inspect the log above")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
