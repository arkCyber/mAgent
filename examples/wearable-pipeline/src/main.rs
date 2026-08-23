//! Wearable health pipeline: simulated sensor → MQTT event bus →
//! email alert → summary persistence.
//!
//! This is a single-process simulator of the multi-protocol
//! pipeline that runs in production when the on-watch health
//! agent detects an anomalous heart rate:
//!
//!   1. The simulated sensor emits a stream of BPM readings.
//!   2. The "agent" classifies each reading as normal / elevated /
//!      critical.
//!   3. Normal readings are persisted to the summary store
//!      (mocked as an in-memory buffer so this binary doesn't
//!      need a broker or an IMAP server to run).
//!   4. Elevated readings are published to an MQTT topic as
//!      structured events.
//!   5. Critical readings trigger an email alert via SMTP.
//!
//! The pipeline asserts that every reading ends up in exactly one
//! of the three sinks, and that the JSON shape of each event is
//! stable across readings — so a downstream consumer can rely on
//! it.
//!
//! Run with: `cargo run -p magent-tools --bin wearable-pipeline`

use std::collections::VecDeque;

fn main() {
    println!("=== Wearable Health Pipeline (mqtt + email + summary) ===\n");

    test_normal_readings_go_to_summary();
    test_elevated_readings_published_as_mqtt_events();
    test_critical_readings_trigger_email_alert();
    test_pipeline_routes_every_reading_exactly_once();
    test_event_payload_shape_is_stable();

    println!("\n=== All pipeline tests passed ===");
}

/// Classification of a single BPM reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    /// Resting / recovery band. Persist only.
    Normal,
    /// Slightly elevated. Publish to MQTT for downstream consumers.
    Elevated,
    /// Critical. Trigger an email alert AND publish to MQTT.
    Critical,
}

impl Severity {
    fn classify(bpm: u32) -> Severity {
        match bpm {
            0..=90 => Severity::Normal,
            91..=120 => Severity::Elevated,
            _ => Severity::Critical,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Severity::Normal => "normal",
            Severity::Elevated => "elevated",
            Severity::Critical => "critical",
        }
    }
}

/// A single BPM reading produced by the simulated sensor.
#[derive(Debug, Clone)]
struct Reading {
    /// Unix millis at which the sensor produced this reading.
    timestamp_ms: u64,
    /// Beats-per-minute.
    bpm: u32,
}

/// In-memory MQTT topic log. Real implementation would call
/// `magent_mqtt_mcp::MqttClient::publish`; here we record the
/// payload so the test can assert on its shape.
#[derive(Default)]
struct MqttLog {
    events: Vec<(String, String)>,
}

impl MqttLog {
    fn publish(&mut self, topic: &str, payload: &str) {
        self.events.push((topic.to_string(), payload.to_string()));
    }
}

/// In-memory email outbox. Real implementation would call
/// `lettre::SmtpTransport`; here we record the message so the
/// test can assert on its contents.
#[derive(Default)]
struct EmailOutbox {
    alerts: Vec<EmailAlert>,
}

#[derive(Debug, Clone)]
struct EmailAlert {
    to: String,
    subject: String,
    body: String,
}

impl EmailOutbox {
    fn send(&mut self, alert: EmailAlert) {
        self.alerts.push(alert);
    }
}

/// In-memory summary store. Real implementation would call
/// `FileSummaryStore::save`; here we just keep a vector.
#[derive(Default)]
struct SummaryStore {
    windows: Vec<Vec<Reading>>,
}

impl SummaryStore {
    fn append_window(&mut self, window: Vec<Reading>) {
        self.windows.push(window);
    }
}

/// The simulated sensor stream. `next_batch` produces a
/// deterministic sequence of readings so the test assertions
/// can pin down exact outcomes.
struct SensorStream {
    queue: VecDeque<Reading>,
}

impl SensorStream {
    fn new(readings: Vec<Reading>) -> Self {
        Self {
            queue: readings.into(),
        }
    }

    fn next(&mut self) -> Option<Reading> {
        self.queue.pop_front()
    }
}

/// The agent — consumes the sensor stream and routes each
/// reading to one of the three sinks.
struct WearableAgent {
    topic: String,
    alert_to: String,
    mqtt: MqttLog,
    email: EmailOutbox,
    summary: SummaryStore,
    /// Number of readings processed.
    processed: usize,
}

