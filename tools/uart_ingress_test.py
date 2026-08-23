#!/usr/bin/env python3
"""Test UART ingress on the ESP32-C61.

Connects to the CP2102 USB-UART bridge (/dev/cu.usbserial-10), waits for the
firmware to boot and the ingress gateway to come up, then sends raw bytes over
UART0 and watches the log for the ingress frame (Signed mode: the device signs
whatever bytes it receives).
"""
import serial
import sys
import time

PORT = "/dev/cu.usbserial-10"
BAUD = 115200
WAIT_BOOT_S = 35   # WiFi association blocks ~30s before threads spawn
LISTEN_S = 6


def main():
    payload = sys.argv[1].encode() if len(sys.argv) > 1 else b"ping-from-host"

    s = serial.Serial(PORT, BAUD, timeout=0.2)
    buf = b""

    # 1) Drain boot logs until the ingress gateway is up.
    print(f"[*] waiting for firmware boot + ingress gateway on {PORT} ...")
    deadline = time.time() + WAIT_BOOT_S
    while time.time() < deadline:
        d = s.read(8192)
        if d:
            buf += d
            if b"gateway ready" in buf or b"all systems nominal" in buf:
                break
    print("[*] boot observed, tail:")
    tail = buf[-800:].decode("utf-8", "replace")
    print(tail)

    # 2) Send test bytes over UART0 (these reach the ESP32 UART0 RX).
    print(f"[*] sending {len(payload)} bytes: {payload!r}")
    s.write(payload)
    s.flush()

    # 3) Listen for the ingress frame log line.
    print("[*] listening for ingress frame log ...")
    deadline = time.time() + LISTEN_S
    got = b""
    while time.time() < deadline:
        d = s.read(8192)
        if d:
            got += d
    text = got.decode("utf-8", "replace")
    print("[*] received log tail:")
    print(text)
    s.close()

    if "frame" in text.lower() and "ingress" in text.lower():
        print("\n[OK] UART ingress received and signed a frame!")
        return 0
    print("\n[WARN] no ingress frame log captured (see tail above).")
    return 1


if __name__ == "__main__":
    sys.exit(main())
