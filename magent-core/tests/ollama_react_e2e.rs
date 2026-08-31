//! End-to-end ReAct-loop test against a *real* mock HTTP server.
//!
//! Run with:
//!
//! ```sh
//! cargo test -p magent-core --features std --test ollama_react_e2e
//! ```
//!
//! This is the network-carrying counterpart to the in-process `MockLlm` unit
//! tests in `agent_runner.rs`. It binds a local TCP listener that speaks the
//! **Ollama `/api/chat` wire format** and points a real `OllamaClient`
//! (blocking `reqwest`) at it, then drives `RealAgentRunner::run()` through a
//! full ReAct cycle — tool-call → observe → final-result — over *actual HTTP*:
//!
//!   1. `POST {base_url}/api/chat` → `{"message":{"content":
//!      "{\"tool\":\"read_sensor\",\"args\":{\"sensor\":\"temperature\"}}"}}`
//!      → the runner parses the tool call, executes `read_sensor` via
//!      `SimulatorExecutor`, appends the tool result, and loops.
//!   2. `POST {base_url}/api/chat` → `{"message":{"content":
//!      "{\"result\":\"Temperature is 22.0C\"}"}}` → the runner detects the
//!      terminal result, sets `Finished`, and returns it.
//!
//! This exercises the real JSON body builder, the HTTP round-trip, response
//! parsing, and the tool-execution loop — not a canned in-process backend.

#![cfg(feature = "std")]

use magent_core::agent_runner::{OllamaClient, RealAgentRunner};
use magent_core::real_tools::SimulatorExecutor;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A tiny, dependency-free HTTP server that mimics the subset of the Ollama
/// API the ReAct loop uses:
///
/// * `GET  /api/tags`   → `{"models":[]}` (the optional backend probe).
/// * `POST /api/chat`   → Ollama chat-completions; first call returns a
///   `read_sensor` tool call, subsequent calls return a terminal result.
///
/// Returns the base URL (with an ephemeral port), a counter of `/api/chat`
/// calls, and a stop flag to shut the server thread down.
fn spawn_mock_ollama() -> (String, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let base_url = format!("http://127.0.0.1:{port}");

    let chat_calls = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let chat_calls_thread = Arc::clone(&chat_calls);
    let stop_thread = Arc::clone(&stop);

    let _server = std::thread::spawn(move || {
        // Non-blocking accept so we can observe the stop flag between polls.
        let _ = listener.set_nonblocking(true);
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    handle_client(&mut stream, &chat_calls_thread);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    (base_url, chat_calls, stop)
}

/// Serve a single HTTP/1.1 request and close the connection.
fn handle_client(stream: &mut TcpStream, chat_calls: &Arc<AtomicUsize>) {
    // Read the request head (until the blank line separating headers+body).
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if find_header_end(&buf).is_some() {
                    break;
                }
            }
            Err(_) => return,
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let request_line = head.lines().next().unwrap_or("").to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // Drain any remaining body bytes the client already sent.
    if let Some(content_length) = parse_content_length(&head) {
        let header_end = find_header_end(&buf).map(|i| i + 4).unwrap_or(buf.len());
        let mut missing = content_length.saturating_sub(buf.len().saturating_sub(header_end));
        while missing > 0 {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            missing = missing.saturating_sub(n);
        }
    }

    let body = if method == "POST" && path == "/api/chat" {
        // First call → tool call; every subsequent call → terminal result.
        let idx = chat_calls.fetch_add(1, Ordering::SeqCst);
        let inner = if idx == 0 {
            serde_json::json!({"tool": "read_sensor", "args": {"sensor": "temperature"}})
        } else {
            serde_json::json!({"result": "Temperature is 22.0C"})
        };
        // `inner.to_string()` is the JSON the model would emit as `content`;
        // `serde_json::json!` re-escapes it so it round-trips through the
        // outer `{"message":{"content": ...}}` envelope exactly as a real
        // Ollama response would.
        serde_json::json!({"message": {"content": inner.to_string()}}).to_string()
    } else {
        // GET /api/tags (backend probe) — present an empty model list.
        "{\"models\":[]}".to_string()
    };

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(head: &str) -> Option<usize> {
    for line in head.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                return value.trim().parse().ok();
            }
        }
    }
    None
}


#[test]
fn react_loop_runs_over_mock_http() {
    let (base_url, chat_calls, stop) = spawn_mock_ollama();

    // Drive the *real* HTTP backend, not an in-process mock.
    let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
    let client = OllamaClient::new(&base_url, "mock-model");
    runner.set_backend(client);
    runner.force_enable_backend();

    let result = runner
        .run("Monitor the environment temperature")
        .expect("run() should complete over the mock HTTP server");

    stop.store(true, Ordering::SeqCst);

    assert_eq!(result, "Temperature is 22.0C");
    assert_eq!(
        chat_calls.load(Ordering::SeqCst),
        2,
        "ReAct loop must make exactly two /api/chat calls: tool call + result"
    );
    // The tool was actually executed by SimulatorExecutor during the loop.
    assert_eq!(runner.tool_call_count(), 1);
}

#[test]
fn real_backend_is_used_not_simulation() {
    let (base_url, chat_calls, stop) = spawn_mock_ollama();

    let mut runner = RealAgentRunner::new(SimulatorExecutor::new());
    let client = OllamaClient::new(&base_url, "mock-model");
    runner.set_backend(client);
    runner.force_enable_backend();
    assert!(runner.using_ollama(), "backend must be engaged, not simulated");

    let result = runner
        .run("Monitor the environment temperature")
        .expect("run() should succeed");
    stop.store(true, Ordering::SeqCst);

    assert_eq!(result, "Temperature is 22.0C");
    // If the runner had silently fallen back to the simulated backend it
    // would have returned the canned "Environmental monitoring complete"
    // and issued no HTTP calls — the counter guards that regression.
    assert!(chat_calls.load(Ordering::SeqCst) >= 1);
}

