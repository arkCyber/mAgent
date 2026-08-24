//! Web tools for host-side `magent` runs.
//!
//! Disabled on embedded targets (this file is gated on `#[cfg(feature
//! = "std")]`). On the host we expose three LLM-callable tools:
//!
//! | Tool             | Purpose                                                    |
//! |------------------|------------------------------------------------------------|
//! | `web_search`     | DuckDuckGo HTML search; returns title + URL + snippet.    |
//! | `fetch_url`      | HTTP GET a URL, strip HTML, return plain text.            |
//! | `webpage_summary`| Fetch a URL and return a short extractive summary.        |
//!
//! All three are wired into `SimulatorExecutor` (see
//! `magent-core/src/real_tools.rs`) so the CLI's ReAct loop can
//! reach them transparently — the LLM only sees the tool name and
//! arguments, not the implementation.
//!
//! ### Argument format
//!
//! Every tool accepts both `key=value,key=value` and bare-token forms:
//!
//!   * `query=foo` (preferred for `web_search`)
//!   * `url=https://example.com/path` (preferred for `fetch_url` /
//!     `webpage_summary`)
//!
//! ## Why DuckDuckGo?
//!
//! DDG's "lite" HTML endpoint doesn't require a key, doesn't track
//! the user, and returns a stable DOM that's easy to parse without a
//! JS engine. The endpoint `https://html.duckduckgo.com/html/?q=...`
//! is the one used by many headless libraries (e.g. `duckduckgo-
//! search`); we hit it directly with `reqwest` and a tiny HTML
//! regex-based extractor to keep the dependency surface minimal.
//!
//! ## Error handling
//!
//! Network errors are surfaced as `Err(String)` so the agent loop
//! sees them as a tool failure (the loop keeps going rather than
//! aborting). The CLI's compression step will truncate the error
//! to the tool-content budget so the LLM never sees a 50 KB HTML
//! page.

#![cfg(feature = "std")]

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::format;
use std::string::{String, ToString};
use std::sync::OnceLock;
use std::time::Duration;
use std::vec::Vec;

/// Default user agent: a realistic browser UA so DuckDuckGo doesn't
/// immediately hit us with an "anomaly" CAPTCHA. The contact URL
/// is real so admins can rate-limit / contact us if needed.
const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/124.0.0.0 Safari/537.36 ",
    "(magent/",
    env!("CARGO_PKG_VERSION"),
    "; +https://github.com/arkCyber/mAgent)"
);

/// DuckDuckGo HTML search endpoint. No API key required.
const DDG_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// Hard HTTP timeout. Network on the host is fast (sub-second to
/// DDG), so we don't have to wait long — but a stuck socket without
/// a timeout would block the ReAct loop indefinitely.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum response body size (10 MB). Prevents RAM exhaustion from
/// malicious or misconfigured servers that send unbounded bodies.
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of results returned by `web_search`. The LLM only
/// needs a handful of pointers to pick the most relevant URL.
const MAX_SEARCH_RESULTS: usize = 8;

/// One hit returned by [`web_search`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// `<title>` for the result.
    pub title: String,
    /// Destination URL (DDG wraps these in `/l/?uddg=...` —
    /// [`web_search`] unwraps the redirect so the LLM gets the
    /// real URL).
    pub url: String,
    /// Short snippet taken from the result's `.result__snippet`.
    pub snippet: String,
}

/// Outcome of [`fetch_url`]: the cleaned text plus the final URL
/// (after any redirects) and the HTTP status, so the LLM can
/// distinguish between "page loaded, body empty" and "soft 404".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedPage {
    /// URL after redirects. Usually the same as the input.
    pub final_url: String,
    /// HTTP status code (e.g. 200 / 404 / 503).
    pub status: u16,
    /// Content-Type header (best-effort).
    pub content_type: String,
    /// Plain-text rendering of the page, with scripts and styles
    /// stripped.
    pub text: String,
    /// Original byte length of the body before text extraction.
    pub bytes: usize,
}

