//! Multi-protocol event router: a single in-memory message bus
//! with three "leaves" — MQTT broker, SMTP relay, and a local
//! summary store. Each event carries a routing key; the router
//! decides which leaves see the event.
//!
//! This is the production routing topology for `magent-core`'s
//! `event_bus` module, simplified. In the real codebase, the
//! router is a `Mutex<Vec<Box<dyn EventSink>>>` so consumers can
//! be added at runtime; here we keep a fixed three-sink array
//! for clarity.
//!
//! Run with: `cargo run -p magent-tools --bin event-router`

use std::collections::HashMap;

fn main() {
    println!("=== Multi-Protocol Event Router (mqtt + email + summary) ===\n");

    test_single_topic_routes_to_one_sink();
    test_wildcard_topic_routes_to_multiple_sinks();
    test_summary_sink_receives_only_persistent_topics();
    test_routing_is_deterministic_for_same_topic();
    test_no_match_drops_event();

    println!("\n=== All router tests passed ===");
}

/// Tag identifying an event's destination protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SinkKind {
    /// MQTT broker.
    Mqtt,
    /// SMTP relay.
    Email,
    /// Local summary store (persisted to disk).
    Summary,
}

impl SinkKind {
    /// Wire name used in the JSON-RPC / log envelopes. Kept
    /// as a public method even though this example doesn't
    /// use it — operators reach for it when triaging.
    #[allow(dead_code)]
    fn as_str(self) -> &'static str {
        match self {
            SinkKind::Mqtt => "mqtt",
            SinkKind::Email => "email",
            SinkKind::Summary => "summary",
        }
    }
}

/// A single event flowing through the router.
#[derive(Debug, Clone)]
struct Event {
    /// Routing topic, e.g. `"magent/health/critical"` or
    /// `"summary/save"`. Topics may contain `/` separators.
    topic: String,
    /// UTF-8 payload.
    body: String,
}

impl Event {
    fn new(topic: &str, body: &str) -> Self {
        Self {
            topic: topic.to_string(),
            body: body.to_string(),
        }
    }
}

/// One protocol-specific subscriber. The router holds a list of
/// these; when an event arrives, it calls `matches` on each and
/// dispatches to the ones that return `true`.
trait Sink: std::fmt::Debug {
    fn kind(&self) -> SinkKind;
    /// Topic-match predicate. `true` means this sink wants the
    /// event. We use a simple prefix match — production code
    /// uses MQTT-style wildcards (`+` and `#`), but a prefix
    /// match is good enough for this demo.
    fn matches(&self, topic: &str) -> bool;
    /// Receive a delivery. Returns the number of bytes consumed
    /// so the router can compute its throughput.
    fn deliver(&mut self, event: &Event) -> usize;
}

/// MQTT subscriber: matches every topic starting with
/// `"magent/"`.
#[derive(Debug)]
struct MqttSink {
    /// Subscriber label — surfaced in `Debug` dumps so operators
    /// can identify which sink caught which event.
    #[allow(dead_code)]
    name: String,
    delivered: Vec<Event>,
}

impl MqttSink {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            delivered: Vec::new(),
        }
    }
}

impl Sink for MqttSink {
    fn kind(&self) -> SinkKind {
        SinkKind::Mqtt
    }
    fn matches(&self, topic: &str) -> bool {
        topic.starts_with("magent/")
    }
    fn deliver(&mut self, event: &Event) -> usize {
        self.delivered.push(event.clone());
        event.body.len()
    }
}

/// Email subscriber: matches the literal topic
/// `"magent/alert/critical"` (no wildcards).
#[derive(Debug)]
struct EmailSink {
    #[allow(dead_code)]
    name: String,
    delivered: Vec<Event>,
}

impl EmailSink {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            delivered: Vec::new(),
        }
    }
}

impl Sink for EmailSink {
    fn kind(&self) -> SinkKind {
        SinkKind::Email
    }
    fn matches(&self, topic: &str) -> bool {
        topic == "magent/alert/critical"
    }
    fn deliver(&mut self, event: &Event) -> usize {
        self.delivered.push(event.clone());
        // SMTP wraps the body in a base64 blob; pretend.
        event.body.len() * 4 / 3
    }
}

/// Summary subscriber: matches every topic starting with
/// `"summary/"`.
#[derive(Debug)]
struct SummarySink {
    #[allow(dead_code)]
    name: String,
    delivered: Vec<Event>,
}

impl SummarySink {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            delivered: Vec::new(),
        }
    }
}

impl Sink for SummarySink {
    fn kind(&self) -> SinkKind {
        SinkKind::Summary
    }
    fn matches(&self, topic: &str) -> bool {
        topic.starts_with("summary/")
    }
    fn deliver(&mut self, event: &Event) -> usize {
        self.delivered.push(event.clone());
        event.body.len()
    }
}

