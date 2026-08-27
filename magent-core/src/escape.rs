//! Minimal, allocation-light escaping helpers for rendering untrusted
//! operator-controlled strings (SSID, IP, model names, …) into output that
//! will be parsed or displayed by another component (JSON over HTTP, HTML).
//!
//! Lives in `magent-core` (chip-agnostic, `no_std` + `alloc`) so the same
//! helpers are usable from the host test suite AND from the ESP32 / nRF52
//! firmware — and so the escaping rules can be locked down with unit tests
//! against malicious inputs without needing an SoC toolchain.

use alloc::string::String;

/// Escape `s` for safe embedding inside a JSON **string literal** (the part
/// between the surrounding double quotes).
///
/// Handles `"`, `\`, `\n`, `\r`, `\t`, and all other control characters
/// (`U+0000..U+001F`) as `\uXXXX`. Other characters (including non-ASCII
/// UTF-8) pass through unchanged, which is valid JSON.
pub fn json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use alloc::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Escape `s` for safe embedding inside an HTML text / attribute context.
///
/// Covers the five characters that can break out of an HTML element or
/// attribute (`&`, `<`, `>`, `"`, `'`). Prevents the `ssid` / `ip` fields
/// rendered by the web dashboard from turning into injected markup (XSS).
pub fn html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{html, json};

    #[test]
    fn json_escapes_quotes_and_backslashes() {
        assert_eq!(json(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn json_escapes_whitespace_and_control() {
        assert_eq!(json("line1\nline2\ttab"), r#"line1\nline2\ttab"#);
        // U+0008 (backspace) -> \u0008
        assert_eq!(json("a\u{0008}b"), r"a\u0008b");
    }

    #[test]
    fn json_passes_ascii_and_utf8_through() {
        assert_eq!(json("plain-ssid"), "plain-ssid");
        assert_eq!(json("wifi-网络-5G"), "wifi-网络-5G");
    }

    #[test]
    fn json_round_trip_via_parse() {
        // Escaped output must be valid JSON that decodes back to the input.
        let evil = "say \"hi\"\nwith <tag> & 'quote'";
        let escaped = json(evil);
        let decoded: serde_json::Value = serde_json::from_str(&format!("\"{escaped}\"")).unwrap();
        assert_eq!(decoded.as_str().unwrap(), evil);
    }

    #[test]
    fn html_escapes_metacharacters() {
        assert_eq!(
            html("<script>alert(\"x\") & 'y'</script>"),
            "&lt;script&gt;alert(&quot;x&quot;) &amp; &#39;y&#39;&lt;/script&gt;"
        );
    }

    #[test]
    fn html_passes_safe_text_through() {
        assert_eq!(html("mAgent-ESP32-S3"), "mAgent-ESP32-S3");
        assert_eq!(html("wifi-网络"), "wifi-网络");
    }
}