/// Extracted summary consisting of up to N sentences ranked by
/// keyword overlap with the query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebpageSummary {
    /// The URL that was summarized.
    pub url: String,
    /// Page title (best-effort — may be empty if the page had no
    /// `<title>`).
    pub title: String,
    /// Top sentences chosen as the most relevant to the query.
    pub summary: String,
    /// Number of sentences the chooser had to pick from.
    pub total_sentences: usize,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Lazily-initialised compiled regex for stripping HTML tags. We
/// keep the patterns local so they don't pollute the global
/// `regex::Regex` static cache across tests.
fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("hardcoded tag regex"))
}

/// Compiled regex for finding DDG result blocks. We accept both
/// the legacy `class="result__a"` markup and the modern
/// `data-testid="result-title-a"` form, since DDG has been slowly
/// migrating. Each branch maps cleanly to its own capture groups so
/// that title and snippet are never cross-contaminated:
///
/// Branch 1 (legacy): `(1)` href, `(2)` title, `(3)` snippet
/// Branch 2 (modern): `(4)` href, `(5)` title, `(6)` snippet
fn ddg_block_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?s)(?:class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>.*?class="result__snippet"[^>]*>(.*?)</a>|data-testid="result-title-a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>.*?data-testid="result-snippet"[^>]*>(.*?)</[^>]+>)"#,
        )
        .expect("hardcoded DDG regex")
    })
}

/// Detect DDG's anti-bot CAPTCHA page. We don't try to solve it —
/// we just want a clear error rather than a generic "no results".
fn ddg_captcha_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)anomaly-modal|Unfortunately, bots use DuckDuckGo")
            .expect("hardcoded CAPTCHA regex")
    })
}

/// Compiled regex for finding `<title>...</title>`.
fn title_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("hardcoded title regex"))
}

/// Remove `<script>` blocks. Uses non-greedy `.*?` so it stops at the
/// first `</script>`. Correct for the common case; silently leaves
/// malformed unclosed tags as-is (the raw JS leaks into the text —
/// acceptable for a minimal implementation).
fn script_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script>").expect("hardcoded script regex"))
}

/// Remove `<style>` blocks. Same trade-offs as `script_regex`.
fn style_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style>").expect("hardcoded style regex"))
}

/// HTML entity unescape for the small set we actually see in DDG
/// results. The full list is much larger; we cover the common ones
/// so we don't pull in a full HTML crate.
fn decode_html_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            let mut buf = String::new();
            while let Some(&next) = chars.peek() {
                if next == ';' || buf.len() > 8 {
                    break;
                }
                buf.push(next);
                chars.next();
            }
            if matches!(chars.peek(), Some(';')) {
                chars.next();
            }
            let replaced = match buf.as_str() {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "copy" => Some('©'),
                "reg" => Some('®'),
                "trade" => Some('™'),
                _ => None,
            };
            match replaced {
                Some(ch) => out.push(ch),
                None => {
                    // Numeric entity: `&#1234;` or `&#x1F4A9;`
                    if let Some(stripped) = buf.strip_prefix('#') {
                        let parsed = if let Some(hex) =
                            stripped.strip_prefix('x').or_else(|| stripped.strip_prefix('X'))
                        {
                            u32::from_str_radix(hex, 16).ok()
                        } else {
                            stripped.parse::<u32>().ok()
                        };
                        if let Some(code) = parsed {
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                                continue;
                            }
                        }
                    }
                    // Unknown — pass through literally (HTML entity found but
                    // not recognised; preserving `&...;` is safer than dropping it).
                    out.push('&');
                    out.push_str(&buf);
                    out.push(';');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Decode DDG's redirect URL wrapper. The HTML endpoint returns
/// `//duckduckgo.com/l/?uddg=<urlencoded>` so the user's click
/// action is logged. We just want the destination.
fn unwrap_ddg_redirect(href: &str) -> String {
    if let Some(idx) = href.find("uddg=") {
        let raw = &href[idx + 5..];
        let end = raw.find('&').unwrap_or(raw.len());
        let encoded = &raw[..end];
        if let Ok(decoded) = url_decode(encoded) {
            return decoded;
        }
    }
    href.to_string()
}

/// Tiny URL-decoder (percent + `+`) so we don't pull in the `url`
/// crate. Inputs are short (the `uddg` query parameter), so a
/// hand-rolled loop is fine.
fn url_decode(s: &str) -> std::result::Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_digit(bytes[i + 1])?;
                let lo = hex_digit(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|e| format!("non-UTF8 in URL: {e}"))
}

