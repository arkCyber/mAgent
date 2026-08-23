# event-router

> Multi-protocol event router: mqtt + email + summary sinks with topic-prefix routing

A single in-memory message bus with three "leaves" — MQTT broker,
SMTP relay, and a local summary store. Each event carries a routing
key; the router decides which leaves see the event.

## What it demonstrates

This is the production routing topology for `magent-core`'s
`event_bus` module, simplified. In the real codebase, the router
is a `Mutex<Vec<Box<dyn EventSink>>>` so consumers can be added at
runtime; here we keep a fixed three-sink array for clarity.

### Routing rules

| Sink        | Topic match prefix           | Notes                          |
|-------------|------------------------------|--------------------------------|
| MQTT        | `magent/`                    | All non-summary events         |
| Email       | `magent/alert/critical`      | Literal match, no wildcards    |
| Summary     | `summary/`                   | Persisted events only          |

If no sink matches, the event is dropped (and the test asserts
that).

## Running

```bash
cd examples/event-router
cargo run --release
```

Expected output:

```text
=== Multi-Protocol Event Router (mqtt + email + summary) ===

Test: 'summary/save' goes to summary sink only
  ✅ summary/save → summary (1 sink, 13 bytes)
Test: 'magent/alert/critical' hits MQTT + Email
  ✅ magent/alert/critical → mqtt(11B) + email(14B)
Test: summary sink only consumes 'summary/*'
  ✅ 5 events → mqtt(2) + email(0) + summary(3)
Test: same topic → same set of sinks
  ✅ deterministic across replays
Test: no-match topic is dropped (0 sinks)
  ✅ system/heartbeat → 0 sinks (dropped)

=== All router tests passed ===
```

## Implementation notes

- **Trait-based sinks** — `trait Sink` exposes `matches(topic) →
  bool` and `deliver(event) → bytes`. Adding a new sink is one
  struct + one impl.
- **Per-sink throughput** — the router tracks bytes-by-sink so
  operators can spot imbalance (e.g. MQTT overloaded while email
  is idle).
- **Deterministic** — same input + same rules → same output. The
  test runs the same routing twice and asserts byte-for-byte
  equality.

## Files

```
event-router/
├── Cargo.toml
├── README.md
└── src/
    └── main.rs
```
