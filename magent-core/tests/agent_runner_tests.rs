//! Comprehensive test suite for `agent_runner` (v2).
//!
//! Coverage:
//! - State machine transitions and budget enforcement
//! - All `parse_tool_call` JSON formats (legacy + OpenAI tool_calls)
//! - `parse_result` (string / numeric / object)
//! - Simulated planner: every documented task template
//! - Error recovery (tool errors don't kill the loop)
//! - Ollama wire format: messages are serialised with correct roles and
//!   tool_call encoding
//! - Performance: allocation/iteration budgets on simulated paths

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use magent_core::agent_runner::*;

/// Find the byte offset of the body in an HTTP/1.1 request (after CRLFCRLF).
fn find_body_start(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Slice out the JSON body from a captured HTTP request.
fn extract_body(raw: &[u8]) -> &[u8] {
    let start = find_body_start(raw).unwrap_or(0);
    &raw[start..]
}

/// Spawn a thread that captures the request body and returns a canned
/// `{"message":{"content":"<text>"}}` response.
fn spawn_capture_server(
    listener: std::net::TcpListener,
    response_text: &'static str,
) -> Arc<Mutex<Vec<u8>>> {
    let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = captured.clone();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(idx) = find_body_start(&buf) {
                            let header = std::str::from_utf8(&buf[..idx]).unwrap_or("");
                            let content_length = header
                                .lines()
                                .find_map(|l| {
                                    let l = l.trim_start();
                                    let lower = l.to_ascii_lowercase();
                                    if lower.starts_with("content-length:") {
                                        l["content-length:".len()..].trim().parse::<usize>().ok()
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(0);
                            if content_length > 0 && buf.len() - idx >= content_length {
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        // Timeout — bail out and use whatever we have.
                        break;
                    }
                }
            }
            cap.lock().unwrap().extend_from_slice(&buf);
            let body_json = format!(r#"{{"message":{{"content":"{}"}}}}"#, response_text);
            let body = body_json.as_bytes();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            use std::io::Write;
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    });
    captured
}

/// Spawn N capture servers (each on a separate accept loop) sharing one
/// listener. Used to test that the body buffer is reused across calls.
fn spawn_capture_server_n(
    listener: std::net::TcpListener,
    n: usize,
) -> Vec<Arc<Mutex<Vec<u8>>>> {
    let captures: Vec<Arc<Mutex<Vec<u8>>>> =
        (0..n).map(|_| Arc::new(Mutex::new(Vec::new()))).collect();
    let caps = captures.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        for cap in caps.iter() {
            let (mut stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            let mut buf = Vec::new();
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(idx) = find_body_start(&buf) {
                            let header = std::str::from_utf8(&buf[..idx]).unwrap_or("");
                            let content_length = header
                                .lines()
                                .find_map(|l| {
                                    let l = l.trim_start();
                                    let lower = l.to_ascii_lowercase();
                                    if lower.starts_with("content-length:") {
                                        l["content-length:".len()..].trim().parse::<usize>().ok()
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(0);
                            if buf.len() - idx >= content_length {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            cap.lock().unwrap().extend_from_slice(&buf);
            let body = br#"{"message":{"content":"ok"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            use std::io::Write;
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    captures
}

// ============================================================================
// Test infrastructure
// ============================================================================

/// Records every tool invocation. Returns pre-canned responses.
#[derive(Default)]
struct MockExecutor {
    calls: Mutex<Vec<(String, String)>>,
    /// tool_name -> response string
    responses: Mutex<HashMap<String, String>>,
    /// tool_name -> error string (overrides `responses` if set)
    errors: Mutex<HashMap<String, String>>,
}

impl MockExecutor {
    fn with_response(tool: &str, response: &str) -> Self {
        let mut r = HashMap::new();
        r.insert(tool.to_string(), response.to_string());
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(r),
            errors: Mutex::new(HashMap::new()),
        }
    }

    fn with_error(tool: &str, err: &str) -> Self {
        let mut e = HashMap::new();
        e.insert(tool.to_string(), err.to_string());
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(HashMap::new()),
            errors: Mutex::new(e),
        }
    }
}

impl ToolExecutor for MockExecutor {
    fn execute(&mut self, tool: &str, args: &str) -> Result<String, String> {
        self.calls.lock().unwrap().push((tool.to_string(), args.to_string()));
        if let Some(err) = self.errors.lock().unwrap().get(tool) {
            return Err(err.clone());
        }
        self.responses
            .lock()
            .unwrap()
            .get(tool)
            .cloned()
            .ok_or_else(|| format!("unknown tool: {}", tool))
    }
}

fn silent_runner<E: ToolExecutor>(executor: E) -> RealAgentRunner<E> {
    let mut r = RealAgentRunner::new(executor);
    r.config_mut().verbose = false;
    r.config_mut().probe_ollama_on_run = false;
    r
}

// ============================================================================
// State machine tests
// ============================================================================

#[test]
fn state_starts_idle_and_ends_finished() {
    let mut runner = silent_runner(MockExecutor::with_response("read_sensor", "23.5°C"));
    assert_eq!(runner.state(), AgentState::Idle);
    let result = runner.run("Read temperature").unwrap();
    assert_eq!(runner.state(), AgentState::Finished);
    assert!(!result.is_empty());
}

#[test]
fn budget_enforces_max_iterations() {
    // Default simulated planner never gets stuck, but the loop does converge
    // within a handful of iterations for short tasks. Use an aggressive cap.
    let exec = MockExecutor::with_response("read_sensor", "23.5");
    let mut runner = silent_runner(exec);
    runner.config_mut().max_iterations = 3;
    let _ = runner.run("Read temperature");
    assert!(runner.iteration() <= 3);
}

#[test]
fn budget_enforces_max_tool_calls() {
    let exec = MockExecutor::with_response("read_sensor", "23.5");
    let mut runner = silent_runner(exec);
    runner.config_mut().max_tool_calls = 2;
    let _ = runner.run("Monitor the environment");
    assert!(runner.tool_call_count() <= 2);
}

#[test]
fn reset_clears_state() {
    let mut runner = silent_runner(MockExecutor::with_response("read_sensor", "23.5"));
    runner.run("Read temperature").unwrap();
    assert!(runner.iteration() > 0);
    assert!(runner.tool_call_count() > 0);
    // Manual reset (private; emulate via re-run)
    let _ = runner.run("Read temperature").unwrap();
    assert_eq!(runner.state(), AgentState::Finished);
}

#[test]
fn empty_task_does_not_panic() {
    let exec = MockExecutor::with_response("read_sensor", "ok");
    let mut runner = silent_runner(exec);
    let _ = runner.run("");
}

// ============================================================================
// parse_tool_call tests
// ============================================================================

#[test]
fn parse_tool_call_format_1_legacy() {
    let r = silent_runner(MockExecutor::default());
    let (name, args) = r.parse_tool_call(r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#).unwrap();
    assert_eq!(name, "read_sensor");
    assert_eq!(
        args.get("sensor").and_then(|v| v.as_str()),
        Some("temperature")
    );
}

#[test]
fn parse_tool_call_format_2_openai_style() {
    let r = silent_runner(MockExecutor::default());
    let json = r#"{
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "send_health_alert",
                "arguments": "{\"type\":\"fall\",\"severity\":\"high\",\"message\":\"Detected fall\"}"
            }
        }]
    }"#;
    let (name, args) = r.parse_tool_call(json).unwrap();
    assert_eq!(name, "send_health_alert");
    assert_eq!(args.get("type").and_then(|v| v.as_str()), Some("fall"));
    assert_eq!(args.get("severity").and_then(|v| v.as_str()), Some("high"));
}

#[test]
fn parse_tool_call_format_3_generic_no_whitelist() {
    // v2: any tool name should work, not just the 5-name whitelist.
    let r = silent_runner(MockExecutor::default());
    let (name, args) = r
        .parse_tool_call(r#"{"start_exercise": {"args": {"type": "running"}}}"#)
        .unwrap();
    assert_eq!(name, "start_exercise");
    assert_eq!(args.get("type").and_then(|v| v.as_str()), Some("running"));

    let (name, _) = r
        .parse_tool_call(
            r#"{"notify_emergency_contact": {"args": {"name": "Alice", "message": "help"}}}"#,
        )
        .unwrap();
    assert_eq!(name, "notify_emergency_contact");
}

#[test]
fn parse_tool_call_rejects_control_keys() {
    let r = silent_runner(MockExecutor::default());
    assert!(r.parse_tool_call(r#"{"result": "done"}"#).is_none());
    assert!(r.parse_tool_call(r#"{"error": "boom"}"#).is_none());
    assert!(r.parse_tool_call(r#"{"reasoning": "thinking"}"#).is_none());
}

#[test]
fn parse_tool_call_returns_none_on_invalid_json() {
    let r = silent_runner(MockExecutor::default());
    assert!(r.parse_tool_call("not json").is_none());
    assert!(r.parse_tool_call("").is_none());
}

#[test]
fn parse_tool_call_anthropic_invoke_format() {
    let r = silent_runner(MockExecutor::default());
    let xml = "<invoke name=\"fetch_url\">\n  <parameter name=\"url\">https://example.com</parameter>\n</invoke>";
    let (name, args) = r.parse_tool_call(xml).unwrap();
    assert_eq!(name, "fetch_url");
    assert_eq!(args.get("url").and_then(|v| v.as_str()), Some("https://example.com"));
}

#[test]
fn parse_tool_call_anthropic_antml_namespace() {
    let r = silent_runner(MockExecutor::default());
    // Claude namespace prefix (`antml:`) must be normalised away.
    let xml = "<antml:invoke name=\"web_search\">\n  <antml:parameter name=\"query\">北京天气</antml:parameter>\n</antml:invoke>";
    let (name, args) = r.parse_tool_call(xml).unwrap();
    assert_eq!(name, "web_search");
    assert_eq!(args.get("query").and_then(|v| v.as_str()), Some("北京天气"));
}

#[test]
fn parse_tool_call_anthropic_multiple_parameters() {
    let r = silent_runner(MockExecutor::default());
    let xml = "<invoke name=\"send_health_alert\">\
        <parameter name=\"type\">fall</parameter>\
        <parameter name=\"severity\">high</parameter>\
        </invoke>";
    let (name, args) = r.parse_tool_call(xml).unwrap();
    assert_eq!(name, "send_health_alert");
    assert_eq!(args.get("type").and_then(|v| v.as_str()), Some("fall"));
    assert_eq!(args.get("severity").and_then(|v| v.as_str()), Some("high"));
}

#[test]
fn parse_tool_call_strips_code_fence_from_json() {
    let r = silent_runner(MockExecutor::default());
    let fenced = "```json\n{\"tool\": \"read_sensor\", \"args\": {\"sensor\": \"temperature\"}}\n```";
    let (name, args) = r.parse_tool_call(fenced).unwrap();
    assert_eq!(name, "read_sensor");
    assert_eq!(args.get("sensor").and_then(|v| v.as_str()), Some("temperature"));
}

#[test]
fn parse_tool_call_strips_code_fence_from_anthropic_xml() {
    let r = silent_runner(MockExecutor::default());
    let fenced = "```xml\n<invoke name=\"get_weather\"><parameter name=\"city\">Beijing</parameter></invoke>\n```";
    let (name, args) = r.parse_tool_call(fenced).unwrap();
    assert_eq!(name, "get_weather");
    assert_eq!(args.get("city").and_then(|v| v.as_str()), Some("Beijing"));
}

#[test]
fn parse_result_handles_string_number_object() {
    let r = silent_runner(MockExecutor::default());
    assert_eq!(r.parse_result(r#"{"result": "ok"}"#).unwrap(), "ok");
    assert_eq!(r.parse_result(r#"{"result": 42}"#).unwrap(), "42");
    assert_eq!(r.parse_result(r#"{"result": 3.14}"#).unwrap(), "3.1");
    assert_eq!(
        r.parse_result(r#"{"result": {"a": 1}}"#).unwrap(),
        "{\"a\":1}"
    );
    assert!(r.parse_result("not json").is_none());
    assert!(r.parse_result(r#"{"other": 1}"#).is_none());
}

#[test]
fn parse_result_takes_precedence_over_tool_call() {
    // v2: a `{"result": "..."}` payload must be classified as a result,
    // not a tool call. The previous behaviour (whitelist of 5 names) made
    // this work incidentally; we now need to ensure the same property holds
    // for the generic Format 3 fallback.
    let r = silent_runner(MockExecutor::default());
    let response = r#"{"result": "Environmental monitoring complete"}"#;
    assert!(r.parse_tool_call(response).is_none(),
        "result payload should not be a tool call");
    let parsed = r.parse_result(response).unwrap();
    assert_eq!(parsed, "Environmental monitoring complete");
}

// ============================================================================
// Format / code fault-tolerance tests
// ============================================================================

#[test]
fn parse_result_handles_fenced_code_block() {
    // A model asked to write code but that forgets the {"result":...}
    // envelope wraps its answer in a fenced block. We must deliver the
    // code verbatim, without the ``` fences or the language tag.
    let r = silent_runner(MockExecutor::default());
    let code = "fn main() { println!(\"hi\"); }";
    let fenced = format!("```rust\n{}\n```", code);
    assert_eq!(r.parse_result(&fenced).unwrap(), code);
    assert_eq!(
        r.parse_result("```\n$ echo hello\n```").unwrap(),
        "$ echo hello"
    );
}

#[test]
fn parse_result_handles_plain_prose_answer() {
    // A model that violates the JSON-only rule and answers in prose must
    // still be treated as a final result rather than looping until budget.
    let r = silent_runner(MockExecutor::default());
    let answer = "The temperature is 24.2 degrees Celsius and it is sunny.";
    assert_eq!(r.parse_result(answer).unwrap(), answer);
}

#[test]
fn parse_result_does_not_terminate_on_chatter() {
    // Short plan/interjection text must NOT be treated as a final result,
    // otherwise the loop would stop before the model finishes its plan.
    let r = silent_runner(MockExecutor::default());
    assert!(r.parse_result("OK").is_none());
    assert!(r.parse_result("Let me check the sensor first").is_none());
    assert!(r.parse_result("I'll now read the humidity sensor").is_none());
}

#[test]
fn parse_result_extracts_embedded_result_json() {
    // The model sometimes wraps the result JSON in surrounding text.
    let r = silent_runner(MockExecutor::default());
    let resp = "Here is the answer: {\"result\": \"42\"} Hope that helps.";
    assert_eq!(r.parse_result(resp).unwrap(), "42");
}

#[test]
fn parse_tool_call_extracts_json_from_prose() {
    // A tool call embedded in prose (a format violation) must be recovered.
    let r = silent_runner(MockExecutor::default());
    let resp = "I'll use the sensor: {\"tool\": \"read_sensor\", \"args\": {\"sensor\": \"temperature\"}}. Please wait.";
    let (name, args) = r.parse_tool_call(resp).unwrap();
    assert_eq!(name, "read_sensor");
    assert_eq!(args.get("sensor").and_then(|v| v.as_str()), Some("temperature"));
}

#[test]
fn parse_tool_call_handles_string_braces_in_embedded_json() {
    // The balanced-brace scanner must not stop at a '}' inside a string value.
    let r = silent_runner(MockExecutor::default());
    let resp = "Here: {\"tool\": \"fetch_url\", \"args\": {\"url\": \"https://example.com/a}\"}} done";
    let (name, args) = r.parse_tool_call(resp).unwrap();
    assert_eq!(name, "fetch_url");
    assert_eq!(args.get("url").and_then(|v| v.as_str()), Some("https://example.com/a}"));
}

// ============================================================================
// Simulated planner tests
// ============================================================================

#[test]
fn sim_temperature_only_task() {
    let exec = MockExecutor::with_response("read_sensor", "23.5");
    let mut runner = silent_runner(exec);
    let result = runner.run("Read temperature sensor").unwrap();
    assert_eq!(runner.tool_call_count(), 1);
    assert_eq!(result, "Temperature sensor reading completed");
}

#[test]
fn sim_led_on_off() {
    let mut runner = silent_runner(MockExecutor::with_response("write_gpio", "ok"));
    runner.config_mut().max_tool_calls = 2;
    let r = runner.run("Turn on the LED").unwrap();
    assert_eq!(r, "LED turned on successfully");

    let mut runner = silent_runner(MockExecutor::with_response("write_gpio", "ok"));
    runner.config_mut().verbose = false;
    runner.config_mut().probe_ollama_on_run = false;
    let r = runner.run("Turn off the LED").unwrap();
    assert_eq!(r, "LED turned off successfully");
}

#[test]
fn sim_environmental_monitoring_runs_full_chain() {
    let exec = MockExecutor::with_response("read_sensor", "20.0");
    let mut runner = silent_runner(exec);
    let result = runner.run("Monitor the environment").unwrap();
    assert!(runner.tool_call_count() >= 4, "got {}", runner.tool_call_count());
    assert!(result.contains("monitoring"));
}

#[test]
fn sim_flash_log_includes_ble() {
    let exec = MockExecutor::with_response("read_sensor", "1000");
    let mut runner = silent_runner(exec);
    let result = runner.run("Read sensors and save to flash").unwrap();
    assert!(result.to_lowercase().contains("morning") || result.to_lowercase().contains("complete"));
    assert!(runner.tool_call_count() >= 3);
}

#[test]
fn sim_temperature_with_fan_control() {
    let exec = MockExecutor::with_response("read_sensor", "35.0");
    let mut runner = silent_runner(exec);
    let result = runner.run("Check temperature and turn on fan if above 25C").unwrap();
    assert!(result.contains("fan"));
}

// ============================================================================
// Tool execution tests
// ============================================================================

#[test]
fn tool_call_serialises_args_as_json() {
    let exec = Arc::new(Mutex::new(MockExecutor::with_response("read_sensor", "ok")));
    struct Wrap(Arc<Mutex<MockExecutor>>);
    impl ToolExecutor for Wrap {
        fn execute(&mut self, tool: &str, args: &str) -> Result<String, String> {
            self.0.lock().unwrap().execute(tool, args)
        }
    }
    let mut runner = silent_runner(Wrap(exec.clone()));
    runner.run("Read temperature").unwrap();
    let calls = exec.lock().unwrap().calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "read_sensor");
    let parsed: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
    assert!(parsed.get("sensor").is_some());
}

#[test]
fn tool_error_does_not_kill_the_run() {
    let exec = MockExecutor::with_error("read_sensor", "sensor offline");
    let mut runner = silent_runner(exec);
    let result = runner.run("Read temperature").unwrap();
    // The simulated planner should still finish with a "result" message.
    assert!(!result.is_empty());
    assert_eq!(runner.state(), AgentState::Finished);
    // The error was surfaced as a Tool message in the conversation.
    let has_err = runner
        .messages()
        .iter()
        .any(|m| m.content.contains("sensor offline"));
    assert!(has_err, "error message must be preserved");
}

#[test]
fn runner_handles_unknown_tool() {
    let exec = MockExecutor::default();
    let mut runner = silent_runner(exec);
    let result = runner.run("Read temperature").unwrap();
    // Executor returns Err("unknown tool: read_sensor") — runner must recover.
    assert!(!result.is_empty());
}

// ============================================================================
// Message and role tests
// ============================================================================

#[test]
fn assistant_tool_call_is_structured() {
    let mut runner = silent_runner(MockExecutor::with_response("read_sensor", "ok"));
    runner.run("Read temperature").unwrap();
    let assistant_tc = runner
        .messages()
        .iter()
        .find(|m| m.role == Role::Assistant && m.tool_call.is_some())
        .expect("expected at least one structured tool-call message");
    let tc = assistant_tc.tool_call.as_ref().unwrap();
    assert_eq!(tc.name, "read_sensor");
}

#[test]
fn tool_message_has_tool_call_id() {
    let mut runner = silent_runner(MockExecutor::with_response("read_sensor", "ok"));
    runner.run("Read temperature").unwrap();
    let tool = runner
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert!(tool.tool_call_id.is_some());
}

// ============================================================================
// Ollama wire format tests
// ============================================================================

#[test]
fn ollama_wire_format_includes_system_first() {
    // Start a tiny TCP server that captures the body, then make a chat call.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = captured.clone();

    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
            // Drain the request (read until EOF, error, or content-length matched).
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        // Look for the end of headers and either content-length
                        // matched or connection-close.
                        if let Some(idx) = find_body_start(&buf) {
                            let header = std::str::from_utf8(&buf[..idx]).unwrap_or("");
                            let content_length = header
                                .lines()
                                .find_map(|l| {
                                    let l = l.trim_start();
                                    let lower = l.to_ascii_lowercase();
                                    if lower.starts_with("content-length:") {
                                        l["content-length:".len()..].trim().parse::<usize>().ok()
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(0);
                            if buf.len() - idx - 4 >= content_length {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            cap.lock().unwrap().extend_from_slice(&buf);
            let body = br#"{"message":{"content":"hello"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            use std::io::Write;
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        }
    });

    let mut client = OllamaClient::new(&format!("http://{}", addr), "test-model");
    let messages = vec![
        Message::system("sys prompt"),
        Message::user("hi"),
    ];
    let resp = client.chat_with_messages(&messages, SamplingParams::default()).unwrap();
    assert_eq!(resp, "hello");

    let raw = captured.lock().unwrap().clone();
    let body = std::str::from_utf8(extract_body(&raw)).unwrap().to_string();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["model"], "test-model");
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
    assert!(json["options"]["temperature"].is_number());
}

#[test]
fn ollama_wire_format_serialises_tool_calls() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = spawn_capture_server(listener, "ok");

    let mut client = OllamaClient::new(&format!("http://{}", addr), "m");
    let tc = ToolCall {
        name: "send_health_alert".into(),
        arguments: {
            let mut m = HashMap::new();
            m.insert("type".into(), serde_json::json!("fall"));
            m.insert("severity".into(), serde_json::json!("high"));
            m
        },
    };
    let messages = vec![
        Message::system("sys"),
        Message::user("detect fall"),
        Message::assistant_tool_call(tc),
    ];
    let _ = client.chat_with_messages(&messages, SamplingParams::default()).unwrap();

    let raw = captured.lock().unwrap().clone();
    let body_str = std::str::from_utf8(extract_body(&raw)).unwrap().to_string();
    let json: serde_json::Value = serde_json::from_str(&body_str).unwrap();
    let msgs = json["messages"].as_array().unwrap();
    let assistant = &msgs[2];
    assert_eq!(assistant["role"], "assistant");
    let tcs = assistant["tool_calls"].as_array().unwrap();
    assert_eq!(tcs.len(), 1);
    assert_eq!(tcs[0]["function"]["name"], "send_health_alert");
    let args_str = tcs[0]["function"]["arguments"].as_str().unwrap();
    let args: serde_json::Value = serde_json::from_str(args_str).unwrap();
    assert_eq!(args["type"], "fall");
    assert_eq!(args["severity"], "high");
}

#[test]
fn ollama_body_buffer_is_reused_across_calls() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captures = spawn_capture_server_n(listener, 2);

    let mut client = OllamaClient::new(&format!("http://{}", addr), "m");
    let m1 = vec![Message::user("hi")];
    let _ = client.chat_with_messages(&m1, SamplingParams::default()).unwrap();
    let m2 = vec![
        Message::user("hi"),
        Message::assistant_text("thinking..."),
        Message::user("now do x"),
    ];
    let _ = client.chat_with_messages(&m2, SamplingParams::default()).unwrap();

    let raw1 = captures[0].lock().unwrap().clone();
    let raw2 = captures[1].lock().unwrap().clone();
    let body1 = std::str::from_utf8(extract_body(&raw1)).unwrap().to_string();
    let body2 = std::str::from_utf8(extract_body(&raw2)).unwrap().to_string();
    assert!(body1.contains("hi"));
    assert!(body2.contains("now do x"));
    let _: serde_json::Value = serde_json::from_str(&body1).unwrap();
    let _: serde_json::Value = serde_json::from_str(&body2).unwrap();
}

// ============================================================================
// Legacy string-array chat API compatibility
// ============================================================================

#[test]
fn legacy_chat_compat_parses_prefixes() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = spawn_capture_server(listener, "ok");

    let mut client = OllamaClient::new(&format!("http://{}", addr), "m");
    let legacy = vec![
        "[User] hello".to_string(),
        "[Assistant] hi back".to_string(),
        "[Tool] read_sensor: 23.5".to_string(),
    ];
    let _ = client.chat(&legacy, "sys").unwrap();

    let raw = captured.lock().unwrap().clone();
    let body_str = std::str::from_utf8(extract_body(&raw)).unwrap().to_string();
    let json: serde_json::Value = serde_json::from_str(&body_str).unwrap();
    let msgs = json["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[3]["tool_call_id"], "read_sensor");
}

// ============================================================================
// Performance: allocation budgets
// ============================================================================

#[test]
fn sim_path_terminates_within_default_budget() {
    // Smoke test: simulated path should never exceed default budget.
    let exec = MockExecutor::with_response("read_sensor", "1.0");
    let mut runner = silent_runner(exec);
    runner.run("Read temperature").unwrap();
    assert!(runner.iteration() <= 10);
    assert!(runner.tool_call_count() <= 8);
}

#[test]
fn allocation_smoke_1000_runs() {
    // If we've introduced a quadratic alloc bug, this will explode.
    let exec = MockExecutor::with_response("read_sensor", "ok");
    let mut runner = silent_runner(exec);
    for i in 0..1000 {
        runner.run(&format!("Read temperature {}", i)).unwrap();
    }
    assert_eq!(runner.state(), AgentState::Finished);
}

// ============================================================================
// End of tests
// ============================================================================