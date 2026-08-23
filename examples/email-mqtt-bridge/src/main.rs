//! Email → MQTT bridge: a tiny in-memory SMTP receiver that
//! decodes each incoming message and re-publishes it on an MQTT
//! topic. This mirrors what a real `magent-email-mcp` +
//! `magent-mqtt-mcp` pipeline does on a host: an IMAP poller
//! picks up a message, parses it, and the bridge publishes a
//! structured event so downstream consumers (dashboard, alert
//! router, summary store) can react.
//!
//! We don't actually open a socket — this example validates the
//! message-decoding + topic-routing pipeline that would run on
//! top of any SMTP-receiving library.
//!
//! Run with: `cargo run -p magent-tools --bin email-mqtt-bridge`

fn main() {
    println!("=== Email → MQTT Bridge ===\n");

    test_decode_plain_text_email();
    test_decode_subject_only_summary_event();
    test_decode_multipart_html_is_treated_as_attachment();
    test_topic_routing_by_from_domain();
    test_topic_routing_by_subject_keyword();
    test_bridge_emits_one_mqtt_event_per_email();
    test_payload_shape_is_stable();

    println!("\n=== All bridge tests passed ===");
}

/// A single decoded email.
#[derive(Debug, Clone)]
struct DecodedEmail {
    from: String,
    to: Vec<String>,
    subject: String,
    body_kind: BodyKind,
    body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    /// `text/plain` part.
    Plain,
    /// `text/html` part (we don't render it; we publish the raw
    /// HTML so a downstream consumer can decide).
    Html,
    /// Multipart with no recognised text part; we publish the
    /// raw wire bytes as an opaque attachment reference.
    Opaque,
}

impl BodyKind {
    fn as_str(self) -> &'static str {
        match self {
            BodyKind::Plain => "text/plain",
            BodyKind::Html => "text/html",
            BodyKind::Opaque => "opaque",
        }
    }
}

/// A decoded email → outgoing MQTT event.
#[derive(Debug, Clone)]
struct MqttEvent {
    topic: String,
    payload: String,
}

/// Minimal SMTP-receiver stand-in. In production this is
/// `async-imap::Session::fetch` + `mailparse::parse_mail`.
fn decode(raw: &RawSmtpMessage) -> DecodedEmail {
    DecodedEmail {
        from: raw.from.clone(),
        to: raw.to.clone(),
        subject: raw.subject.clone(),
        body_kind: raw.body_kind,
        body: raw.body.clone(),
    }
}

/// Topic-routing rule: each rule looks at the email and decides
/// which MQTT topic to publish to.
fn route_topic(email: &DecodedEmail, rules: &[RoutingRule]) -> String {
    for rule in rules {
        if (rule.match_from_domain.as_deref())
            .map(|d| email.from.ends_with(&format!("@{}", d)))
            .unwrap_or(true)
            && (rule.match_subject_keyword.as_deref())
                .map(|k| email.subject.to_lowercase().contains(&k.to_lowercase()))
                .unwrap_or(true)
        {
            return rule.topic.clone();
        }
    }
    "magent/email/inbox".to_string()
}

#[derive(Debug, Clone)]
struct RoutingRule {
    /// Match emails whose `From:` ends with `@<domain>`. `None`
    /// matches any sender.
    match_from_domain: Option<String>,
    /// Match emails whose subject contains this substring
    /// (case-insensitive). `None` matches any subject.
    match_subject_keyword: Option<String>,
    /// Topic to publish matching emails to.
    topic: String,
}

/// In-memory SMTP "message" — what the bridge receives from
/// the upstream listener.
#[derive(Debug, Clone)]
struct RawSmtpMessage {
    from: String,
    to: Vec<String>,
    subject: String,
    body_kind: BodyKind,
    body: String,
}

impl RawSmtpMessage {
    fn plain(from: &str, to: &[&str], subject: &str, body: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.iter().map(|s| s.to_string()).collect(),
            subject: subject.to_string(),
            body_kind: BodyKind::Plain,
            body: body.to_string(),
        }
    }
    fn html(from: &str, to: &[&str], subject: &str, body: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.iter().map(|s| s.to_string()).collect(),
            subject: subject.to_string(),
            body_kind: BodyKind::Html,
            body: body.to_string(),
        }
    }
    fn opaque(from: &str, to: &[&str], subject: &str, body: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.iter().map(|s| s.to_string()).collect(),
            subject: subject.to_string(),
            body_kind: BodyKind::Opaque,
            body: body.to_string(),
        }
    }
}

/// The bridge. Receives raw SMTP messages and emits one
/// `MqttEvent` per message.
struct Bridge {
    rules: Vec<RoutingRule>,
    mqtt_log: Vec<MqttEvent>,
}

impl Bridge {
    fn new(rules: Vec<RoutingRule>) -> Self {
        Self {
            rules,
            mqtt_log: Vec::new(),
        }
    }

    fn ingest(&mut self, raw: RawSmtpMessage) {
        let decoded = decode(&raw);
        let topic = route_topic(&decoded, &self.rules);
        let payload = format!(
            r#"{{"from":"{}","to":"{}","subject":"{}","body_kind":"{}","body":{}}}"#,
            decoded.from,
            decoded.to.join(","),
            decoded.subject,
            decoded.body_kind.as_str(),
            // Wrap the body in a JSON string. `mailparse` returns
            // the raw body, so we just escape the quotes for the
            // bridge payload.
            serde_json_escape(&decoded.body),
        );
        self.mqtt_log.push(MqttEvent { topic, payload });
    }

