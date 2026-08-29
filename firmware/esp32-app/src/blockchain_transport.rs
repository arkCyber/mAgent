//! Real esp-idf wire backend for the blockchain JSON-RPC `Transport`.
//!
//! This module implements [`magent_core::web3::blockchain::esp32_http::Transport`]
//! on top of ESP-IDF's native HTTP client (`esp_idf_svc::http::client`), so the
//! agent's `get_balance` / `send_transaction` blockchain tools actually talk to
//! an RPC endpoint from the device instead of returning a placeholder error.
//!
//! Same production hardening as `AT+HTTPGET` and the DeepSeek backend (`llm.rs`):
//!   - a **hard TCP-connect timeout** (esp-idf-svc's `timeout` does not reliably
//!     bound DNS / TCP-connect / TLS on the C61, so a dead host must not hang
//!     the agent thread and trip a watchdog);
//!   - the **system certificate bundle** for real TLS verification;
//!   - a **bounded response buffer** so a hostile endpoint cannot exhaust the
//!     device heap.
//!
//! The struct is stateless (a fresh connection per request), which keeps it
//! `Send + Sync` so it can be shared behind an `Arc` as the process-wide default.

use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::Duration;

use embedded_svc::http::client::Client as HttpClient;
use embedded_svc::http::Method;
use embedded_svc::io::Write as _;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};

use magent_core::web3::blockchain::esp32_http::{HttpError, SharedTransport, Transport};

/// Hard cap on any single RPC response body (64 KiB): large enough for normal
/// JSON-RPC replies, small enough that a malicious endpoint cannot OOM the
/// agent's 320 KB SRAM / 2 MB PSRAM budget.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// Bounded connect / read timeout, in seconds.
const REQUEST_TIMEOUT_S: u64 = 8;

/// A stateless esp-idf HTTP `Transport` for blockchain JSON-RPC.
#[derive(Debug, Clone, Default)]
pub struct EspIdfTransport;

impl EspIdfTransport {
    /// Install this as the process-wide default transport.
    ///
    /// Call once during firmware bring-up (before any blockchain tool runs).
    /// Subsequent calls are a no-op (the first install wins), so a soft-reboot
    /// re-entry is safe.
    pub fn install_default() {
        let _ = magent_core::web3::blockchain::esp32_http::set_default_transport(
            Arc::new(EspIdfTransport) as SharedTransport,
        );
    }
}

impl Transport for EspIdfTransport {
    fn post(
        &self,
        url: &str,
        path: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Result<String, HttpError> {
        let full = format!("{url}{path}");
        // Bounded DNS / TCP-connect preflight so a dead host cannot hang here.
        preflight(&full)?;

        let cfg = HttpConfig {
            timeout: Some(Duration::from_secs(REQUEST_TIMEOUT_S)),
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            ..Default::default()
        };
        let conn = EspHttpConnection::new(&cfg)
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf conn: {e}")))?;
        let mut client = HttpClient::wrap(conn);
        let mut request = client
            .request(Method::Post, &full, headers)
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf request: {e}")))?;
        request
            .write_all(body.as_bytes())
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf write: {e}")))?;
        request
            .flush()
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf flush: {e}")))?;
        let mut response = request
            .submit()
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf submit: {e}")))?;

        let status = response.status();
        if status != 200 {
            return Err(HttpError::InvalidResponse(format!("HTTP status {status}")));
        }
        read_bounded(&mut response)
    }

    fn get(&self, url: &str, path: &str) -> Result<String, HttpError> {
        let full = format!("{url}{path}");
        preflight(&full)?;

        let cfg = HttpConfig {
            timeout: Some(Duration::from_secs(REQUEST_TIMEOUT_S)),
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            ..Default::default()
        };
        let conn = EspHttpConnection::new(&cfg)
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf conn: {e}")))?;
        let mut client = HttpClient::wrap(conn);
        let request = client
            .request(Method::Get, &full, &[])
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf request: {e}")))?;
        let mut response = request
            .submit()
            .map_err(|e| HttpError::ConnectionFailed(format!("esp-idf submit: {e}")))?;

        let status = response.status();
        if status != 200 {
            return Err(HttpError::InvalidResponse(format!("HTTP status {status}")));
        }
        read_bounded(&mut response)
    }
}

/// Resolve the `(host, port)` authority from a URL for the TCP preflight.
fn host_port(url: &str) -> Option<(String, u16)> {
    let scheme_rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let is_tls = url.starts_with("https://");
    let host_part = scheme_rest.split('/').next()?;
    let (host, port) = match host_part.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (host_part, if is_tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

/// Bounded DNS resolve + TCP connect, so a dead / unresolvable host fails fast.
fn preflight(url: &str) -> Result<(), HttpError> {
    let (host, port) =
        host_port(url).ok_or_else(|| HttpError::ConnectionFailed(format!("malformed URL: {url}")))?;
    let authority = format!("{host}:{port}");
    let mut addrs = authority
        .to_socket_addrs()
        .map_err(|e| HttpError::DnsFailed(format!("{authority}: {e}")))?;
    let addr = addrs
        .next()
        .ok_or_else(|| HttpError::DnsFailed(format!("no address for {authority}")))?;
    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(REQUEST_TIMEOUT_S))
        .map_err(|e| HttpError::ConnectionFailed(format!("connect {authority}: {e}")))?;
    Ok(())
}

/// Read the response body into a `String`, capping total bytes.
///
/// Uses `Vec<u8>` so large-but-legitimate JSON-RPC replies are handled, while
/// `MAX_RESPONSE_BYTES` guards the heap against a hostile endpoint.
fn read_bounded<R: embedded_svc::io::Read>(response: &mut R) -> Result<String, HttpError> {
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 512];
    loop {
        match response.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() + n > MAX_RESPONSE_BYTES {
                    return Err(HttpError::BufferOverflow);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) => {
                return Err(HttpError::InvalidResponse(format!("read: {e}")));
            }
        }
    }
    String::from_utf8(buf).map_err(|e| HttpError::InvalidResponse(format!("utf8: {e}")))
}

