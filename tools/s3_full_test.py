#!/usr/bin/env python3
"""Full on-hardware acceptance sweep for the mAgent ESP32-S3 firmware.

Everything runs over the board's **native USB console** (`/dev/cu.usbmodem*`).
That single link carries both the boot/agent log mirror and — thanks to the
`UsbSerialAdapter` registered by `firmware/esp32-app/src/link_adapters.rs` —
AT commands / natural-language agent tasks, so no external USB-UART adapter
wired to the UART0 pins is required.

Sections
--------
1. Boot      8/8 boot phases, all threads, PSRAM-backed free heap, both
             ingress links (UART0 + USB-Serial/JTAG) registered
2. AT smoke  GMR / SYSRAM / HEAP / IDENT / UPTIME / CWSTATE / IFCONFIG /
             CIPSTAMAC / SAFEMODE / TIME / BLE + `+CMDER` error codes
3. Ingress   every command round-trips (`[ingress] frame src=…` + reply), i.e.
             the USB console really is a bidirectional command channel
4. Agent     natural-language ReAct task served by an on-device tool
             (`read the temperature`) plus multi-turn continuation
5. Memory    free-heap drift across N agent turns (leak detection)
6. Network   Wi-Fi link state; when the board has an IP: web_admin HTTP
             dashboard (`/`), `/api/status` JSON, and `AT+HTTPGET`
7. Report    PASS/FAIL per check; exit code 1 if anything failed

Usage
-----
    python3 tools/s3_full_test.py [PORT] [--turns N] [--no-reset]

Defaults: PORT=/dev/cu.usbmodem101, --turns 5. Requires `pyserial`
(`pip3 install pyserial`) and, for the reset step, `esptool`.
"""
import argparse
import json
import re
import sys
import time
import urllib.request

try:
    import serial
except ImportError:  # pragma: no cover - environment guard
    print("[FATAL] pyserial not installed: pip3 install pyserial")
    sys.exit(2)

BAUD = 115200
BOOT_WAIT_S = 90          # WiFi association + agent warm-up at boot
REPLY_S = 4.0             # per-AT-command reply window
AGENT_S = 45.0            # agent ReAct turn (LLM probe timeout + local fallback)

passed = []
failed = []


def check(name, cond, detail=""):
    if cond:
        passed.append(name)
        print(f"  [PASS] {name}")
    else:
        failed.append(name)
        print(f"  [FAIL] {name} {detail}")
    return bool(cond)


def read_until(s, markers, timeout):
    """Read until one of `markers` (bytes) appears, or `timeout` seconds pass.

    A quiet-time based drain is wrong for this firmware: boot-time Wi-Fi
    association and the agent's ReAct turn both have multi-second gaps with no
    serial traffic at all.
    """
    buf = b""
    end = time.time() + timeout
    while time.time() < end:
        d = s.read(8192)
        if d:
            buf += d
            if any(m in buf for m in markers):
                break
        else:
            time.sleep(0.02)
    return buf


def send_cmd(s, line, markers=(b"OK", b"ERROR"), timeout=REPLY_S):
    """Send one console line and read until a reply marker (or timeout).

    The marker can land *mid-line*: a long reply (e.g. `AT+HTTPGET` echoing a
    body) is flushed in several UART/USB chunks, so breaking on the first chunk
    that contains `+HTTPGET:` would truncate the body. Once a marker matched we
    therefore keep draining until the trailing `OK`/`ERROR` appears (or a short
    hard cap), which is a no-op for commands whose marker *is* the terminator.
    """
    s.reset_input_buffer()
    s.write((line + "\r\n").encode())
    s.flush()
    buf = read_until(s, markers, timeout)
    if b"OK" not in buf and b"ERROR" not in buf:
        hard_end = time.time() + 2.0
        while time.time() < hard_end:
            d = s.read(8192)
            if d:
                buf += d
                if b"OK" in buf or b"ERROR" in buf:
                    break
            else:
                time.sleep(0.02)
    return buf.decode("utf-8", "replace")


def reset_board(s):
    """esptool's classic reset sequence (USB-Serial/JTAG honours RTS/DTR)."""
    s.setDTR(False)
    s.setRTS(True)
    time.sleep(0.15)
    s.setDTR(True)
    s.setRTS(False)
    time.sleep(0.15)
    s.setRTS(False)