/// The router itself. Holds the sink list and a per-sink-kind
/// throughput counter so operators can spot imbalance.
struct Router {
    sinks: Vec<Box<dyn Sink>>,
    bytes_by_sink: HashMap<SinkKind, usize>,
}

impl Router {
    fn new(sinks: Vec<Box<dyn Sink>>) -> Self {
        Self {
            sinks,
            bytes_by_sink: HashMap::new(),
        }
    }

    /// Dispatch one event. Returns the number of sinks that
    /// consumed it (could be 0, 1, or many).
    fn route(&mut self, event: &Event) -> usize {
        let mut delivered = 0;
        for sink in &mut self.sinks {
            if sink.matches(&event.topic) {
                let bytes = sink.deliver(event);
                *self.bytes_by_sink.entry(sink.kind()).or_insert(0) += bytes;
                delivered += 1;
            }
        }
        delivered
    }

    fn bytes_through(&self, kind: SinkKind) -> usize {
        self.bytes_by_sink.get(&kind).copied().unwrap_or(0)
    }
}

fn build_default_router() -> Router {
    Router::new(vec![
        Box::new(MqttSink::new("primary")),
        Box::new(EmailSink::new("oncall")),
        Box::new(SummarySink::new("default")),
    ])
}

fn test_single_topic_routes_to_one_sink() {
    println!("Test: 'summary/save' goes to summary sink only");

    let mut r = build_default_router();
    let delivered = r.route(&Event::new("summary/save", r#"{"topic":"x"}"#));
    assert_eq!(delivered, 1);
    assert_eq!(r.bytes_through(SinkKind::Summary), 13);
    assert_eq!(r.bytes_through(SinkKind::Mqtt), 0);
    assert_eq!(r.bytes_through(SinkKind::Email), 0);

    println!("  ✅ summary/save → summary (1 sink, 13 bytes)");
}

fn test_wildcard_topic_routes_to_multiple_sinks() {
    println!("Test: 'magent/alert/critical' hits MQTT + Email");

    let mut r = build_default_router();
    let delivered = r.route(&Event::new(
        "magent/alert/critical",
        r#"{"bpm":160}"#,
    ));
    // MQTT matches ("magent/" prefix) AND Email matches (literal topic).
    assert_eq!(delivered, 2);
    assert!(r.bytes_through(SinkKind::Mqtt) > 0);
    assert!(r.bytes_through(SinkKind::Email) > 0);
    assert_eq!(r.bytes_through(SinkKind::Summary), 0);

    println!(
        "  ✅ magent/alert/critical → mqtt({}B) + email({}B)",
        r.bytes_through(SinkKind::Mqtt),
        r.bytes_through(SinkKind::Email)
    );
}

fn test_summary_sink_receives_only_persistent_topics() {
    println!("Test: summary sink only consumes 'summary/*'");

    let mut r = build_default_router();

    // A burst of events: 3 persistent, 2 transient.
    let events = vec![
        Event::new("summary/save", "s1"),
        Event::new("magent/health/normal", "n1"),
        Event::new("summary/load", "l1"),
        Event::new("magent/health/elevated", "e1"),
        Event::new("summary/delete", "d1"),
    ];
    for e in &events {
        r.route(e);
    }

    // Mqtt: 2 (magent/*) — n1 + e1
    // Email: 0 (none matched the literal critical topic)
    // Summary: 3 (save/load/delete) — s1 + l1 + d1
    assert_eq!(r.bytes_through(SinkKind::Mqtt), 4);
    assert_eq!(r.bytes_through(SinkKind::Email), 0);
    assert_eq!(r.bytes_through(SinkKind::Summary), 6);

    println!(
        "  ✅ 5 events → mqtt(2) + email(0) + summary(3)"
    );
}

fn test_routing_is_deterministic_for_same_topic() {
    println!("Test: same topic → same set of sinks");

    let mut r = build_default_router();
    let e1 = Event::new("magent/alert/critical", "x");
    let e2 = Event::new("magent/alert/critical", "y");

    let d1 = r.route(&e1);
    let d2 = r.route(&e2);
    assert_eq!(d1, d2);
    assert_eq!(d1, 2, "alert/critical topic hits exactly 2 sinks");

    println!("  ✅ deterministic across replays");
}

fn test_no_match_drops_event() {
    println!("Test: no-match topic is dropped (0 sinks)");

    let mut r = build_default_router();
    let delivered = r.route(&Event::new("system/heartbeat", "tick"));
    assert_eq!(delivered, 0);
    assert_eq!(r.bytes_through(SinkKind::Mqtt), 0);
    assert_eq!(r.bytes_through(SinkKind::Email), 0);
    assert_eq!(r.bytes_through(SinkKind::Summary), 0);

    println!("  ✅ system/heartbeat → 0 sinks (dropped)");
}
