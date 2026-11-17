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
use std::io::Read;
use std::net::ToSocketAddrs;
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

/// Whether `fetch_url` / `webpage_summary` may reach RFC1918 private
/// (10/8, 172.16/12, 192.168/16) addresses. Defaults to `false` so an
/// agent that fetches URLs chosen by an LLM cannot be coerced into
/// probing a local intranet. Set to `true` only when browsing trusted
/// private documentation servers.
const ALLOW_PRIVATE_NETWORKS: bool = false;

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

/// Compiled regex matching a single DDG result *anchor* (the title link).
/// Legacy markup: `class="result__a"`; modern: `data-testid="result-title-a"`.
///
/// Captures: `(1)` href (legacy), `(2)` title (legacy), `(3)` href (modern),
/// `(4)` title (modern).
///
/// Deliberately *not* one regex spanning title → snippet. That form used
/// `(?s).*?` between the two, which crosses result boundaries: a result
/// without a snippet matched the *next* result's snippet, so A was reported
/// with B's text and B disappeared entirely.
const DDG_ANCHOR_PATTERN: &str = r#"(?s)(?:class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>|data-testid="result-title-a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>)"#;

fn ddg_anchor_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(DDG_ANCHOR_PATTERN).expect("hardcoded DDG anchor regex"))
}

/// Compiled regex matching one snippet element's body.
/// Legacy: `class="result__snippet"`; modern: `data-testid="result-snippet"`.
fn ddg_snippet_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?s)(?:class="result__snippet"[^>]*>(.*?)</a>|data-testid="result-snippet"[^>]*>(.*?)</[^>]+>)"#,
        )
        .expect("hardcoded DDG snippet regex")
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
    RE.get_or_init(|| {
        Regex::new(r"(?is)<script\b[^>]*>.*?</script>").expect("hardcoded script regex")
    })
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
            // Collect the candidate entity name, but only treat it as an
            // entity when a `;` actually terminates it.
            let mut buf = String::new();
            while let Some(&next) = chars.peek() {
                if next == ';' {
                    break;
                }
                // A raw `&` starts a new candidate; anything else that can
                // never appear in an entity name (whitespace, `=`, `/`, …)
                // means this `&` was ordinary text.
                if next == '&' || !next.is_ascii_alphanumeric() && next != '#' {
                    break;
                }
                if buf.len() > 8 {
                    break;
                }
                buf.push(next);
                chars.next();
            }
            // Without the terminator this was a bare ampersand in prose
            // ("AT&T", "Tom & Jerry"). Emitting `&T;` inserted a semicolon
            // into the middle of a word — the page text the model sees must
            // be what the page actually said.
            if !matches!(chars.peek(), Some(';')) {
                out.push('&');
                out.push_str(&buf);
                continue;
            }
            chars.next(); // consume the `;`
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
                        let parsed = if let Some(hex) = stripped
                            .strip_prefix('x')
                            .or_else(|| stripped.strip_prefix('X'))
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
                    // Unknown but *terminated* entity (`&foo;`): preserve it
                    // literally rather than dropping bytes.
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

/// Returns true when the response Content-Type is an HTML family we
/// know how to render to plain text. Case-insensitive so a server that
/// sends `Text/Html` isn't wrongly rejected.
fn is_html_content_type(ct: &str) -> bool {
    ct.to_ascii_lowercase().contains("html")
}

/// Reject URLs that point at loopback, link-local (APIPA / cloud
/// metadata 169.254.169.254), or - unless [ALLOW_PRIVATE_NETWORKS]
/// is set - RFC1918 private networks. This is the SSRF guard for the
/// web tools.
///
/// The host/address classification itself lives in
/// [`crate::at_validate::public_fetch_target_refusal`] so the host tools and the
/// firmware's on-device tools (`fetch_web` et al.) share one implementation;
/// this wrapper only does the URL-shape checks the firmware's URL parser has
/// already done by the time it calls the classifier.
fn validate_fetch_url(url: &str) -> Result<(), String> {
    let host = match crate::at_validate::parse_http_authority(url) {
        Some((h, _port)) => h,
        None if url.starts_with("http://") || url.starts_with("https://") => {
            return Err(format!("URL has no host: {url}"))
        }
        None => return Err(format!("refusing non-http(s) URL: {url}")),
    };
    match crate::at_validate::public_fetch_target_refusal(host, ALLOW_PRIVATE_NETWORKS) {
        Some(reason) => Err(format!("{reason}: {host}")),
        None => Ok(()),
    }
}