def release_lines(s):
    """Leave the port in a benign state before closing it.

    The S3's native USB-Serial/JTAG is also the flashing channel *and* (via
    `UsbSerialAdapter`) an ingress link, and its reset lines are driven by the
    host's DTR/RTS. Closing a port while they are asserted can leave the chip
    with the USB-JTAG peripheral in an inconsistent state — observed once as a
    silent console plus `esptool: No serial data received` until the board was
    physically reset. De-asserting both lines (and a short settle) costs nothing
    and lowers that risk.
    """
    try:
        s.setDTR(False)
        s.setRTS(False)
        time.sleep(0.1)
    except Exception:  # noqa: BLE001 - best effort, never fail the run
        pass


def http_get(url, timeout=6.0):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return r.status, r.read().decode("utf-8", "replace")


def main():
    ap = argparse.ArgumentParser(description="mAgent ESP32-S3 full acceptance sweep")
    ap.add_argument("port", nargs="?", default="/dev/cu.usbmodem101")
    ap.add_argument("--turns", type=int, default=5, help="agent turns for the leak check")
    ap.add_argument("--no-reset", action="store_true", help="do not reset the board first")
    args = ap.parse_args()

    print(f"[*] mAgent ESP32-S3 acceptance sweep on {args.port} @ {BAUD}")
    s = serial.Serial(args.port, BAUD, timeout=0.1)
    time.sleep(0.3)

    # ------------------------------------------------------------------
    # 1. Boot
    # ------------------------------------------------------------------
    print("\n== 1. boot ==")
    if not args.no_reset:
        print("[*] resetting the board (capturing the boot log from byte 0) ...")
        s.reset_input_buffer()
        reset_board(s)
    boot = read_until(s, (b"all systems nominal",), BOOT_WAIT_S).decode("utf-8", "replace")
    with open("/tmp/s3_full_test_boot.log", "w") as f:
        f.write(boot)

    check("boot: 8/8 phases + all systems nominal",
          "all systems nominal" in boot,
          f"(tail: {boot[-200:]!r})")
    check("boot: agent thread ready", "[agent] MiniAgent ready" in boot)
    check("boot: ingress gateway ready", "[ingress] gateway ready" in boot)
    check("boot: UART0 ingress link", "[ingress] UART0 registered" in boot)
    check("boot: USB-Serial/JTAG ingress link",
          "[ingress] USB-Serial/JTAG console registered" in boot,
          "(see the [ingress] warn line for the reason)")
    check("boot: no panic / crash-loop",
          not any(m in boot for m in ("Guru Meditation", "abort()", "assert failed",
                                      "Backtrace:", "Safe mode")))

    m = re.search(r"total_free=(\d+) B", boot) or re.search(r'"free_heap_b":(\d+)', boot)
    check("boot: PSRAM heap > 2 MB",
          bool(m) and int(m.group(1)) > 2_000_000,
          f"(heap={m.group(1) if m else 'n/a'})")

    heap0 = None
    m = re.search(r"\+SYSRAM:(\d+)", boot)
    if m:
        heap0 = int(m.group(1))

    # ------------------------------------------------------------------
    # 2. AT smoke
    # ------------------------------------------------------------------
    print("\n== 2. AT command surface ==")
    s.reset_input_buffer()
    r = send_cmd(s, "AT")
    check("AT -> OK", "OK" in r)
    check("ingress: frame logged for the AT (USB link)",
          "[ingress] frame" in r,
          "(no [ingress] frame line: the USB console is not wired to the gateway)")

    r = send_cmd(s, "AT+GMR")
    m = re.search(r"\+GMR:([^\r\n]+)", r)
    check("AT+GMR -> board id", bool(m) and "esp32-s3" in m.group(1),
          m.group(1) if m else "(no +GMR)")

    r = send_cmd(s, "AT+SYSRAM?")
    m = re.search(r"\+SYSRAM:(\d+)", r)
    heap = int(m.group(1)) if m else 0
    check("AT+SYSRAM? -> free heap > 2 MB (PSRAM)", heap > 2_000_000, f"(heap={heap})")
    if heap and heap0 is None:
        heap0 = heap

    r = send_cmd(s, "AT+HEAP?")
    check("AT+HEAP? -> free heap", "+HEAP:" in r, r[-120:])

    r = send_cmd(s, "AT+IDENT?")
    m = re.search(r"\+IDENT:([0-9a-fA-F]+)", r)
    check("AT+IDENT? -> Ed25519 pubkey", bool(m) and len(m.group(1)) == 64,
          f"(len={len(m.group(1)) if m else 0})")

    r = send_cmd(s, "AT+UPTIME?")
    m = re.search(r"\+UPTIME:(\d+)", r)
    check("AT+UPTIME? -> ms counter", bool(m) and int(m.group(1)) > 0,
          m.group(1) if m else "(no +UPTIME)")

    r = send_cmd(s, "AT+CWSTATE?")
    m = re.search(r"\+CWSTATE:(\d+)(?:,(\S+))?", r)
    sta_state = int(m.group(1)) if m else -1
    sta_ip = (m.group(2) or "").strip() if m else ""
    check("AT+CWSTATE? -> state reply", bool(m), r[-120:])
    print(f"    wifi state={sta_state} ip={sta_ip or 'none'}")

    r = send_cmd(s, "AT+IFCONFIG?")
    m = re.search(r"\+IFCONFIG:\"([^\"]*)\"", r)
    ip = (m.group(1).strip() if m else "") or sta_ip
    check("AT+IFCONFIG? -> ip field", bool(m), r[-120:])

    r = send_cmd(s, "AT+CIPSTAMAC?")
    m = re.search(r"\+CIPSTAMAC:\"?([0-9a-fA-F:]{17})", r)
    check("AT+CIPSTAMAC? -> real STA MAC", bool(m), m.group(1) if m else r[-120:])

    r = send_cmd(s, "AT+SAFEMODE?")
    check("AT+SAFEMODE? -> 0 (normal boot)", "+SAFEMODE:0" in r, r[-120:])

    r = send_cmd(s, "AT+TIME?")
    check("AT+TIME? -> reply", ("+TIME:" in r) or ("OK" in r), r[-120:])

    # Error paths: syntactically valid but unsupported (BLE is not compiled
    # into this build) and a malformed verb must both be rejected, not swallowed.
    r = send_cmd(s, "AT+BLE?")
    check("AT+BLE? -> +CMDER:9 (BLE not built)", "+CMDER:9" in r, r[-120:])
    r = send_cmd(s, "AT+NOTACOMMAND=1")
    check("AT+NOTACOMMAND=1 -> +CMDER error (no silent drop)",
          "+CMDER:" in r and "ERROR" in r, r[-120:])


    # ------------------------------------------------------------------
    # 3. Agent (natural-language ReAct loop, on-device tool)
    # ------------------------------------------------------------------
    print("\n== 3. agent ReAct loop (on-device tools) ==")
    r = send_cmd(s, "read the temperature", markers=(b"RESULT[",), timeout=AGENT_S)
    m = re.search(r"temperature=([0-9.]+) C", r)
    check("agent: temperature tool answered via the USB link", bool(m),
          f"(reply tail: {r[-200:]!r})")
    if m:
        print(f"    temperature = {m.group(1)} C")
    check("agent: reply was routed back to the host (`RESULT[`)", "RESULT[" in r,
          "(no RESULT[ line seen)")

    # ------------------------------------------------------------------
    # 4. Memory drift across N agent turns (leak detection)
    # ------------------------------------------------------------------
    print(f"\n== 4. free-heap stability over {args.turns} agent turns ==")
    heaps = []
    for i in range(args.turns):
        send_cmd(s, "read the temperature", markers=(b"RESULT[",), timeout=AGENT_S)
        time.sleep(0.3)
        r = send_cmd(s, "AT+SYSRAM?")
        m = re.search(r"\+SYSRAM:(\d+)", r)
        if m:
            heaps.append(int(m.group(1)))
        print(f"    turn {i + 1}/{args.turns}: heap={heaps[-1] if heaps else 'n/a'} B")
    if len(heaps) >= 2:
        start, end = heaps[0], heaps[-1]
        check("heap stable (no monotonic leak >= 10%)", end >= start * 0.90,
              f"(start={start} end={end} drift={start - end})")

    # ------------------------------------------------------------------
    # 5. Multi-turn continuation
    # ------------------------------------------------------------------
    print("\n== 5. multi-turn continuation ==")
    r1 = send_cmd(s, "read the temperature and remember it", markers=(b"RESULT[",), timeout=AGENT_S)
    check("turn 1 produced a tool result", "temperature=" in r1 or "RESULT[" in r1)
    r2 = send_cmd(s, "what temperature did you just report?", markers=(b"RESULT[",), timeout=AGENT_S)
    check("turn 2 answered", ("temperature=" in r2) or ("RESULT[" in r2),
          f"(tail: {r2[-160:]!r})")

    # ------------------------------------------------------------------
    # 6. Wi-Fi + web_admin (only when the board actually has an IP)
    # ------------------------------------------------------------------
    print("\n== 6. Wi-Fi link + web_admin ==")

    def station_ip(timeout=90.0):
        """Poll for a station IP (a fresh boot may need an extra association
        attempt: the boot-time connect can time out once and the supervisor then
        retries with a short backoff)."""
        deadline = time.time() + timeout
        last = ""
        while time.time() < deadline:
            r = send_cmd(s, "AT+CWSTATE?", markers=(b"+CWSTATE:", b"ERROR"), timeout=6)
            m = re.search(r"\+CWSTATE:(\d+)(?:,([0-9.]+))?", r)
            if m:
                last = m.group(0)
                if m.group(2):
                    return m.group(2).strip()
            time.sleep(5)
        if last:
            print(f"    (last state seen: {last})")
        return ""

    ip = station_ip()

    if not ip or ip == "0.0.0.0":
        print("  [SKIP] no station IP — the configured AP is not reachable from "
              "here (hotspot off / 2.4 GHz max-compatibility disabled?)")
        print("         provision one with:  AT+CWJAP=\"<ssid>\",\"<pass>\"  "
              "(live reconnect, no reboot)")
        print("         tip: AT+CWLAP lists the 2.4 GHz APs the radio actually sees")
    else:
        print(f"    station ip = {ip}")

        # Host-side checks only work when this machine is on the board's subnet.
        # Probe first so an unshared-network setup is reported as SKIP (with the
        # device-side equivalents below still exercising the same HTTP server).
        host_sees_board = False
        try:
            http_get(f"http://{ip}/api/status", timeout=3.0)
            host_sees_board = True
        except Exception:  # noqa: BLE001 - any transport error means "not reachable"
            host_sees_board = False

        if host_sees_board:
            try:
                status, body = http_get(f"http://{ip}/")
                check("web_admin: GET / -> 200 HTML dashboard",
                      status == 200 and "mAgent" in body, f"(status={status})")
            except Exception as e:  # noqa: BLE001
                check("web_admin: GET / -> 200 HTML dashboard", False, f"({e})")
            try:
                status, body = http_get(f"http://{ip}/api/status")
                js = json.loads(body)
                check("web_admin: /api/status -> JSON with heap/uptime/wifi fields",
                      status == 200
                      and {"free_heap_b", "uptime_ms", "wifi_state", "wifi_reason"} <= set(js),
                      f"(keys={sorted(js)})")
                print(f"    api/status = {json.dumps(js)[:220]}")
            except Exception as e:  # noqa: BLE001
                check("web_admin: /api/status -> JSON with heap/uptime/wifi fields",
                      False, f"({e})")
        else:
            print(f"  [SKIP] host-side HTTP: this machine cannot reach {ip} "
                  "(not on the board's subnet) — using device-side HTTPGET instead")

        # Device-side HTTP through the board's own stack (works either way).
        # Wait for the *reply* line (`+HTTPGET:`) rather than the default
        # OK/ERROR markers: the firmware logs `[at] HTTPGET: free heap before
        # request …` / `… (preflight ok)` around the request, and those log words
        # can satisfy a naive marker/`OK` match before the real reply arrives.
        r = send_cmd(s, f"AT+HTTPGET=http://{ip}/", timeout=25,
                     markers=(b"+HTTPGET:", b"+CMDER:"))
        m = re.search(r"\+HTTPGET:(\d+) bytes=(\d+)", r)
        check("AT+HTTPGET / (web_admin dashboard over the board's own stack)",
              bool(m) and m.group(1) == "200"
              and ("<html" in r or "<!DOCTYPE" in r or "mAgent v0.1.0" in r),
              f"(tail: {r[-200:]!r})")
        if m:
            print(f"    GET / -> HTTP {m.group(1)}, {m.group(2)} bytes")

        r = send_cmd(s, f"AT+HTTPGET=http://{ip}/api/status", timeout=25,
                     markers=(b"+HTTPGET:", b"+CMDER:"))
        m = re.search(r"\+HTTPGET:(\d+) bytes=(\d+)", r)
        # NOTE: the `+HTTPGET` reply line echoes at most REPLY_LINE_MAX (256) bytes
        # of the body, so only the *leading* JSON fields can be asserted on here.
        check("AT+HTTPGET /api/status -> 200 + JSON",
              bool(m) and m.group(1) == "200"
              and '"version"' in r and '"wifi_state"' in r,
              f"(tail: {r[-200:]!r})")
        if m:
            print(f"    GET /api/status -> HTTP {m.group(1)}, {m.group(2)} bytes")

        # Outbound internet: DNS + TCP + TLS + HTTP in one shot. A 401/403/404
        # from the real endpoint still proves the whole path worked.
        r = send_cmd(s, "AT+HTTPGET=https://api.deepseek.com", timeout=45,
                     markers=(b"+HTTPGET:", b"+CMDER", b"ERROR"))
        m = re.search(r"\+HTTPGET:(\d+)", r)
        check("AT+HTTPGET https://api.deepseek.com -> real TLS HTTP status",
              bool(m) and int(m.group(1)) in (200, 301, 302, 401, 403, 404),
              f"(tail: {r[-200:]!r})")
        if m:
            print(f"    outbound HTTPS status = {m.group(1)} (DNS+TCP+TLS verified)")

        # ICMP echo (esp_ping). 8.8.8.8 may be filtered by the carrier — the
        # board's own address always answers, so that is the hard check.
        r = send_cmd(s, f"AT+PING={ip}", timeout=20,
                     markers=(b"+PING:", b"+CMDER", b"ERROR"))
        m = re.search(r"\+PING: reply=(\d+)", r)
        check("AT+PING <own ip> -> ICMP reply", bool(m) and int(m.group(1)) > 0,
              f"(tail: {r[-200:]!r})")
        r = send_cmd(s, "AT+PING=8.8.8.8", timeout=20,
                     markers=(b"+PING:", b"+CMDER", b"ERROR"))
        m = re.search(r"\+PING: reply=(\d+) rtt=(\d+)us", r)
        if m and int(m.group(1)) > 0:
            check("AT+PING 8.8.8.8 -> internet ICMP reply", True)
            print(f"    internet ping reply={m.group(1)} rtt={m.group(2)}us")
        else:
            print("  [SKIP] AT+PING 8.8.8.8 no reply — upstream ICMP filter "
                  "(the local-address ping above already proved esp_ping works)")

        # Wall-clock sync (SNTP over UDP/123). `AT+NTPSYNC` forces an immediate
        # attempt; the AT reply is just `OK`, so the observable result is the
        # `AT+TIME?` line switching from `UNSYNCED` to an RFC-3339 timestamp.
        r = send_cmd(s, "AT+NTPSYNC")
        check("AT+NTPSYNC accepted", "OK" in r, r[-160:])
        deadline = time.time() + 45
        time_line = ""
        while time.time() < deadline:
            r = send_cmd(s, "AT+TIME?", markers=(b"+TIME:", b"+CMDER", b"ERROR"), timeout=8)
            m = re.search(r"\+TIME:([^\r\n\"]+)", r)
            if m:
                time_line = m.group(1).strip()
                if "UNSYNCED" not in time_line:
                    break
            time.sleep(3)
        check("SNTP time sync -> RFC-3339 wall clock (not UNSYNCED)",
              bool(time_line) and "UNSYNCED" not in time_line,
              f"(+TIME:{time_line})")
        if time_line:
            print(f"    +TIME:{time_line}")

    # ------------------------------------------------------------------
    # 7. Report
    # ------------------------------------------------------------------
    release_lines(s)
    s.close()
    print("\n==============================================")
    print(f"RESULT: {len(passed)} passed, {len(failed)} failed  "
          f"(boot log saved to /tmp/s3_full_test_boot.log)")
    if failed:
        print("FAILED CHECKS:")
        for f in failed:
            print(f"  - {f}")
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
