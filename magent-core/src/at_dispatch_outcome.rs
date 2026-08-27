//! Result type for AT command dispatch.
//!
//! Lives in `magent-core` (rather than the firmware) so the
//! pure-logic validators in `at_validate.rs` and unit tests
//! across the workspace can reference it without dragging in
//! `esp_idf_svc` and the rest of the firmware stack.
//!
//! The wire renderer (`render_outcome`) stays in the firmware
//! because it touches the UART output buffer, which is
//! firmware-specific. The data carried in `AtOutcome::Ok` is a
//! bounded heapless string so the renderer is purely mechanical
//! (no formatting, no allocation).

use heapless::String as HeaplessString;

/// Maximum length of a single `+CMDxxx:value` reply line.
pub const REPLY_LINE_MAX: usize = 256;

/// What we did with a command. Drives the reply machinery.
///
/// `#[allow(clippy::large_enum_variant)]`: the `Ok { data }` variant carries a
/// 256-byte reply buffer. Boxing it would save ~248 bytes of stack per value
/// but would add a heap allocation on the hot AT dispatch path (and the S3/C61
/// already heap-allocate the reply line in most handlers). The enum is
/// returned by value one-at-a-time and never stored in a Vec, so the 256-byte
/// variant is acceptable — boxing is a deliberate trade-off we have not made.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq, Eq)]
pub enum AtOutcome {
    /// Send only `OK\r\n`.
    NoReply,
    /// Send `data\r\n` (a single line) followed by `OK\r\n`.
    Ok {
        /// The reply line payload, capped at `REPLY_LINE_MAX` bytes.
        data: HeaplessString<REPLY_LINE_MAX>,
    },
    /// Send `+CMDER:<code>\r\nERROR\r\n`.
    Error {
        /// The numeric `+CMDER:<n>` error code to emit.
        code: u8,
    },
}

impl AtOutcome {
    /// Convenience constant for the common "successful no-data" reply.
    pub const OK: Self = AtOutcome::NoReply;

    /// Build an `AtOutcome::Ok` from a static-ish string. The string
    /// is copied into a `HeaplessString<REPLY_LINE_MAX>` so the
    /// original can be freed after the call returns (useful for
    /// `&str` literals and short formatted lines).
    pub fn ok_line<S: AsRef<str>>(s: S) -> Self {
        let mut line: HeaplessString<REPLY_LINE_MAX> = HeaplessString::new();
        let _ = line.push_str(s.as_ref());
        AtOutcome::Ok { data: line }
    }

    /// Build an `AtOutcome::Error`. The code is the numeric
    /// identifier used in the `+CMDER:<code>` reply.
    pub fn error(code: u8) -> Self {
        AtOutcome::Error { code }
    }
}

impl Clone for AtOutcome {
    fn clone(&self) -> Self {
        match self {
            AtOutcome::NoReply => AtOutcome::NoReply,
            AtOutcome::Ok { data } => AtOutcome::Ok {
                data: HeaplessString::clone(data),
            },
            AtOutcome::Error { code } => AtOutcome::Error { code: *code },
        }
    }
}