fn hex_digit(b: u8) -> std::result::Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        other => {
            let repr = if let Some(ch) = char::from_u32(u32::from(other)) {
                format!("'{ch}'")
            } else {
                format!("0x{:02X}", other)
            };
            Err(format!("invalid hex digit: {}", repr))
        }
    }
}

/// Collapse runs of whitespace into a single space and trim.
fn normalise_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out.trim().to_string()
}

/// Strip HTML tags and decode the entities in one pass. We
/// combine these two because the regex-replace produces a `String`
/// and we don't want to round-trip through `&str`/`String` at every
/// call site.
fn strip_and_decode(body: &str) -> String {
    let stripped = tag_regex().replace_all(body, " ").to_string();
    decode_html_entities(&stripped)
}

/// Remove `<script>` and `<style>` blocks before tag stripping.
fn strip_script_style(body: &str) -> String {
    let body = script_regex().replace_all(body, " ").to_string();
    style_regex().replace_all(&body, " ").to_string()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Top-level tool: search the web via DuckDuckGo.
///
/// `args` should contain `query=...`. Also accepts a bare token
/// (no `=`), matching the convention used by `read_sensor` / etc.
pub fn web_search(args: &str) -> std::result::Result<String, String> {
    let query =
        extract_query(args, "query").ok_or_else(|| "web_search: missing 'query' arg".to_string())?;
    if query.trim().is_empty() {
        return Err("web_search: empty query".to_string());
    }

    let url = format!("{}?q={}", DDG_ENDPOINT, url_encode(&query));
    let body = blocking_get(&url)?;

    if ddg_captcha_regex().is_match(&body) {
        return Err(
            "web_search: blocked by DuckDuckGo anti-bot challenge (rate-limited). \
                 Try again later or use fetch_url against a known search engine."
                .to_string(),
        );
    }

    let mut hits: Vec<SearchHit> = Vec::new();
    for cap in ddg_block_regex().captures_iter(&body) {
        // Branch 1 (legacy): groups 1-3; Branch 2 (modern): groups 4-6.
        // We try branch 1 first, then branch 2. Each branch has a
        // self-contained href/title/snippet set, so they never bleed
        // into each other.
        let (href, title_raw, snippet_raw) = if let (Some(h), Some(t), Some(s)) =
            (cap.get(1), cap.get(2), cap.get(3))
        {
            (h.as_str(), t.as_str(), s.as_str())
        } else if let (Some(h), Some(t), Some(s)) =
            (cap.get(4), cap.get(5), cap.get(6))
        {
            (h.as_str(), t.as_str(), s.as_str())
        } else {
            continue;
        };

        let title = normalise_whitespace(&strip_and_decode(title_raw));
        let snippet = normalise_whitespace(&strip_and_decode(snippet_raw));
        let url = unwrap_ddg_redirect(href);

        if title.is_empty() && url.is_empty() {
            continue;
        }
        hits.push(SearchHit {
            title,
            url,
            snippet,
        });
        if hits.len() >= MAX_SEARCH_RESULTS {
            break;
        }
    }

    if hits.is_empty() {
        return Err(
            "web_search: no results (DDG returned no recognisable hits; \
             the page layout may have changed)"
                .to_string(),
        );
    }

    serde_json::to_string_pretty(&hits).map_err(|e| format!("web_search: serialise: {e}"))
}

/// Top-level tool: fetch a URL and return its plain-text body.
pub fn fetch_url(args: &str) -> std::result::Result<String, String> {
    let url =
        extract_query(args, "url").ok_or_else(|| "fetch_url: missing 'url' arg".to_string())?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("fetch_url: refusing non-http(s) URL: {url}"));
    }

    let response = blocking_get_with_meta(&url)?;

    // Reject non-HTML content types — trying to HTML-parse a PNG or
    // JSON response produces garbage.
    if !response.content_type.contains("html") {
        return Err(format!(
            "fetch_url: content-type '{}' is not HTML; refusing to parse",
            response.content_type
        ));
    }

    let body = strip_script_style(&response.body);
    let text = normalise_whitespace(&strip_and_decode(&body));

    let page = FetchedPage {
        final_url: response.final_url,
        status: response.status,
        content_type: response.content_type,
        text,
        bytes: response.bytes,
    };

    serde_json::to_string_pretty(&page).map_err(|e| format!("fetch_url: serialise: {e}"))
}