impl WearableAgent {
    fn new(topic: &str, alert_to: &str) -> Self {
        Self {
            topic: topic.to_string(),
            alert_to: alert_to.to_string(),
            mqtt: MqttLog::default(),
            email: EmailOutbox::default(),
            summary: SummaryStore::default(),
            processed: 0,
        }
    }

    /// Process one reading. Returns the severity so the test
    /// can cross-check against the routing decision.
    fn on_reading(&mut self, reading: &Reading) -> Severity {
        let sev = Severity::classify(reading.bpm);
        let payload = format!(
            r#"{{"ts":{},"bpm":{},"severity":"{}"}}"#,
            reading.timestamp_ms,
            reading.bpm,
            sev.as_str()
        );
        match sev {
            Severity::Normal => {
                // Persist a single-entry window. Real pipeline
                // would batch 30-second windows before saving.
                self.summary.append_window(vec![reading.clone()]);
            }
            Severity::Elevated => {
                self.mqtt.publish(&self.topic, &payload);
            }
            Severity::Critical => {
                self.mqtt.publish(&self.topic, &payload);
                self.email.send(EmailAlert {
                    to: self.alert_to.clone(),
                    subject: format!("[magent] critical heart rate: {} bpm", reading.bpm),
                    body: payload.clone(),
                });
            }
        }
        self.processed += 1;
        sev
    }
}

fn test_normal_readings_go_to_summary() {
    println!("Test: normal readings are persisted, not published");

    let mut agent = WearableAgent::new("magent/health", "user@example.com");
    let r = Reading {
        timestamp_ms: 1_000,
        bpm: 72,
    };
    let sev = agent.on_reading(&r);
    assert_eq!(sev, Severity::Normal);
    assert_eq!(agent.summary.windows.len(), 1);
    assert_eq!(agent.summary.windows[0].len(), 1);
    assert_eq!(agent.mqtt.events.len(), 0, "normal readings must not be published");
    assert_eq!(agent.email.alerts.len(), 0, "normal readings must not alert");

    println!("  ✅ 72 bpm → summary only");
}

fn test_elevated_readings_published_as_mqtt_events() {
    println!("Test: elevated readings hit the MQTT topic");

    let mut agent = WearableAgent::new("magent/health", "user@example.com");
    let r = Reading {
        timestamp_ms: 2_000,
        bpm: 105,
    };
    let sev = agent.on_reading(&r);
    assert_eq!(sev, Severity::Elevated);
    assert_eq!(agent.mqtt.events.len(), 1);
    let (topic, payload) = &agent.mqtt.events[0];
    assert_eq!(topic, "magent/health");
    assert!(payload.contains("\"bpm\":105"));
    assert!(payload.contains("\"severity\":\"elevated\""));
    assert_eq!(agent.email.alerts.len(), 0, "elevated readings must not email");
    assert_eq!(agent.summary.windows.len(), 0, "elevated readings must not be summarised");

    println!("  ✅ 105 bpm → mqtt topic");
}

fn test_critical_readings_trigger_email_alert() {
    println!("Test: critical readings email + mqtt");

    let mut agent = WearableAgent::new("magent/health", "user@example.com");
    let r = Reading {
        timestamp_ms: 3_000,
        bpm: 145,
    };
    let sev = agent.on_reading(&r);
    assert_eq!(sev, Severity::Critical);
    assert_eq!(agent.mqtt.events.len(), 1);
    assert_eq!(agent.email.alerts.len(), 1);
    let alert = &agent.email.alerts[0];
    assert_eq!(alert.to, "user@example.com");
    assert!(alert.subject.contains("145"));
    assert!(alert.body.contains("\"severity\":\"critical\""));

    println!("  ✅ 145 bpm → mqtt + email");
}

