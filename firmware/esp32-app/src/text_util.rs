//! Small text helpers shared by the AT/ingress/tool adapters.

/// Copy `s` into a `heapless::String<N>`, stopping at a UTF-8 character
/// boundary.
///
/// The obvious spelling —
/// `heapless::String::push_str(core::str::from_utf8(&bytes[..take]).unwrap_or("?"))`
/// — is wrong twice over. Slicing at a raw byte offset can split a multi-byte
/// code point, which makes `from_utf8` fail, and `unwrap_or("?")` then discards
/// the **entire** payload in favour of a single question mark. A Chinese web
/// preview (`fetch_web`) or a Chinese city (`get_weather`) crossing the 255-byte
/// `ToolResult::data` budget therefore reached the agent as `"?"`.
///
/// Backing the cut off to the previous boundary keeps the prefix the caller
/// asked to keep, which is the whole point of a bounded preview.
pub fn push_str_truncated<const N: usize>(s: &str, out: &mut heapless::String<N>) {
    // Budget against the *remaining* capacity, not the total. Using `N` here
    // made the function only correct for an empty `out`: a caller appending to
    // a non-empty buffer asked `push_str` for more bytes than fit, which
    // returned `Err` — silently discarded by the `let _` below — so the whole
    // append vanished. (All current callers pass a fresh buffer, so this was
    // latent, not live.)
    let remaining = N.saturating_sub(out.len());
    let mut end = s.len().min(remaining);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    // Cannot fail: the slice is valid UTF-8 by construction and
    // `end <= N - out.len()`, so it always fits the remaining capacity.
    let _ = out.push_str(&s[..end]);
}

#[cfg(test)]
mod tests {
    // The firmware crate links esp-idf, so these run only under a device
    // target; keep them dependent on nothing but `heapless`.
    use super::push_str_truncated;

    #[test]
    fn keeps_short_input_verbatim() {
        let mut out = heapless::String::<16>::new();
        push_str_truncated("hello", &mut out);
        assert_eq!(out.as_str(), "hello");
    }

    #[test]
    fn truncates_on_char_boundary_not_mid_codepoint() {
        // "上海" is three bytes per character; a 4-byte budget must yield one
        // character, not two bytes of a second one.
        let mut out = heapless::String::<4>::new();
        push_str_truncated("上海", &mut out);
        assert_eq!(out.as_str(), "上");
    }

    #[test]
    fn never_replaces_the_payload_with_a_question_mark() {
        let mut out = heapless::String::<4>::new();
        push_str_truncated("上海天气", &mut out);
        assert!(!out.is_empty());
        assert_eq!(out.as_str(), "上");
    }

    #[test]
    fn appends_up_to_the_remaining_capacity() {
        // Regression: budgeting against the total capacity `N` instead of the
        // remaining space made an append to a non-empty buffer fail (and get
        // silently dropped). "he" (2 B) + room for 2 more must keep "hexy",
        // not lose the second call.
        let mut out = heapless::String::<4>::new();
        push_str_truncated("he", &mut out);
        push_str_truncated("xyz", &mut out);
        assert_eq!(out.as_str(), "hexy");
    }
}