/// Top-level tool: fetch a URL and return a short extractive
/// summary of the page.
///
/// `query` is the topic the LLM is researching; we use it to rank
/// sentences by keyword overlap. If `query` is missing we fall back
/// to picking the first non-trivial sentences.
pub fn webpage_summary(args: &str) -> std::result::Result<String, String> {
    let url = extract_query(args, "url")
        .ok_or_else(|| "webpage_summary: missing 'url' arg".to_string())?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("webpage_summary: refusing non-http(s) URL: {url}"));
    }
    let query = extract_query(args, "query").unwrap_or_default();

    let response = blocking_get_with_meta(&url)?;

    if !response.content_type.contains("html") {
        return Err(format!(
            "webpage_summary: content-type '{}' is not HTML; refusing to summarise",
            response.content_type
        ));
    }

    let body = strip_script_style(&response.body);

    let title = title_regex()
        .captures(&body)
        .and_then(|c| c.get(1))
        .map(|m| normalise_whitespace(&strip_and_decode(m.as_str())))
        .unwrap_or_default();

    let text = normalise_whitespace(&strip_and_decode(&body));

    let sentences = split_sentences(&text);
    let total = sentences.len();

    // Report an empty summary as an error so the LLM knows the page
    // couldn't be summarised (rather than silently returning `""`).
    if sentences.is_empty() {
        return Err("webpage_summary: could not split page into sentences \
             (no terminating punctuation found); page may be empty or non-textual".to_string());
    }

    let ranked = rank_sentences(&sentences, &query);
    let chosen: Vec<&str> = ranked
        .iter()
        .take(5)
        .map(|&(i, _)| sentences[i].as_str())
        .collect();

    let summary = WebpageSummary {
        url: response.final_url,
        title,
        summary: chosen.join(" "),
        total_sentences: total,
    };

    serde_json::to_string_pretty(&summary).map_err(|e| format!("webpage_summary: serialise: {e}"))
}

/// Map a WMO weather-interpretation code to a short human description.
fn wmo_description(code: i64) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 | 63 | 65 => "Rain",
        66 | 67 => "Freezing rain",
        71 | 73 | 75 => "Snow",
        77 => "Snow grains",
        80 | 81 | 82 => "Rain showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        _ => "Unknown",
    }
}

