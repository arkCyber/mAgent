# wearable-pipeline

> Wearable health pipeline: sensor → mqtt + email + summary routing

A single-process simulator of the multi-protocol pipeline that runs in
production when the on-watch health agent detects an anomalous heart
rate.

## What it demonstrates

| Severity   | BPM         | Routing target                       |
|------------|-------------|--------------------------------------|
| Normal     | 0–90        | `SummaryStore` (window persisted)    |
| Elevated   | 91–120      | MQTT topic `magent/health`           |
| Critical   | 121+        | MQTT topic **and** email alert       |

Every reading is routed to **exactly one** of the three sinks, and the
JSON payload shape is asserted on at every step so a downstream consumer
can rely on the field order.

## Running

```bash
cd examples/wearable-pipeline
cargo run --release
```

Expected output:

```text
=== Wearable Health Pipeline (mqtt + email + summary) ===

Test: normal readings are persisted, not published
  ✅ 72 bpm → summary only
Test: elevated readings hit the MQTT topic
  ✅ 105 bpm → mqtt topic
Test: critical readings email + mqtt
  ✅ 145 bpm → mqtt + email
Test: every reading is routed to exactly one sink
  ✅ routed 7 readings → 3 summary + 4 mqtt + 2 email
Test: JSON payload shape is stable across readings
  ✅ all 3 events parseable, fields present

=== All pipeline tests passed ===
```

Exit code is non-zero on assertion failure, so this works as a CI gate.

## Implementation notes

- **No external dependencies** — the sensor, MQTT log, SMTP outbox,
  and summary store are all in-memory stubs sized for the test
  paths. Replacing the stubs with `magent_mqtt_mcp::MqttClient` /
  `magent_email_mcp::SmtpSession` is a 1-line swap per sink.
- **Stable payload shape** — the test parses every emitted MQTT
  payload back out and asserts the three fields
  (`ts`, `bpm`, `severity`) are present and in order. Downstream
  consumers can rely on this.
- **No real network** — the binary runs in <1 ms and doesn't need a
  broker or SMTP server.

## Files

```
wearable-pipeline/
├── Cargo.toml
├── README.md
└── src/
    └── main.rs
```