/// Like [`validate_fetch_url`], but also resolves the host and vets every
/// returned address.
///
/// The lexical guard cannot see a *name* that resolves inward:
/// `http://127.0.0.1.nip.io/` (or a numeric spelling such as
/// `http://2130706433/`) is not an IP literal and passed it. Every hop is run
/// through this before the request is sent. DNS rebinding between this
/// resolution and reqwest's own is still theoretically possible (reqwest
/// exposes no pinned-address hook); the immediate bypass is closed.
fn validate_resolved_fetch_target(url: &str) -> Result<(), String> {
    validate_fetch_url(url)?;
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL {url}: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("URL has no host: {url}"))?;

    // Classify the *normalised* host, not only the raw input.
    //
    // `Url::parse` rewrites numeric and alternate-radix host spellings into
    // canonical IP literals: `http://2130706433/` and `http://0x7f000001/`
    // both become `127.0.0.1`, and `http://2852039166/` becomes
    // `169.254.169.254` (cloud metadata). The lexical pass above sees none of
    // those as an IP, so it let them through; this function then re-checked
    // the *parsed* host, found it was an IP literal, concluded "already
    // vetted" and returned Ok — sending the request inward. That early
    // return is the bypass; the check has to run on this value.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let is_literal = bare.parse::<std::net::IpAddr>().is_ok();
    if let Some(reason) =
        crate::at_validate::public_fetch_target_refusal(bare, ALLOW_PRIVATE_NETWORKS)
    {
        return Err(format!("{reason}: {host}"));
    }
    if is_literal {
        // A literal is its own resolution; it has just been classified.
        return Ok(());
    }

    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| format!("URL has no port: {url}"))?;
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution for {host} failed: {e}"))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        let ip = addr.ip().to_string();
        if let Some(reason) =
            crate::at_validate::public_fetch_target_refusal(&ip, ALLOW_PRIVATE_NETWORKS)
        {
            return Err(format!("{reason}: {host} resolved to {ip}"));
        }
    }
    if !any {
        return Err(format!("no address for {host}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Top-level tool: search the web via DuckDuckGo.
///
/// `args` should contain `query=...`. Also accepts a bare token
/// (no `=`), matching the convention used by `read_sensor` / etc.
pub fn web_search(args: &str) -> std::result::Result<String, String> {
    let query = extract_query(args, "query")
        .ok_or_else(|| "web_search: missing 'query' arg".to_string())?;
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

    let hits = parse_search_results(&body);
    if hits.is_empty() {
        return Err(
            "web_search: no results (DDG returned no recognisable hits; \
             the page layout may have changed)"
                .to_string(),
        );
    }

    serde_json::to_string_pretty(&hits).map_err(|e| format!("web_search: serialise: {e}"))
}

/// Parse a DDG HTML response body into ranked [`SearchHit`]s.
///
/// Split out from [`web_search`] so the markup-parsing logic — both
/// the legacy and modern layouts, the empty-hit skip, and the result
/// cap — is testable without a live network round-trip.
fn parse_search_results(body: &str) -> Vec<SearchHit> {
    // Split the document into per-result segments first: each result owns
    // the markup from its title anchor up to (but not including) the next
    // anchor. A snippet is only ever taken from *its own* segment, which is
    // what keeps result A from being reported with result B's summary.
    let anchors: Vec<_> = ddg_anchor_regex().captures_iter(body).collect();
    let mut hits: Vec<SearchHit> = Vec::new();
    for (i, cap) in anchors.iter().enumerate() {
        let (href, title_raw) = if let (Some(h), Some(t)) = (cap.get(1), cap.get(2)) {
            (h.as_str(), t.as_str())
        } else if let (Some(h), Some(t)) = (cap.get(3), cap.get(4)) {
            (h.as_str(), t.as_str())
        } else {
            continue;
        };

        // Segment = from this anchor's end to the next anchor's start.
        // `captures_iter` yields non-overlapping, ordered matches and regex
        // boundaries are character boundaries, so this range always exists.
        // If that ever stopped being true, falling back to an empty segment
        // would silently drop the snippet — which is the bug this function
        // exists to fix — so the range is taken directly instead.
        let Some(whole) = cap.get(0) else {
            continue;
        };
        let seg_start = whole.end();
        let seg_end = anchors
            .get(i + 1)
            .and_then(|n| n.get(0))
            .map_or(body.len(), |m| m.start());
        let segment = &body[seg_start..seg_end];
        let snippet_raw = ddg_snippet_regex()
            .captures(segment)
            .and_then(|c| c.get(1).or_else(|| c.get(2)))
            .map_or("", |m| m.as_str());

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
    hits
}

/// Top-level tool: fetch a URL and return its plain-text body.
pub fn fetch_url(args: &str) -> std::result::Result<String, String> {
    let url =
        extract_query(args, "url").ok_or_else(|| "fetch_url: missing 'url' arg".to_string())?;
    validate_fetch_url(&url)?;

    let response = blocking_get_with_meta(&url)?;

    // Reject non-HTML content types — trying to HTML-parse a PNG or
    // JSON response produces garbage.
    if !is_html_content_type(&response.content_type) {
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
    validate_fetch_url(&url)?;
    let query = extract_query(args, "query").unwrap_or_default();

    let response = blocking_get_with_meta(&url)?;

    if !is_html_content_type(&response.content_type) {
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
             (no terminating punctuation found); page may be empty or non-textual"
            .to_string());
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
        80..=82 => "Rain showers",
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
    let city =
        extract_query(args, "city").ok_or_else(|| "get_weather: missing 'city' arg".to_string())?;
    let city = city.trim();
    if city.is_empty() {
        return Err("get_weather: empty city".to_string());
    }

    let geo_url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=zh&format=json",
        url_encode(city)
    );
    let geo_body = blocking_get(&geo_url)?;
    let geo: serde_json::Value =
        serde_json::from_str(&geo_body).map_err(|e| format!("get_weather: geocode parse: {e}"))?;
    let result = geo
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| format!("get_weather: no results for city '{city}'"))?;
    let lat = result
        .get("latitude")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "get_weather: geocode missing latitude".to_string())?;
    let lon = result
        .get("longitude")
        .and_then(serde_json::Value::as_f64)
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
    let fc: serde_json::Value =
        serde_json::from_str(&fc_body).map_err(|e| format!("get_weather: forecast parse: {e}"))?;

    let current = fc
        .get("current")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let daily = fc
        .get("daily")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let current_code = current
        .get("weather_code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(-1);

    // Build a compact, labelled daily summary so the LLM doesn't have to
    // interpret raw WMO codes.
    let arr = |k: &str| {
        daily
            .get(k)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    };
    let days = arr("time");
    let maxs = arr("temperature_2m_max");
    let mins = arr("temperature_2m_min");
    let codes = arr("weather_code");
    let preps = arr("precipitation_probability_max");
    let forecast: Vec<serde_json::Value> = (0..days.len().min(3))
        .map(|i| {
            serde_json::json!({
                "date": days.get(i).cloned().unwrap_or(serde_json::Value::Null),
                "condition": wmo_description(codes.get(i).and_then(serde_json::Value::as_i64).unwrap_or(-1)),
                "max_c": maxs.get(i).cloned().unwrap_or(serde_json::Value::Null),
                "min_c": mins.get(i).cloned().unwrap_or(serde_json::Value::Null),
                "precip_prob_pct": preps.get(i).cloned().unwrap_or(serde_json::Value::Null),
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
    // A JSON object that did not yield the key is malformed for this tool.
    // Falling through to the bare-token rule would return a fragment of the
    // JSON (e.g. `{"timeout":5`) as the query, so the tool would search for
    // garbage and report "no results" instead of "bad arguments".
    if args.trim_start().starts_with('{') {
        return None;
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

/// Extract a string value for `key` from a JSON-object argument payload.
///
/// Returns `None` when the input is not a JSON object or the key is absent
/// (the caller then falls back to the `key=value` parser).
///
/// This used to be a hand-rolled scanner that `return None`'d the moment it
/// met a value that was not a string literal — so `{"timeout":5,"url":…}`
/// gave up *before* reaching `url`, and the caller's `key=value` fallback
/// then treated the first bare token (`{"timeout":5`) as the value. The
/// tools really do receive objects with non-string members (`max_results`,
/// `latitude`, `timeout`), so the scanner is now `serde_json`, which this
/// crate already depends on.
fn extract_json_value(args: &str, key: &str) -> Option<String> {
    let trimmed = args.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        // Models routinely emit a trailing comma before a closing brace.
        // Stripping just those two-character sequences is a bounded repair
        // (it cannot change a valid document) and keeps an otherwise
        // well-formed call working.
        Err(_) => serde_json::from_str(&strip_trailing_commas(trimmed)).ok()?,
    };
    let object = parsed.as_object()?;
    // Exact key first, then a case-insensitive match: the model emits `URL`
    // and `City` as often as the lower-case spelling, and the `key=value`
    // branch already compares case-insensitively.
    if let Some(v) = object.get(key).and_then(|v| v.as_str()) {
        return Some(v.to_string());
    }
    object
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.as_str())
        .map(str::to_string)
}

/// Remove a *trailing* comma — one followed only by whitespace before the
/// closing `}` or `]` — ignoring commas inside string literals.
///
/// Used only as a retry after strict parsing failed. It must not alter an
/// already-valid document: an earlier version dropped every comma before a
/// closer, turning `["a","b"]` into `["a""b"]`.
///
/// Works on **bytes** throughout and only ever removes ASCII `,`/whitespace
/// ranges, so the surviving byte ranges are valid UTF-8 by construction. (A
/// previous version rebuilt the string with `byte as char`, which mapped
/// every non-ASCII byte to a different code point — `"北京"` came back as
/// `"å\u{8c}—å\u{8c}—"`.)
fn strip_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    // Byte ranges to delete, in order.
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    // Start of the last top-level comma and whether only whitespace has
    // followed it.
    let mut comma_start: Option<usize> = None;
    let mut only_whitespace_since = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                only_whitespace_since = false;
            }
            b',' => {
                comma_start = Some(i);
                only_whitespace_since = true;
            }
            b'}' | b']' => {
                if only_whitespace_since {
                    if let Some(start) = comma_start.take() {
                        cuts.push((start, i));
                    }
                }
                comma_start = None;
                only_whitespace_since = false;
            }
            _ if b.is_ascii_whitespace() => {}
            _ => only_whitespace_since = false,
        }
    }

    if cuts.is_empty() {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut last = 0usize;
    for (start, end) in cuts {
        // Drop the comma but keep the whitespace between it and the closer.
        out.push_str(&input[last..start]);
        out.push_str(&input[start + 1..end]);
        last = end;
    }
    out.push_str(&input[last..]);
    out
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
            // Redirects are followed manually in `blocking_get_with_meta`:
            // reqwest's built-in policy dials every hop before returning, so
            // the SSRF guard could only re-check the final URL *after* an
            // internal GET (and its side effects) had already occurred.
            // `Policy::none()` makes each hop explicit and vetted before it
            // is dialled.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest Client must build — check TLS backend availability")
    })
}

/// Minimal GET that returns the body as a `String`.
/// Read up to cap bytes from reader, erroring if the stream would
/// exceed the cap. Used instead of Response::text() so a server that
/// omits Content-Length (chunked encoding) cannot force us to buffer
/// an unbounded body into memory.
fn read_capped<R: Read>(mut reader: R, cap: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("read body: {e}"))?;
        if n == 0 {
            break;
        }
        if out.len() + n > cap {
            return Err(format!("body exceeds size limit {} bytes", cap));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

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

/// Maximum redirects followed by the host web tools.
const MAX_REDIRECTS: usize = 5;

fn blocking_get_with_meta(url: &str) -> std::result::Result<ResponseMeta, String> {
    // Follow redirects one hop at a time so every target is vetted *before*
    // it is dialled. The old flow let reqwest follow internally (Policy
    // limited(5)) and only re-checked the final URL, which meant a public
    // page could 302 the client onto an internal service and the GET — with
    // any side effects — had already been sent by the time the guard ran.
    let client = http_client();
    let mut current = url.to_string();
    let mut hops = 0usize;
    let resp = loop {
        validate_resolved_fetch_target(&current).map_err(|e| format!("GET {current}: {e}"))?;
        let resp = client
            .get(&current)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8",
            )
            .send()
            .map_err(|e| format!("GET {current}: {e}"))?;

        let status = resp.status();
        if !status.is_redirection() {
            break resp;
        }
        let Some(location) = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string)
        else {
            break resp; // 3xx without Location: report it as-is below.
        };
        if hops >= MAX_REDIRECTS {
            return Err(format!("GET {url}: more than {MAX_REDIRECTS} redirects"));
        }
        match crate::at_validate::resolve_http_redirect(&current, &location) {
            Some(next) => {
                current = next;
                hops += 1;
            }
            None => {
                return Err(format!(
                    "GET {current}: refusing redirect to {location:?} (cannot vet the target)"
                ))
            }
        }
    };

    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Respect the size cap before reading the full body.
    let declared = resp.content_length().unwrap_or(0) as usize;
    if declared > MAX_BODY_BYTES {
        return Err(format!(
            "GET {url}: body Content-Length {declared} bytes exceeds limit {} MB",
            MAX_BODY_BYTES / (1024 * 1024)
        ));
    }

    // Fail fast on non-2xx so we do not buffer a (possibly large) error body.
    if !(200..300).contains(&status) {
        return Err(format!(
            "GET {url} returned HTTP {status} (declared {declared} bytes)"
        ));
    }

    // Bounded streaming read (handles chunked bodies with no
    // Content-Length, which .text() would buffer unboundedly).
    let bytes = read_capped(resp, MAX_BODY_BYTES)?;
    let body = String::from_utf8_lossy(&bytes).into_owned();

    Ok(ResponseMeta {
        body,
        final_url,
        status,
        content_type,
        bytes: bytes.len(),
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
    fn url_decode_maps_literal_plus_to_space() {
        // The `+` form (as opposed to `%2B`) is the query-encoding for a
        // space; it hits a distinct match arm from the percent branch.
        assert_eq!(url_decode("a+b+c").unwrap(), "a b c");
        assert_eq!(url_decode("+").unwrap(), " ");
    }

    #[test]
    fn url_decode_passes_unreserved_bytes_through() {
        // Bytes that are neither `+` nor `%` take the generic copy arm.
        assert_eq!(url_decode("plain-._~text").unwrap(), "plain-._~text");
    }

    #[test]
    fn url_decode_rejects_non_utf8_escape() {
        // `%FF` alone is not a valid UTF-8 sequence, so the final
        // `String::from_utf8` fails and the error is surfaced.
        assert!(url_decode("%FF").is_err());
    }

    #[test]
    fn hex_digit_accepts_the_full_alphanumeric_ranges() {
        assert_eq!(hex_digit(b'0').unwrap(), 0);
        assert_eq!(hex_digit(b'9').unwrap(), 9);
        assert_eq!(hex_digit(b'a').unwrap(), 10);
        assert_eq!(hex_digit(b'f').unwrap(), 15);
        assert_eq!(hex_digit(b'A').unwrap(), 10);
        assert_eq!(hex_digit(b'F').unwrap(), 15);
    }

    #[test]
    fn unwrap_ddg_redirect_falls_back_when_the_escape_is_invalid() {
        // A malformed percent escape inside `uddg` makes `url_decode`
        // fail; the helper must return the original href, not panic.
        let raw = "//duckduckgo.com/l/?uddg=%ZZ&ss=foo";
        assert_eq!(unwrap_ddg_redirect(raw), raw);
    }

    #[test]
    fn extract_json_value_parses_whitespace_and_ignores_trailing_comma() {
        // The hand parser must skip whitespace/commas and handle a
        // trailing comma without falling off the end.
        assert_eq!(
            extract_json_value(r#"{ "url" : "https://x" , }"#, "url").unwrap(),
            "https://x"
        );
    }

    #[test]
    fn extract_json_value_rejects_non_object_and_non_string_values() {
        assert!(extract_json_value("not json", "url").is_none());
        assert!(extract_json_value(r#"{"url": 42}"#, "url").is_none());
        // A key that isn't a string at all.
        assert!(extract_json_value("{42: \"x\"}", "url").is_none());
    }

    #[test]
    fn extract_json_value_skips_a_non_matching_key_then_finds_the_target() {
        // Exercises the "value is a string, key doesn't match, keep
        // scanning" path and the final `None` when the key is absent.
        assert_eq!(
            extract_json_value(r#"{"a":"1","b":"2"}"#, "b").unwrap(),
            "2"
        );
        assert!(extract_json_value(r#"{"a":"1"}"#, "b").is_none());
    }

    #[test]
    fn extract_json_value_decodes_json_escapes() {
        // Keys and values carry JSON escapes. The previous hand-rolled
        // scanner skipped over `\\"` but did NOT unescape, so a URL or query
        // containing a quote or a backslash came back with the escape
        // sequence still in it. `serde_json` decodes it, which is what the
        // tool actually needs.
        let args = "{\"a\\\"b\":\"v\"}";
        assert_eq!(extract_json_value(args, "a\"b").unwrap(), "v");
        let args = "{\"url\":\"v\\\"w\"}";
        assert_eq!(extract_json_value(args, "url").unwrap(), "v\"w");
        // A unicode escape decodes too.
        assert_eq!(
            extract_json_value(r#"{"query":"caf\u00e9"}"#, "query").unwrap(),
            "café"
        );
    }

    /// A trailing comma is only the *last* comma before a closer. Stripping
    /// every comma that precedes `}`/`]` would silently merge array
    /// elements (`["a","b"]` → `["a""b"]`) and break a document that parsed
    /// fine without repair.
    #[test]
    fn strip_trailing_commas_leaves_valid_arrays_alone() {
        for valid in [
            r#"["a","b"]"#,
            r#"{"a":["x","y"],"b":2}"#,
            r#"{"a":[1],"b":3}"#,
            r#"{"k":"v,"}"#,
        ] {
            assert_eq!(
                strip_trailing_commas(valid),
                valid,
                "valid JSON must not be rewritten"
            );
        }
        assert_eq!(strip_trailing_commas(r#"["a","b",]"#), r#"["a","b"]"#);
        assert_eq!(strip_trailing_commas(r#"{"k":"v",}"#), r#"{"k":"v"}"#);
    }

    /// Multiple and nested trailing commas are all removed, and a comma
    /// inside a string is never touched.
    #[test]
    fn strip_trailing_commas_handles_nesting_and_repetition() {
        for (input, want) in [
            (r#"{"a":[1,],"b":2,}"#, r#"{"a":[1],"b":2}"#),
            (r#"[1,2,3,]"#, r#"[1,2,3]"#),
            (r#"{"a":1,"b":[2,3,],}"#, r#"{"a":1,"b":[2,3]}"#),
            (r#"{"nested":{"x":1,},}"#, r#"{"nested":{"x":1}}"#),
            (r#"[[1,],[2,]]"#, r#"[[1],[2]]"#),
            (r#"{"s":"a,]","t":1,}"#, r#"{"s":"a,]","t":1}"#),
        ] {
            assert_eq!(strip_trailing_commas(input), want, "input: {input}");
        }
    }

    /// Non-ASCII must survive verbatim.
    ///
    /// The repair pass used to rebuild the string by pushing each *byte* as
    /// a `char`, which mangled every multi-byte code point: `"北京"` came
    /// back as `"å\u{8c}—å\u{8c}—"`. Skipping the surgical edit entirely is
    /// not enough — the pass has to be byte-exact even when it *does* edit.
    #[test]
    fn strip_trailing_commas_preserves_utf8() {
        let with_trailing = r#"{"city":"北京","note":"café",}"#;
        assert_eq!(
            strip_trailing_commas(with_trailing),
            r#"{"city":"北京","note":"café"}"#
        );
        // No edit needed: must be byte-identical.
        let clean = r#"{"city":"北京","note":"café,"}"#;
        assert_eq!(strip_trailing_commas(clean), clean);
        // A comma *inside* a string is not a trailing comma.
        let inner = r#"{"a":"x,","b":"y,]"}"#;
        assert_eq!(strip_trailing_commas(inner), inner);
    }

    /// A non-string member must not stop the scan — the tools really receive
    /// objects like `{"timeout":5,"url":…}` and `{"max_results":10,"query":…}`.
    /// The old scanner bailed at the numeric value and the `key=value`
    /// fallback then returned the raw fragment `{"timeout":5` as the query.
    #[test]
    fn extract_json_value_skips_non_string_members() {
        assert_eq!(
            extract_json_value(r#"{"timeout":5,"url":"https://x"}"#, "url").unwrap(),
            "https://x"
        );
        assert_eq!(
            extract_json_value(r#"{"max_results":10,"query":"rust async"}"#, "query").unwrap(),
            "rust async"
        );
        // Nested objects / arrays before the target key.
        assert_eq!(
            extract_json_value(r#"{"opts":{"a":1},"list":[1,2],"city":"Paris"}"#, "city").unwrap(),
            "Paris"
        );
        // Case-insensitive fallback, matching the key=value branch.
        assert_eq!(
            extract_json_value(r#"{"URL":"https://x"}"#, "url").unwrap(),
            "https://x"
        );
    }

    #[test]
    fn parse_search_results_reads_the_legacy_layout() {
        let body = r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F">Example &amp; Co</a>
            <a class="result__snippet">A snippet here.</a>"#;
        let hits = parse_search_results(body);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/");
        assert_eq!(hits[0].title, "Example & Co");
        assert_eq!(hits[0].snippet, "A snippet here.");
    }

    #[test]
    fn parse_search_results_reads_the_modern_layout() {
        let body = r#"<a data-testid="result-title-a" href="https://modern.example/">Modern</a>
            <div data-testid="result-snippet">Modern snippet.</div>"#;
        let hits = parse_search_results(body);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://modern.example/");
        assert_eq!(hits[0].title, "Modern");
        assert_eq!(hits[0].snippet, "Modern snippet.");
    }

    #[test]
    fn parse_search_results_returns_empty_for_unrecognised_markup() {
        assert!(parse_search_results("<html><body>nothing here</body></html>").is_empty());
    }

    #[test]
    fn parse_search_results_caps_the_result_count() {
        // Build more than MAX_SEARCH_RESULTS recognisable blocks and
        // confirm the loop stops at the cap.
        let mut body = String::new();
        for i in 0..(MAX_SEARCH_RESULTS + 3) {
            body.push_str(&format!(
                r#"<a class="result__a" href="https://example.com/{i}">Title {i}</a>"#
            ));
            body.push_str(&format!(r#"<a class="result__snippet">Snippet {i}</a>"#));
        }
        let hits = parse_search_results(&body);
        assert_eq!(hits.len(), MAX_SEARCH_RESULTS);
    }

    /// A result with no snippet must not steal the *next* result's snippet.
    ///
    /// The legacy branch of the block regex is
    /// `href="…">title</a>.*?class="result__snippet"…`, and `(?s)` lets that
    /// `.*?` cross result boundaries. So when result A has no snippet, the
    /// match runs on and returns A's url/title with B's snippet — and B is
    /// consumed, never reported. The model then reads B's text as A's.
    #[test]
    fn parse_search_results_does_not_attach_a_neighbours_snippet() {
        let body = r#"<a class="result__a" href="https://a.example/">Title A</a>
            <a class="result__a" href="https://b.example/">Title B</a>
            <a class="result__snippet">Snippet of B</a>"#;
        let hits = parse_search_results(body);
        for hit in &hits {
            assert!(
                !(hit.url.contains("a.example") && hit.snippet.contains("Snippet of B")),
                "result A must not carry B's snippet: {:?}",
                hits.iter()
                    .map(|h| (&h.url, &h.snippet))
                    .collect::<Vec<_>>()
            );
        }
        // B (which does own the snippet) should still be reported.
        assert!(
            hits.iter().any(|h| h.url.contains("b.example")),
            "the result that owns the snippet must be reported: {:?}",
            hits.iter().map(|h| &h.url).collect::<Vec<_>>()
        );
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

    /// A bare `&` in prose is not an entity. The decoder used to append a
    /// `;` to it unconditionally, so page text came out as `AT&T;` and, for
    /// `R&D department`, the semicolon landed *inside* the following word
    /// (`R&D departm;ent`). That is the page content handed to the model.
    #[test]
    fn decode_html_entities_leaves_bare_ampersands_alone() {
        assert_eq!(decode_html_entities("AT&T"), "AT&T");
        assert_eq!(decode_html_entities("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(decode_html_entities("R&D department"), "R&D department");
        // An entity that *is* terminated still decodes.
        assert_eq!(decode_html_entities("AT&amp;T"), "AT&T");
        assert_eq!(decode_html_entities("R&amp;D"), "R&D");
    }

    /// Numeric entities outside Unicode's scalar range (and the lone
    /// surrogates D800–DFFF) cannot become a `char`. They are left as text
    /// rather than dropped, so the page content is never silently shortened.
    #[test]
    fn decode_html_entities_rejects_out_of_range_code_points() {
        assert_eq!(decode_html_entities("&#xD800;"), "&#xD800;");
        assert_eq!(decode_html_entities("&#1114112;"), "&#1114112;");
        // In-range ones still decode, including above the BMP.
        assert_eq!(decode_html_entities("&#x1F600;"), "😀");
        assert_eq!(decode_html_entities("&#8364;"), "€");
    }

    /// A `&` followed by too many characters, or by punctuation, is text.
    #[test]
    fn decode_html_entities_stops_at_non_entity_characters() {
        assert_eq!(
            decode_html_entities("& very long prose"),
            "& very long prose"
        );
        assert_eq!(decode_html_entities("x & y"), "x & y");
        assert_eq!(decode_html_entities("&&"), "&&");
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
        let body = "Hello<script>alert(1)</script>world<style>body{}</style>done";
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
            "Rust.".to_string(),               // score=1, len=1
            "Rust is a language.".to_string(), // score=1, len=4
            "Python.".to_string(),             // score=0
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

    #[test]
    fn validate_fetch_url_rejects_non_http() {
        assert!(validate_fetch_url("ftp://example.com/file").is_err());
        assert!(validate_fetch_url("javascript:alert(1)").is_err());
        assert!(validate_fetch_url("").is_err());
    }

    /// An `http(s)://` URL whose authority is empty parses as "no host" — the
    /// distinct branch from "not an http(s) URL at all". Both must refuse, but
    /// they report different reasons, and the empty-authority form is the one a
    /// crafted redirect would use to slip past a scheme-only check.
    #[test]
    fn validate_fetch_url_rejects_an_http_url_with_no_host() {
        assert!(validate_fetch_url("http://").is_err());
        assert!(validate_fetch_url("http:///path").is_err());
        assert!(validate_fetch_url("https://?q=1").is_err());
    }

    #[test]
    fn validate_fetch_url_rejects_loopback_hostname() {
        assert!(validate_fetch_url("http://localhost/").is_err());
        assert!(validate_fetch_url("http://foo.localhost/x").is_err());
        assert!(validate_fetch_url("http://printer.local/").is_err());
    }

    /// Trailing dots, unspecified addresses, IPv6 ULA, and the backslash
    /// authority confusion all passed the old classifier while the HTTP
    /// client dialled the local/LAN target.
    #[test]
    fn validate_fetch_url_rejects_classifier_bypasses() {
        for url in [
            "http://localhost./admin",
            "http://127.0.0.1./x",
            "http://0.0.0.0:8080/",
            "http://[::]:8080/",
            "http://[fd12:3456::1]/",
            "http://[fc00::1]/",
            "http://127.0.0.1\\@example.com/",
        ] {
            assert!(validate_fetch_url(url).is_err(), "{url} must be refused");
        }
    }

    #[test]
    fn validate_fetch_url_rejects_loopback_and_link_local_ipv4() {
        assert!(validate_fetch_url("http://127.0.0.1/").is_err());
        assert!(validate_fetch_url("http://127.8.8.8/x").is_err());
        assert!(validate_fetch_url("http://169.254.169.254/latest/meta-data/").is_err());
    }

    #[test]
    fn validate_fetch_url_rejects_private_ipv4() {
        assert!(validate_fetch_url("http://10.0.0.1/").is_err());
        assert!(validate_fetch_url("http://172.16.0.1/").is_err());
        assert!(validate_fetch_url("http://192.168.1.1/").is_err());
    }

    /// An empty port (`http://127.0.0.1:/`) is accepted by the URL client,
    /// which then dials the scheme default. The guard must still refuse these
    /// targets; it used to see `"127.0.0.1:"`, which is not a parseable
    /// address, and let them through to the network.
    #[test]
    fn validate_fetch_url_rejects_an_empty_port_authority() {
        assert!(validate_fetch_url("http://127.0.0.1:/admin").is_err());
        assert!(validate_fetch_url("http://169.254.169.254:/latest/meta-data/").is_err());
        assert!(validate_fetch_url("http://localhost:/x").is_err());
        assert!(validate_fetch_url("http://10.0.0.1:/x").is_err());
        assert!(validate_fetch_url("http://[::1]:/").is_err());
        // A public host with an explicit empty port is still a public host.
        assert!(validate_fetch_url("https://example.com:/page").is_ok());
    }

    #[test]
    fn validate_fetch_url_rejects_ipv6_loopback_and_link_local() {
        assert!(validate_fetch_url("http://[::1]/").is_err());
        assert!(validate_fetch_url("http://[fe80::1]/").is_err());
        assert!(validate_fetch_url("http://[::ffff:127.0.0.1]/").is_err());
        assert!(validate_fetch_url("http://[::ffff:192.168.0.1]/").is_err());
    }

    #[test]
    fn validate_fetch_url_accepts_public_targets() {
        assert!(validate_fetch_url("https://example.com/page").is_ok());
        assert!(validate_fetch_url("http://8.8.8.8/").is_ok());
        assert!(validate_fetch_url("https://1.1.1.1/dns").is_ok());
    }

    #[test]
    fn validate_fetch_url_strips_userinfo() {
        assert!(validate_fetch_url("https://user:pass@example.com/").is_ok());
        assert!(validate_fetch_url("http://user@127.0.0.1/x").is_err());
    }

    #[test]
    fn is_html_content_type_case_insensitive() {
        assert!(is_html_content_type("text/html"));
        assert!(is_html_content_type("Text/Html; charset=utf-8"));
        assert!(is_html_content_type("application/xhtml+xml"));
        assert!(!is_html_content_type("application/json"));
        assert!(!is_html_content_type("image/png"));
    }

    #[test]
    fn read_capped_under_limit() {
        let data = b"hello world";
        let out = read_capped(std::io::Cursor::new(data), 100).unwrap();
        assert_eq!(out, data.to_vec());
    }

    #[test]
    fn read_capped_stops_at_cap() {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        let err = read_capped(std::io::Cursor::new(data), 10).unwrap_err();
        assert!(err.contains("size limit"));
    }

    #[test]
    fn wmo_description_covers_all_codes() {
        assert_eq!(wmo_description(0), "Clear");
        assert_eq!(wmo_description(1), "Mostly clear");
        assert_eq!(wmo_description(2), "Partly cloudy");
        assert_eq!(wmo_description(3), "Overcast");
        assert_eq!(wmo_description(45), "Fog");
        assert_eq!(wmo_description(48), "Fog");
        assert_eq!(wmo_description(51), "Drizzle");
        assert_eq!(wmo_description(55), "Drizzle");
        assert_eq!(wmo_description(56), "Freezing drizzle");
        assert_eq!(wmo_description(61), "Rain");
        assert_eq!(wmo_description(66), "Freezing rain");
        assert_eq!(wmo_description(71), "Snow");
        assert_eq!(wmo_description(77), "Snow grains");
        assert_eq!(wmo_description(80), "Rain showers");
        assert_eq!(wmo_description(85), "Snow showers");
        assert_eq!(wmo_description(95), "Thunderstorm");
        assert_eq!(wmo_description(96), "Thunderstorm with hail");
        // Anything else falls through to a safe default.
        assert_eq!(wmo_description(999), "Unknown");
    }

    #[test]
    fn web_search_missing_query_is_error_before_network() {
        assert!(web_search("").unwrap_err().contains("missing 'query'"));
        assert!(web_search("query=   ").unwrap_err().contains("empty query"));
    }

    #[test]
    fn fetch_url_missing_url_is_error_before_network() {
        assert!(fetch_url("").unwrap_err().contains("missing 'url'"));
    }

    #[test]
    fn fetch_url_rejects_ssrf_target_before_network() {
        // A private/loopback URL is refused by `validate_fetch_url` before
        // any HTTP request is made — no network required.
        let err = fetch_url("url=http://127.0.0.1/admin").unwrap_err();
        assert!(
            err.contains("non-public") || err.contains("loopback"),
            "got: {err}"
        );
        let err2 = fetch_url("url=http://169.254.169.254/latest/meta-data").unwrap_err();
        assert!(err2.contains("non-public"), "got: {err2}");
    }

    /// Numeric and alternate-radix host spellings pass the lexical check
    /// (`2130706433` is not an IP literal *as written*) but `Url::parse`
    /// canonicalises them into `127.0.0.1` / `169.254.169.254`. The guard
    /// used to see the canonical form, recognise it as an IP literal and
    /// conclude "already vetted", sending the request inward.
    ///
    /// These are checked at the validation layer directly, so the test
    /// needs no network: every spelling below must be refused *before* a
    /// socket is opened.
    #[test]
    fn fetch_url_rejects_alternate_ip_spellings_before_network() {
        for url in [
            // 127.0.0.1 as a decimal integer and in hex / octal.
            "http://2130706433:8080/",
            "http://0x7f000001:8080/",
            "http://017700000001:8080/",
            // 169.254.169.254 (cloud metadata) in integer / dotted-octal /
            // hex forms.
            "http://2852039166/",
            "http://0251.0376.0251.0376/",
            "http://0xa9fea9fe/",
        ] {
            let err = validate_resolved_fetch_target(url)
                .expect_err(&format!("{url} must be refused as a non-public target"));
            assert!(
                err.contains("non-public"),
                "{url} must be refused for being non-public, got: {err}"
            );
        }
    }

    /// A public IP literal still works: the classifier runs on it and
    /// accepts it, so the fix did not turn the SSRF guard into a blanket
    /// "all literals refused".
    #[test]
    fn fetch_target_validation_accepts_a_public_literal() {
        assert!(validate_resolved_fetch_target("http://93.184.216.34/").is_ok());
    }

    #[test]
    fn fetch_url_rejects_non_http_scheme_before_network() {
        let err = fetch_url("url=ftp://example.com/file").unwrap_err();
        assert!(err.contains("non-http(s)"), "got: {err}");
    }

    #[test]
    fn webpage_summary_missing_url_is_error_before_network() {
        assert!(webpage_summary("").unwrap_err().contains("missing 'url'"));
    }

    #[test]
    fn get_weather_missing_or_empty_city_is_error_before_network() {
        assert!(get_weather("").unwrap_err().contains("missing 'city'"));
        assert!(get_weather("city=   ").unwrap_err().contains("empty city"));
    }
}
