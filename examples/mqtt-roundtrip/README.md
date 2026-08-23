# `mqtt-roundtrip`

> **Real** MQTT 3.1.1 round-trip test, all in-process. No broker
> install required, no internet — just `cargo run`.

## What it shows

Unlike the other examples (which simulate the wire protocol in
memory), this one stands up a **minimal in-process MQTT broker**
on `127.0.0.1:<random-port>` and connects **two `rumqttc`
clients** to it — a publisher and a subscriber. We publish a
single message and assert it arrives at the subscriber intact.

This is a true wire-level test: it exercises the same
`rumqttc 0.24` used by `host/mqtt-mcp`, the same TCP transport,
and the same MQTT 3.1.1 packet framing a real broker would
speak. If `rumqttc` and a real broker ever drift, this example
catches it.

## Why we wrote our own broker

The other examples avoid the network entirely (good — they're
fast and deterministic). We deliberately **don't** here,
because MQTT is the subject under test. But installing
`mosquitto` to run a unit test is overkill, and `rumqttd` 0.20
panics on `rumqttc` 0.24's CONNECT packets (an upstream
incompatibility).

So the broker is a tiny (~200 LoC) pure-`tokio` server that
handles exactly what this example needs:

| Packet | Direction |
|---|---|
| `CONNECT`     | client → broker |
| `CONNACK`     | broker → client |
| `SUBSCRIBE`   | client → broker |
| `SUBACK`      | broker → client |
| `PUBLISH`     | client → broker → client |
| `DISCONNECT`  | client → broker |

Quality of Service is fixed at **QoS 0** for this example:
that's enough to prove the wire-level fan-out works, and it
keeps the broker small.

## Layout

```
mqtt-roundtrip/
├── Cargo.toml
├── README.md
└── src/
    ├── main.rs        ← the test harness
    ├── broker.rs      ← the in-process MQTT broker
    └── protocol.rs    ← MQTT 3.1.1 packet codec
```

## Running

```bash
cd examples/mqtt-roundtrip
cargo run --release
```

Expected output:

```
=== MQTT Round-Trip (real wire, in-process broker) ===

Test: broker binds a free port
  ✅ bound on 127.0.0.1:54321
Test: subscriber connects + subscribes
  ✅ connected
Test: publisher publishes + subscriber receives
  ✅ payload round-tripped: 42,42,42,42
Test: subscriber disconnect is acknowledged
  ✅ clean disconnect
```

## How it relates to production

This example uses the **same `rumqttc 0.24`** dependency that
`host/mqtt-mcp` ships with. If a future `rumqttc` upgrade
introduces a protocol change (e.g. new CONNECT properties,
MQTT 5 vs 3.1.1), the in-process broker stops being able to
parse the new packets and the round-trip test fails immediately
— before we ship a binary that can't talk to real brokers.

## Limitations

- QoS 1 / QoS 2 are not implemented. Adding them requires
  `PUBACK` / `PUBREC` / `PUBREL` / `PUBCOMP` handling on both
  sides — straightforward but a bigger patch.
- The broker is single-threaded (`tokio::spawn` per
  connection, in-memory topic table). It is **not** a
  substitute for `mosquitto` in any production setting.