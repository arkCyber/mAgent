# email-mqtt-bridge

> Email → MQTT bridge: decode SMTP messages and publish to MQTT topics

A tiny in-memory SMTP receiver that decodes each incoming message
and re-publishes it on an MQTT topic. This mirrors what a real
`magent-email-mcp` + `magent-mqtt-mcp` pipeline does on a host:
an IMAP poller picks up a message, parses it, and the bridge
publishes a structured event so downstream consumers (dashboard,
alert router, summary store) can react.

## What it demonstrates

We don't actually open a socket — this example validates the
message-decoding + topic-routing pipeline that would run on top of
any SMTP-receiving library.

### Routing rules

A `RoutingRule` matches the email by:

- `match_from_domain` — email's `From:` ends with `@<domain>`.
- `match_subject_keyword` — case-insensitive substring match on `Subject:`.

Both fields are optional; if both are `None`, the rule is a
catch-all. First matching rule wins; if no rule matches, the
event falls back to the default topic `magent/email/inbox`.

## Running

```bash
cd examples/email-mqtt-bridge
cargo run --release
```

Expected output:

```text
=== Email → MQTT Bridge ===

Test: plain-text email decodes with body intact
  ✅ plain text → decoded
Test: subject-only email keeps both header and body
  ✅ subject-only → decoded (empty body)
Test: HTML-only email is recorded as text/html, body preserved
  ✅ html + opaque body kinds decoded
Test: routing rule matches by sender domain
  ✅ sender domain → routing rule
Test: routing rule matches by subject keyword
  ✅ subject keyword → routing rule
Test: one ingest → one MQTT event
  ✅ 5 emails → 5 mqtt events
Test: payload contains the five expected fields
  ✅ payload has 5 stable fields + escaped quotes

=== All bridge tests passed ===
```

## Why this matters

Operators want to send alerts to **multiple channels** without
duplicating logic. The bridge lets you write one SMTP listener
and route each message to whichever downstream consumer cares
about it:

- Dashboard subscribers want every event.
- Pager subscribers only want the critical ones.
- The summary store only wants persisted events.

The rule-based routing keeps the dispatch logic declarative and
easy to audit.

## Files

```
email-mqtt-bridge/
├── Cargo.toml
├── README.md
└── src/
    └── main.rs
```
