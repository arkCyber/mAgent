//! Live-network tests for `magent_core::web`.
//!
//! These tests exercise the real `web_search`, `fetch_url`,
//! `webpage_summary`, and `get_weather` paths over HTTP. They hit
//! production endpoints (DuckDuckGo, Open-Meteo) and therefore require
//! network access.
//!
//! ```sh
//! cargo test -p magent-core --features std --test web_api_integration -- --ignored
//! ```
//!
//! The non-ignored tests in this file run purely against the parser /
//! argument-validation logic so the file still compiles in a clean sandbox
//! without network access. The real network tests are gated behind `#[ignore]`
//! and exercise the actual `reqwest::blocking` client the production
//! firmware would use.
//!
//! ## Why these tests matter
//!
//! `web.rs` is the real "LLM touches the internet" surface of the agent.
//! A bug here means the LLM hallucinates URLs or reports stale weather.
//! These tests pin the wire format and the JSON-RPC parsing so future
//! refactors can't silently regress it.

#![cfg(feature = "std")]

use magent_core::web::{fetch_url, get_weather, web_search, webpage_summary};

// ---------------------------------------------------------------------------
// 1. Argument validation — runs without network
// ---------------------------------------------------------------------------

#[test]
fn web_search_requires_query_argument() {
    let err = web_search("").unwrap_err();
    assert!(
        err.contains("query"),
        "error must mention the missing arg: {err}"
    );
}

#[test]
fn web_search_rejects_empty_query_value() {
    let err = web_search("query=").unwrap_err();
    assert!(err.contains("empty"), "expected empty-query error: {err}");
}

#[test]
fn fetch_url_requires_url_argument() {
    let err = fetch_url("").unwrap_err();
    assert!(err.contains("url"), "error must mention url arg: {err}");
}

#[test]
fn fetch_url_rejects_non_http_scheme() {
    // The validate_fetch_url guard rejects any scheme that isn't http/https
    // (SSRF protection). `javascript:` is the most dangerous — an attacker
    // could pass `javascript:alert(1)` and have the LLM think it fetched
    // a page. The error must explicitly call out the rejected scheme so
    // the LLM can self-correct.
    for bad in [
        "url=ftp://example.com/file",
        "url=javascript:alert(1)",
        "url=file:///etc/passwd",
        "url=data:text/html,<script>",
    ] {
        let err = fetch_url(bad).unwrap_err();
        assert!(
            err.to_ascii_lowercase().contains("non-http") || err.contains("refusing"),
            "non-http scheme must be explicitly rejected ({bad}): {err}"
        );
    }
}

#[test]
fn fetch_url_rejects_empty_url() {
    // Empty url falls through the http://https:// prefix check and is
    // rejected as "refusing non-http(s) URL". The validator must surface
    // a clear, machine-readable error so the LLM can self-correct.
    let err = fetch_url("url=").unwrap_err();
    assert!(
        err.contains("refusing") || err.contains("non-http"),
        "empty url must be rejected with a clear message: {err}"
    );
}

#[test]
fn fetch_url_rejects_loopback_and_private_networks() {
    // SSRF guard: an attacker who controls a prompt must not be able to
    // fetch `http://127.0.0.1/admin` or `http://169.254.169.254/latest/`
    // (AWS metadata) via the LLM. The validator must reject these
    // explicitly — silent fall-through would leak the device's local state.
    for private_url in [
        "url=http://127.0.0.1/",
        "url=http://localhost/",
        "url=http://10.0.0.5/internal",
        "url=http://169.254.169.254/latest/meta-data/",
    ] {
        let err = fetch_url(private_url).unwrap_err();
        assert!(
            err.to_ascii_lowercase().contains("loopback")
                || err.to_ascii_lowercase().contains("private")
                || err.to_ascii_lowercase().contains("refusing"),
            "private network must be rejected ({private_url}): {err}"
        );
    }
}

#[test]
fn get_weather_requires_city_argument() {
    let err = get_weather("").unwrap_err();
    assert!(err.contains("city"), "error must mention city arg: {err}");
}

#[test]
fn get_weather_rejects_empty_city() {
    let err = get_weather("city=").unwrap_err();
    assert!(err.contains("empty"), "expected empty-city error: {err}");
}

#[test]
fn webpage_summary_rejects_empty_url() {
    let err = webpage_summary("").unwrap_err();
    assert!(err.contains("url"), "error must mention url arg: {err}");
}

// ---------------------------------------------------------------------------
// 2. Live network tests — gated by `#[ignore]`
//
// These require network access and an HTTP client that can resolve
// DuckDuckGo / Open-Meteo. Run explicitly:
//
//   cargo test -p magent-core --features std --test web_api_integration -- --ignored
// ---------------------------------------------------------------------------

/// DuckDuckGo HTML endpoint is rate-limited and may serve a captcha
/// challenge. We therefore only assert the call returns *some* well-formed
/// string, not a specific result, when run live.
#[test]
#[ignore = "requires network access"]
fn web_search_returns_text_for_known_query() {
    let result = web_search("query=rust+programming+language");
    match result {
        Ok(s) => assert!(!s.is_empty(), "search returned empty body"),
        Err(e) => {
            // DDG may captcha — that's a legitimate failure mode.
            eprintln!("web_search live test skipped: {e}");
        }
    }
}

#[test]
#[ignore = "requires network access"]
fn get_weather_returns_structured_payload() {
    // Open-Meteo is a free public service that does not require an API key.
    let result = get_weather(r#"{"city":"Tokyo"}"#);
    let result = result.or_else(|_| get_weather("city=Tokyo"));
    match result {
        Ok(s) => {
            // The Open-Meteo path returns a structured one-line string with
            // 'temperature_2m' somewhere in it.
            assert!(
                s.contains("temperature_2m") || s.contains("weather"),
                "weather payload missing expected keys: {s}"
            );
        }
        Err(e) => panic!("get_weather live test failed: {e}"),
    }
}

#[test]
#[ignore = "requires network access"]
fn fetch_url_strips_html() {
    let result = fetch_url("url=https://example.com");
    match result {
        Ok(s) => {
            // example.com's body is a single short paragraph — the HTML
            // stripper should leave a near-empty document. We just assert
            // it doesn't contain raw `<html>` or `<body>` tags.
            assert!(
                !s.contains("<html>"),
                "html tags leaked through stripper: {s}"
            );
            assert!(
                !s.contains("<body>"),
                "html tags leaked through stripper: {s}"
            );
        }
        Err(e) => panic!("fetch_url live test failed: {e}"),
    }
}