/// Top-level tool: current conditions + a short 3-day forecast for a
/// city, returned as compact JSON.
///
/// Uses Open-Meteo (no API key): geocodes the city name, then fetches
/// the forecast. The payload is deliberately small and structured so
/// the LLM can summarise it directly — unlike `fetch_url` on a full
/// forecast page (75 KB), which gets head+tail truncated during
/// compression and loses the key numbers in the middle.
///
/// `args` accepts both `{"city":"Beijing"}` and `city=Beijing`.
pub fn get_weather(args: &str) -> std::result::Result<String, String> {
    let city = extract_query(args, "city")
        .ok_or_else(|| "get_weather: missing 'city' arg".to_string())?;
    let city = city.trim();
    if city.is_empty() {
        return Err("get_weather: empty city".to_string());
    }

    let geo_url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=zh&format=json",
        url_encode(city)
    );
    let geo_body = blocking_get(&geo_url)?;
    let geo: serde_json::Value = serde_json::from_str(&geo_body)
        .map_err(|e| format!("get_weather: geocode parse: {e}"))?;
    let result = geo
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| format!("get_weather: no results for city '{city}'"))?;
    let lat = result
        .get("latitude")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "get_weather: geocode missing latitude".to_string())?;
    let lon = result
        .get("longitude")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "get_weather: geocode missing longitude".to_string())?;
    let resolved_name = result
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(city)
        .to_string();
    let country = result
        .get("country")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let fc_url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m\
         &daily=temperature_2m_max,temperature_2m_min,weather_code,precipitation_probability_max\
         &timezone=auto&forecast_days=3",
        lat = lat,
        lon = lon,
    );
    let fc_body = blocking_get(&fc_url)?;
    let fc: serde_json::Value = serde_json::from_str(&fc_body)
        .map_err(|e| format!("get_weather: forecast parse: {e}"))?;

    let current = fc.get("current").cloned().unwrap_or_else(|| serde_json::json!({}));
    let daily = fc.get("daily").cloned().unwrap_or_else(|| serde_json::json!({}));
    let current_code = current.get("weather_code").and_then(|v| v.as_i64()).unwrap_or(-1);

    // Build a compact, labelled daily summary so the LLM doesn't have to
    // interpret raw WMO codes.
    let arr = |k: &str| daily.get(k).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let days = arr("time");
    let maxs = arr("temperature_2m_max");
    let mins = arr("temperature_2m_min");
    let codes = arr("weather_code");
    let preps = arr("precipitation_probability_max");
    let forecast: Vec<serde_json::Value> = (0..days.len().min(3))
        .map(|i| {
            serde_json::json!({
                "date": days.get(i).cloned().unwrap_or_else(|| serde_json::Value::Null),
                "condition": wmo_description(codes.get(i).and_then(|v| v.as_i64()).unwrap_or(-1)),
                "max_c": maxs.get(i).cloned().unwrap_or_else(|| serde_json::Value::Null),
                "min_c": mins.get(i).cloned().unwrap_or_else(|| serde_json::Value::Null),
                "precip_prob_pct": preps.get(i).cloned().unwrap_or_else(|| serde_json::Value::Null),
            })
        })
        .collect();

    let out = serde_json::json!({
        "city": resolved_name,
        "country": country,
        "current": {
            "condition": wmo_description(current_code),
            "temperature_c": current.get("temperature_2m"),
            "feels_like_c": current.get("apparent_temperature"),
            "humidity_pct": current.get("relative_humidity_2m"),
            "wind_kmh": current.get("wind_speed_10m"),
        },
        "forecast": forecast,
    });

    serde_json::to_string_pretty(&out).map_err(|e| format!("get_weather: serialise: {e}"))
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

/// Extract a string value for `key` from a tool-call argument string.
///
/// Two payload shapes are accepted because the LLM and the embedded
/// firmware disagree about the wire format:
///
/// 1. **`key=value,key=value`** — produced by the canned mock
///    planner and the embedded `MiniAgent::pick_tool` path. The
///    comma is the only separator; embedded targets can't pull in
///    a full JSON parser.
/// 2. **`{"key":"value", …}`** — produced by the LLM via
///    `serde_json::to_string(args)` in `agent_runner::execute_tool`.
///    `BareToken` is a single word with no `=` and no `,`.
///
/// We try JSON first, then fall back to `key=value`. Whichever
/// shape matches wins; if neither finds the key, we return `None`.
///
/// Bare-token fallback (a single token with no `=`) is honoured
/// only in the `key=value` branch — JSON bare values are pulled
/// out by the explicit JSON parser.
fn extract_query(args: &str, key: &str) -> Option<String> {
    if let Some(v) = extract_json_value(args, key) {
        return Some(v);
    }
    let mut bare: Option<String> = None;
    for part in args.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim().to_string());
            }
        } else if bare.is_none() {
            bare = Some(trimmed.to_string());
        }
    }
    bare
}

/// Tiny JSON-value extractor for the single-string case we see from
/// the LLM (`{"url":"https://…"}`). We deliberately do NOT pull in
/// `serde_json::Value` here — the `web` module's only purpose is to
/// be an `std`-only leaf and the JSON we receive is short and
/// well-formed by construction (`agent_runner::execute_tool`
/// serialises a `HashMap<String, serde_json::Value>` with `to_string`).
///
/// Returns `None` if the input isn't a JSON object or the key is
/// absent — the caller falls back to the `key=value` parser.
fn extract_json_value(args: &str, key: &str) -> Option<String> {
    let trimmed = args.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    // Hand-parse `"key":"value"` pairs. We accept whitespace around
    // `:` and `,`, and we do NOT unescape JSON escapes (\u, \n, \").
    // The LLM emits URLs as ASCII so escaping is rare in practice;
    // a malformed hit just falls through to the key=value parser.
    let body = &trimmed[1..trimmed.len() - 1];
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace and commas.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Expect `"key"`.
        if bytes[i] != b'"' {
            return None;
        }
        i += 1;
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            // Honour backslash escapes inside the key just enough to
            // skip over `\"` so we don't terminate on the escaped quote.
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
            } else {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return None;
        }
        let k = &body[key_start..i];
        i += 1;
        // Skip whitespace + `:`.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        // Value must be a string for us to care; numeric / bool / object
        // values are ignored so the key=value parser gets a shot.
        if bytes[i] != b'"' {
            return None;
        }
        i += 1;
        let value_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
            } else {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return None;
        }
        let v = &body[value_start..i];
        if k == key {
            return Some(v.to_string());
        }
        i += 1;
    }
    None
}

