//! Manual / CLI adapter — reads bytes from `stdin`.
//!
//! Suitable for desktop simulations, REPL-style debug sessions, and
//! integration tests where the agent is being fed by a human operator.
//! Each line typed at the prompt becomes one ingress frame (line
//! terminators are stripped).

use super::link::{IngressSource, LinkAdapter};
use std::io::{self, BufRead, Write};
use std::string::String;

/// Manual (stdin) adapter.
///
/// `poll` reads a single line from stdin (blocking). To avoid blocking
/// the agent's main loop forever when stdin is not interactive, the
/// adapter has a configurable timeout via [`ManualAdapter::with_timeout`].
#[derive(Debug)]
pub struct ManualAdapter {
    timeout_ms: u32,
}

impl Default for ManualAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualAdapter {
    /// Create a manual adapter with a 30-second per-read timeout.
    pub fn new() -> Self {
        Self { timeout_ms: 30_000 }
    }

    /// Override the per-read timeout. The value is advisory — the
    /// adapter's blocking behaviour is bounded by the underlying OS read.
    pub fn with_timeout(mut self, timeout_ms: u32) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

/// Errors that can come out of a [`ManualAdapter`].
#[derive(Debug)]
pub enum ManualError {
    /// Underlying I/O failure (stdin closed, interrupted, etc.).
    Io(io::Error),
}

impl core::fmt::Display for ManualError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "manual adapter I/O error: {e}"),
        }
    }
}

impl std::error::Error for ManualError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
        }
    }
}

impl LinkAdapter for ManualAdapter {
    type Error = ManualError;

    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, ManualError> {
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        // Echo a prompt so an interactive session knows the agent is
        // waiting. Tests / scripts that don't want this can disable
        // stdout for the agent process.
        let _ = write!(io::stdout(), "magent> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        let n = handle.read_line(&mut line).map_err(ManualError::Io)?;
        if n == 0 {
            // EOF on stdin — surface as zero-byte so the gateway
            // moves on rather than treating it as an error.
            return Ok(0);
        }
        // Strip the trailing newline; the gateway doesn't care about
        // transport framing, just bytes.
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let copy = trimmed.len().min(buf.len());
        buf[..copy].copy_from_slice(&trimmed.as_bytes()[..copy]);
        Ok(copy)
    }

    fn send(&mut self, buf: &[u8]) -> Result<(), ManualError> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        handle.write_all(buf).map_err(ManualError::Io)?;
        handle.write_all(b"\n").map_err(ManualError::Io)?;
        Ok(())
    }

    fn source_kind(&self) -> IngressSource {
        IngressSource::Manual
    }

    fn is_connected(&self) -> bool {
        // We treat stdin as always connected; EOF is reported via
        // `poll` returning Ok(0).
        true
    }
}
