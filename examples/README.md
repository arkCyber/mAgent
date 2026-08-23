# mAgent Examples

This directory contains **end-to-end application case studies** for
mAgent. Each example is a standalone Cargo project that you can
build, run, and inspect without touching the rest of the workspace.

> **Reading time:** ~5 min to skim, ~30 min to study.
> **Build time:** <30 s per example on a modern laptop.

## Why examples instead of more bin tests?

| | `src/bin/` | `examples/` |
|---|---|---|
| Discoverable by `cargo run` | ✅ | ✅ |
| Has its own README | ❌ | ✅ |
| Reusable as a project template | ❌ | ✅ (each is a standalone crate) |
| Standalone Cargo build | ❌ (needs workspace) | ✅ |
| Cargo workspace integration | ✅ | ✅ (via the parent `Cargo.toml`) |

Each example is also a **regression gate**: the assertion failures
tell operators exactly which contract changed.

## Layout

```
examples/
├── README.md                  ← you are here
├── wearable-pipeline/         ← heartbeat data → mqtt + email + summary
├── event-router/              ← multi-protocol sink routing
├── topic-watcher/             ← head/tail compression
├── email-mqtt-bridge/         ← SMTP → MQTT topic routing
├── mqtt-roundtrip/            ← real wire-level mqtt pub/sub test
└── magent-mqtt-ingest/        ← rumqttc → MqttAdapter → IngressGateway (signed)
```

## Running any example

```bash
cd examples/<name>
cargo run --release
```

Each binary prints a header banner, then runs a series of assertions
and prints ✅ / ❌ markers. The exit code is **non-zero on failure**, so
these work as CI gates.

## Common: building everything

```bash
# Build all examples in one go
for ex in examples/*/; do
  (cd "$ex" && cargo build --release)
done
```

Or, if your Cargo workspace is set up to include `examples/` (a
follow-up PR can add it), just:

```bash
cargo build --release -p wearable-pipeline -p event-router \
                  -p topic-watcher -p email-mqtt-bridge
```

## Per-example index

| Example | What it shows | Tests |
|---|---|---|
| [wearable-pipeline](wearable-pipeline/) | Routing heart-rate readings to MQTT / email / summary based on severity | 5 |
| [event-router](event-router/) | Multi-protocol event router with topic-prefix + literal match | 5 |
| [topic-watcher](topic-watcher/) | Head/tail compression + tool-result truncation, mirroring `CompressionPolicy` | 5 |
| [email-mqtt-bridge](email-mqtt-bridge/) | SMTP → MQTT bridge with rule-based routing | 7 |
| [mqtt-roundtrip](mqtt-roundtrip/) | Real MQTT 3.1.1 wire-level round-trip via in-process broker + rumqttc | 4 |
| [magent-mqtt-ingest](magent-mqtt-ingest/) | rumqttc → MqttAdapter → IngressGateway (Signed mode) + web3 envelope verify | 1 (full-pipeline) |

## How they relate to production code

Each example is a **standalone simulation** of a real mAgent
pipeline. No broker, no SMTP server, no LLM endpoint needed.

When the production code in `magent-core` / `magent-mqtt-mcp` /
`magent-email-mcp` changes, the corresponding example's
assertions might (legitimately) shift. In that case:

1. Update the production code first.
2. Run the example to see which assertion now fails.
3. Either update the example to match the new contract, or
   investigate the regression — the example's tests are designed
   to be a tripwire for unintended changes.

## Adding a new example

1. Create `examples/<kebab-name>/` with `Cargo.toml` and `src/main.rs`.
2. Use `main.rs` as the entry point. Structure the code as
   `fn main() { banner(); test_a(); test_b(); ... }` so the
   output reads top-to-bottom.
3. Add a `README.md` with the layout, expected output, and "Why this
   matters" section.
4. Add a row to the per-example index above.
5. Add the binary to the workspace root `Cargo.toml` `members` list
   (if you want it to share workspace metadata).

Each example is intentionally tied to **one observable outcome** so
the failure messages read like a contract change.