/// Percent-encode for safe inclusion in a URL query string.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            // RFC 3986 §2.3 unreserved set: `A-Z a-z 0-9 - . _ ~`
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// HTTP layer — singleton client for connection reuse
// ---------------------------------------------------------------------------

/// Singleton HTTP client with connection pooling. Built once and reused
/// across all `web_search` / `fetch_url` / `webpage_summary` calls so
/// that TLS handshakes and TCP connections are amortised.
#[cfg(feature = "reqwest")]
fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest Client must build — check TLS backend availability")
    })
}

/// Minimal GET that returns the body as a `String`.
fn blocking_get(url: &str) -> std::result::Result<String, String> {
    let meta = blocking_get_with_meta(url)?;
    Ok(meta.body)
}

/// A GET response with metadata.
struct ResponseMeta {
    body: String,
    final_url: String,
    status: u16,
    content_type: String,
    bytes: usize,
}

fn blocking_get_with_meta(url: &str) -> std::result::Result<ResponseMeta, String> {
    let resp = http_client()
        .get(url)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8",
        )
        .send()
        .map_err(|e| format!("GET {url}: {e}"))?;

    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Respect the size cap before reading the full body.
    let bytes = resp.content_length().unwrap_or(0) as usize;
    if bytes > MAX_BODY_BYTES {
        return Err(format!(
            "GET {url}: body Content-Length {} bytes exceeds limit {} MB",
            bytes,
            MAX_BODY_BYTES / (1024 * 1024)
        ));
    }

    let body = resp
        .text()
        .map_err(|e| format!("read body from {url}: {e}"))?;

    let bytes = body.len();
    if bytes > MAX_BODY_BYTES {
        return Err(format!(
            "GET {url}: body {} bytes exceeds limit {} MB",
            bytes,
            MAX_BODY_BYTES / (1024 * 1024)
        ));
    }

    if !(200..300).contains(&status) {
        return Err(format!(
            "GET {url} returned HTTP {status} ({} bytes)",
            bytes
        ));
    }

    Ok(ResponseMeta {
        body,
        final_url,
        status,
        content_type,
        bytes,
    })
}

// ---------------------------------------------------------------------------
// Sentence ranking
// ---------------------------------------------------------------------------

/// Returns `true` if `chars[i]` is a period that ends an abbreviation
/// (e.g. "Dr.", "e.g.", "U.S.A.") rather than a sentence boundary.
///
/// Only periods (`.`) can be abbreviation ends. Exclamation marks (`!`)
/// and question marks (`?`) are always sentence terminators.
///
/// We count alphanumeric characters in the word preceding the period:
///   - ≤ 3 letters → short word → may be an abbreviation → suppress split.
///   - > 3 letters → real word → never an abbreviation → allow split.
fn is_abbreviation_end(chars: &[char], i: usize) -> bool {
    // Only periods can be abbreviations — '!' and '?' are always terminators.
    if chars[i] != '.' {
        return false;
    }
    let mut word_len = 0;
    let mut j = i;
    while j > 0 && chars[j - 1].is_alphanumeric() {
        word_len += 1;
        j -= 1;
    }
    word_len <= 3
}