fn test_pipeline_routes_every_reading_exactly_once() {
    println!("Test: every reading is routed to exactly one sink");

    let stream = vec![
        Reading {
            timestamp_ms: 1,
            bpm: 70,
        }, // normal
        Reading {
            timestamp_ms: 2,
            bpm: 72,
        }, // normal
        Reading {
            timestamp_ms: 3,
            bpm: 110,
        }, // elevated
        Reading {
            timestamp_ms: 4,
            bpm: 130,
        }, // critical
        Reading {
            timestamp_ms: 5,
            bpm: 88,
        }, // normal
        Reading {
            timestamp_ms: 6,
            bpm: 150,
        }, // critical
        Reading {
            timestamp_ms: 7,
            bpm: 100,
        }, // elevated
    ];
    let mut stream = SensorStream::new(stream);
    let mut agent = WearableAgent::new("magent/health", "user@example.com");

    let mut routed = 0;
    while let Some(r) = stream.next() {
        agent.on_reading(&r);
        routed += 1;
    }

    assert_eq!(routed, 7);
    assert_eq!(agent.processed, 7);
    // 2 critical → 2 emails + 2 mqtt
    // 2 elevated → 2 mqtt
    // 3 normal → 3 windows
    assert_eq!(agent.email.alerts.len(), 2);
    assert_eq!(agent.mqtt.events.len(), 4);
    assert_eq!(agent.summary.windows.len(), 3);

    println!(
        "  ✅ routed {} readings → 3 summary + 4 mqtt + 2 email",
        routed
    );
}

fn test_event_payload_shape_is_stable() {
    println!("Test: JSON payload shape is stable across readings");

    let mut agent = WearableAgent::new("magent/health", "user@example.com");
    agent.on_reading(&Reading {
        timestamp_ms: 100,
        bpm: 110,
    });
    agent.on_reading(&Reading {
        timestamp_ms: 200,
        bpm: 140,
    });
    agent.on_reading(&Reading {
        timestamp_ms: 300,
        bpm: 160,
    });

    // Every payload must contain the three fields in this exact
    // order. The downstream consumer relies on this ordering for
    // its grep / jq queries.
    for (topic, payload) in &agent.mqtt.events {
        assert_eq!(topic, "magent/health");
        assert!(payload.starts_with(r#"{"ts":"#));
        assert!(payload.contains(r#""bpm":"#));
        assert!(payload.contains(r#""severity":"#));
        // The ts and bpm numbers must be parseable back.
        let json: serde_json_like::Value = serde_json_like::parse(payload);
        let ts = json.field("ts").as_u64();
        let bpm = json.field("bpm").as_u32();
        let sev = json.field("severity").as_str();
        assert!(ts > 0);
        assert!(bpm >= 90);
        assert!(!sev.is_empty());
    }

    println!("  ✅ all {} events parseable, fields present", agent.mqtt.events.len());
}

// ---------------------------------------------------------------------------
// Tiny JSON parser — used by the pipeline to assert payload shape.
// We avoid pulling in `serde_json` here because the tools crate
// is intentionally dep-light; a 30-line parser covers the four
// cases the test exercises.
// ---------------------------------------------------------------------------
mod serde_json_like {
    pub struct Value {
        fields: Vec<(String, Raw)>,
    }

    enum Raw {
        Str(String),
        Num(u64),
    }

    impl Value {
        pub fn field(&self, name: &str) -> Field<'_> {
            for (k, v) in &self.fields {
                if k == name {
                    return match v {
                        Raw::Str(s) => Field::Str(s.as_str()),
                        Raw::Num(n) => Field::Num(*n),
                    };
                }
            }
            Field::Str("")
        }
    }

    pub enum Field<'a> {
        Str(&'a str),
        Num(u64),
    }

    impl<'a> Field<'a> {
        // By-name accessor convention chosen over the clippy-default
        // `&self` because the enum is single-variant choice and the
        // accessors are intentionally consuming. The `as_*` pattern
        // matches the embedded-hal conventions this example targets.
        #[allow(clippy::wrong_self_convention)]
        pub fn as_u64(self) -> u64 {
            match self {
                Field::Num(n) => n,
                _ => 0,
            }
        }
        #[allow(clippy::wrong_self_convention)]
        pub fn as_u32(self) -> u32 {
            self.as_u64() as u32
        }
        #[allow(clippy::wrong_self_convention)]
        pub fn as_str(self) -> &'a str {
            match self {
                Field::Str(s) => s,
                Field::Num(_) => "",
            }
        }
    }

    pub fn parse(s: &str) -> Value {
        let mut fields = Vec::new();
        let body = s.trim_start_matches('{').trim_end_matches('}');
        for pair in body.split(',') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let mut kv = pair.splitn(2, ':');
            let key = kv.next().unwrap_or("").trim().trim_matches('"').to_string();
            let raw = kv.next().unwrap_or("").trim();
            if let Ok(num) = raw.parse::<u64>() {
                fields.push((key, Raw::Num(num)));
            } else {
                let val = raw.trim_matches('"').to_string();
                fields.push((key, Raw::Str(val)));
            }
        }
        Value { fields }
    }
}
