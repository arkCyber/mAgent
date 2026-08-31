#!/usr/bin/env python3
"""Test UART ingress on an ESP32 (C61, S3, ...).

Connects to the board's USB-UART bridge, waits for the firmware to boot and the
ingress gateway to come up, then sends raw bytes over UART0 and watches the log
for evidence the ingress received and signed them (Signed mode: the device signs
whatever bytes it receives, or dispatches them as an AT command).

Usage:
    python3 tools/uart_ingress_test.py [PORT] [PAYLOAD]

    PORT    serial device, default /dev/cu.usbserial-10 (C61)
    PAYLOAD bytes to send after boot, default "ping-from-host"
"""
import serial
import sys
import time

PORT = "/dev/cu.usbserial-10"
BAUD = 115200
WAIT_BOOT_S = 35   # WiFi association blocks ~30s before threads spawn
LISTEN_S = 6
# Evidence the ingress received the bytes: either an explicit frame/sign log
# line, or the firmware dispatching the payload as an AT command.
EVIDENCE = ("frame", "ingress", "sign", "ok", "[at]", "cmder")


def main():
    args = [a for a in sys.argv[1:] if a]
    port = args[0] if len(args) > 0 else PORT
    payload = args[1].encode() if len(args) > 1 else b"ping-from-host"

    s = serial.Serial(port, BAUD, timeout=0.2)
    buf = b""

    # 1) Drain boot logs until the ingress gateway is up.
    print(f"[*] waiting for firmware boot + ingress gateway on {port} ...")
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

    if any(ev in text.lower() for ev in EVIDENCE):
        print("\n[OK] UART ingress evidence observed (ingress frame / sign / AT dispatch).")
        return 0
    print("\n[WARN] no ingress evidence captured (see tail above).")
    return 1


if __name__ == "__main__":
    sys.exit(main())