/// Split text into rough sentences. We look for `[.!?]` followed by
/// whitespace + a capital letter (or end-of-string) to decide a
/// boundary. We also check [`is_abbreviation_end`] to avoid splitting
/// mid-sentence abbreviations.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        let c = chars[i];
        current.push(c);
        if matches!(c, '.' | '!' | '?') {
            // Look ahead past whitespace to the next non-space character.
            let mut j = i + 1;
            while j < len && chars[j].is_whitespace() {
                j += 1;
            }

            let at_end = j >= len;
            let next_is_upper = j < len && chars[j].is_ascii_uppercase();

            // Determine if this punctuation ends a sentence.
            let ends_sentence = if at_end {
                true
            } else if next_is_upper {
                !is_abbreviation_end(&chars, i)
            } else {
                // Next is lowercase or non-alpha — not a sentence boundary.
                false
            };

            if ends_sentence {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
                current.clear();
                i = j;
                continue;
            }
        }
        i += 1;
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    out
}

/// Tokenise `s` into lowercase, alphanum-only tokens for keyword
/// ranking.
fn tokenise(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|tok| !tok.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Rank sentences by TF (term frequency) overlap with the query.
/// Returns indices in descending score order.
fn rank_sentences(sentences: &[String], query: &str) -> Vec<(usize, usize)> {
    let query_tokens: Vec<String> = tokenise(query);
    if query_tokens.is_empty() {
        return sentences.iter().enumerate().map(|(i, _)| (i, 0)).collect();
    }

    let mut query_tf: HashMap<String, usize> = HashMap::new();
    for tok in &query_tokens {
        *query_tf.entry(tok.clone()).or_insert(0) += 1;
    }

    let mut scored: Vec<(usize, usize)> = Vec::new();
    for (i, s) in sentences.iter().enumerate() {
        let toks = tokenise(s);
        if toks.is_empty() {
            continue;
        }
        let mut score = 0usize;
        for tok in &toks {
            if let Some(w) = query_tf.get(tok) {
                score += *w;
            }
        }
        // Tie-break: longer sentences first (up to 50 tokens) so we
        // get substantive content rather than fragments.
        if score > 0 {
            score = score * 1000 + toks.len().min(50);
        }
        scored.push((i, score));
    }
    scored.sort_by_key(|b| core::cmp::Reverse(b.1));
    scored
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_query_finds_key_value() {
        assert_eq!(
            extract_query("query=hello world", "query").unwrap(),
            "hello world"
        );
        assert_eq!(
            extract_query("url=https://example.com,query=hi", "query").unwrap(),
            "hi"
        );
    }

    #[test]
    fn extract_query_falls_back_to_bare_token() {
        assert_eq!(extract_query("hello", "query").unwrap(), "hello");
        assert_eq!(
            extract_query("url=https://x,query=hi", "url").unwrap(),
            "https://x"
        );
    }

    #[test]
    fn extract_query_returns_none_when_missing() {
        assert!(extract_query("", "query").is_none());
        assert!(extract_query("foo=bar", "query").is_none());
    }

    #[test]
    fn extract_query_reads_json_payload_from_llm() {
        // The LLM emits tool args as `serde_json::to_string(...)`, so
        // the web tools receive JSON like `{"url":"https://x"}`.
        // The previous parser only understood `key=value` and the
        // dispatch always returned "missing url" — an integration
        // failure that took the whole web stack offline for LLM users.
        assert_eq!(
            extract_query(r#"{"url":"https://example.com"}"#, "url").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            extract_query(r#"{"query":"rust async"}"#, "query").unwrap(),
            "rust async"
        );
        // Multi-key and whitespace-tolerant inputs.
        assert_eq!(
            extract_query(r#"{ "url": "https://a", "query": "rust" }"#, "query").unwrap(),
            "rust"
        );
    }

    #[test]
    fn extract_query_returns_none_when_key_completely_missing() {
        // `key=value` form with no matching key and no bare token:
        // None. The LLM always sends JSON, so key=value is the
        // legacy shape used by the embedded planner.
        assert!(extract_query("foo=bar", "url").is_none());
        assert!(extract_query("", "url").is_none());
    }

    #[test]
    fn url_encode_handles_spaces() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn url_decode_round_trips() {
        assert_eq!(url_decode("hello%20world").unwrap(), "hello world");
        assert_eq!(url_decode("a%2Bb").unwrap(), "a+b");
    }

    #[test]
    fn decode_html_entities_handles_named_and_numeric() {
        assert_eq!(decode_html_entities("A &amp; B"), "A & B");
        assert_eq!(decode_html_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_html_entities("&#65;&#x42;"), "AB");
    }

    #[test]
    fn decode_html_entities_unknown_entity_preserved() {
        // Unknown entities should be passed through literally, not dropped.
        assert_eq!(decode_html_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn normalise_whitespace_collapses_runs() {
        assert_eq!(normalise_whitespace("  a  b\t\nc  "), "a b c");
        assert_eq!(normalise_whitespace("trailing  "), "trailing");
    }

    #[test]
    fn strip_and_decode_removes_simple_tags() {
        assert_eq!(
            strip_and_decode("<p>hello <b>world</b></p>"),
            " hello  world  "
        );
    }

    #[test]
    fn strip_and_decode_handles_entities() {
        assert_eq!(strip_and_decode("A &amp; B"), "A & B");
    }

    #[test]
    fn strip_script_style_removes_blocks() {
        let body =
            "Hello<script>alert(1)</script>world<style>body{}</style>done";
        let out = strip_script_style(body);
        assert!(!out.contains("alert(1)"));
        assert!(!out.contains("body{}"));
        assert!(out.contains("Hello"));
        assert!(out.contains("done"));
    }

    #[test]
    fn split_sentences_breaks_on_punctuation() {
        let s = "First sentence. Second one! Third question? And a fourth.";
        let parts = split_sentences(s);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "First sentence.");
    }

    #[test]
    fn split_sentences_handles_abbreviations() {
        // "Dr.", "e.g.", and "MIT." should not split mid-sentence.
        // Our abbreviation heuristic: periods after ≤3-letter words are
        // suppressed; '!' and '?' always split regardless of abbreviation.
        let s = "Dr. Smith works at e.g. MIT.";
        let parts = split_sentences(s);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], "Dr. Smith works at e.g. MIT.");
    }

    #[test]
    fn rank_sentences_prefers_overlap() {
        let sentences = vec![
            "Rust is a systems programming language.".to_string(),
            "The cat sat on the mat.".to_string(),
            "Rust borrow checker prevents data races.".to_string(),
            "Birds fly in the sky.".to_string(),
        ];
        let ranked = rank_sentences(&sentences, "rust borrow checker");
        assert_eq!(ranked[0].0, 2);
    }

    #[test]
    fn rank_sentences_handles_empty_query() {
        let sentences = ["a".to_string(), "b".to_string(), "c".to_string()];
        let ranked = rank_sentences(&sentences, "");
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].0, 0);
    }

    #[test]
    fn rank_sentences_tiebreak_favours_longer() {
        let sentences = vec![
            "Rust.".to_string(),           // score=1, len=1
            "Rust is a language.".to_string(), // score=1, len=4
            "Python.".to_string(),          // score=0
        ];
        let ranked = rank_sentences(&sentences, "rust");
        assert_eq!(ranked[0].0, 1); // longer sentence wins tiebreak
        assert_eq!(ranked[1].0, 0); // shorter
        assert_eq!(ranked[2].0, 2); // no overlap last
    }

    #[test]
    fn unwrap_ddg_redirect_extracts_uddg() {
        let raw = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F&ss=foo";
        assert_eq!(unwrap_ddg_redirect(raw), "https://example.com/");
    }

    #[test]
    fn unwrap_ddg_redirect_passthrough_when_no_uddg() {
        assert_eq!(
            unwrap_ddg_redirect("https://example.com/"),
            "https://example.com/"
        );
    }

    #[test]
    fn tokenise_lowercases_and_drops_punct() {
        assert_eq!(
            tokenise("The Quick, BROWN fox!"),
            vec!["the", "quick", "brown", "fox"]
        );
    }

    #[test]
    fn hex_digit_rejects_invalid() {
        assert!(hex_digit(b'X').is_err());
        assert!(hex_digit(b'g').is_err());
    }

    #[test]
    fn url_encode_special_chars() {
        // Characters outside ASCII get percent-encoded.
        assert_eq!(url_encode("hello世界"), "hello%E4%B8%96%E7%95%8C");
        assert_eq!(url_encode("foo?bar=1"), "foo%3Fbar%3D1");
    }
}