    fn events(&self) -> &[MqttEvent] {
        &self.mqtt_log
    }
}

fn serde_json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn test_decode_plain_text_email() {
    println!("Test: plain-text email decodes with body intact");

    let raw = RawSmtpMessage::plain(
        "alice@example.com",
        &["bob@example.com"],
        "Heart rate alert",
        "Patient's BPM is 145.",
    );
    let decoded = decode(&raw);
    assert_eq!(decoded.from, "alice@example.com");
    assert_eq!(decoded.subject, "Heart rate alert");
    assert_eq!(decoded.body_kind, BodyKind::Plain);
    assert_eq!(decoded.body, "Patient's BPM is 145.");

    println!("  ✅ plain text → decoded");
}

fn test_decode_subject_only_summary_event() {
    println!("Test: subject-only email keeps both header and body");

    let raw = RawSmtpMessage::plain(
        "noreply@magent.local",
        &["user@magent.local"],
        "[magent] summary saved",
        "",
    );
    let decoded = decode(&raw);
    assert_eq!(decoded.subject, "[magent] summary saved");
    assert_eq!(decoded.body, "");

    println!("  ✅ subject-only → decoded (empty body)");
}

fn test_decode_multipart_html_is_treated_as_attachment() {
    println!("Test: HTML-only email is recorded as text/html, body preserved");

    let raw = RawSmtpMessage::html(
        "noreply@example.com",
        &["user@example.com"],
        "Daily report",
        "<html><body>hi</body></html>",
    );
    let decoded = decode(&raw);
    assert_eq!(decoded.body_kind, BodyKind::Html);
    assert!(decoded.body.contains("<html>"));

    let opaque = RawSmtpMessage::opaque("noreply@example.com", &["u"], "x", "binary");
    let opaque_decoded = decode(&opaque);
    assert_eq!(opaque_decoded.body_kind, BodyKind::Opaque);

    println!("  ✅ html + opaque body kinds decoded");
}

fn test_topic_routing_by_from_domain() {
    println!("Test: routing rule matches by sender domain");

    let rules = vec![
        RoutingRule {
            match_from_domain: Some("alert.example.com".into()),
            match_subject_keyword: None,
            topic: "magent/email/alerts".into(),
        },
        RoutingRule {
            match_from_domain: None,
            match_subject_keyword: None,
            topic: "magent/email/inbox".into(),
        },
    ];

    let mut b = Bridge::new(rules);
    b.ingest(RawSmtpMessage::plain(
        "noreply@alert.example.com",
        &["user@example.com"],
        "alert",
        "ping",
    ));
    b.ingest(RawSmtpMessage::plain(
        "alice@personal.example.com",
        &["user@example.com"],
        "hi",
        "hello",
    ));
    assert_eq!(b.events()[0].topic, "magent/email/alerts");
    assert_eq!(b.events()[1].topic, "magent/email/inbox");

    println!("  ✅ sender domain → routing rule");
}

fn test_topic_routing_by_subject_keyword() {
    println!("Test: routing rule matches by subject keyword");

    let rules = vec![RoutingRule {
        match_from_domain: None,
        match_subject_keyword: Some("critical".into()),
        topic: "magent/email/critical".into(),
    }];
    let mut b = Bridge::new(rules);
    b.ingest(RawSmtpMessage::plain(
        "a@x",
        &["b@x"],
        "CRITICAL: out of disk",
        "",
    ));
    b.ingest(RawSmtpMessage::plain(
        "a@x",
        &["b@x"],
        "info: scheduled maintenance",
        "",
    ));
    assert_eq!(b.events()[0].topic, "magent/email/critical");
    // Second one matches no rule, falls back to default.
    assert_eq!(b.events()[1].topic, "magent/email/inbox");

    println!("  ✅ subject keyword → routing rule");
}

fn test_bridge_emits_one_mqtt_event_per_email() {
    println!("Test: one ingest → one MQTT event");

    let mut b = Bridge::new(vec![]);
    for i in 0..5 {
        b.ingest(RawSmtpMessage::plain(
            "a@x",
            &["b@x"],
            &format!("msg {}", i),
            "body",
        ));
    }
    assert_eq!(b.events().len(), 5);

    println!("  ✅ 5 emails → 5 mqtt events");
}

fn test_payload_shape_is_stable() {
    println!("Test: payload contains the five expected fields");

    let mut b = Bridge::new(vec![]);
    b.ingest(RawSmtpMessage::plain(
        "alice@example.com",
        &["bob@example.com"],
        "Alert",
        "Body with \"quotes\" and \\backslashes\\.",
    ));
    let payload = &b.events()[0].payload;
    for field in ["from", "to", "subject", "body_kind", "body"] {
        assert!(payload.contains(&format!("\"{}\":", field)), "missing field: {}", field);
    }
    // Quotes must be escaped in the body JSON.
    assert!(payload.contains("\\\"quotes\\\""));
    // Body kind is the wire form.
    assert!(payload.contains("\"body_kind\":\"text/plain\""));

    println!("  ✅ payload has 5 stable fields + escaped quotes");
}
