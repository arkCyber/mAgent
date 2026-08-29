//! Aeronautical-grade error type for the Lua scripting host.
//!
//! Every fallible operation in this crate returns [`Result`] (an alias for
//! [`std::result::Result`] with [`LuaHostError`]). Errors are always
//! propagated explicitly — never swallowed and never turned into a panic.

use std::fmt;

/// All failure modes surfaced by `magent-lua`.
#[derive(Debug)]
pub enum LuaHostError {
    /// A Lua parse / runtime / resource-budget error. The embedded message is
    /// the interpreter's own error text, so it is user-visible but never
    /// fatal to the host process.
    Lua(String),
    /// `agent.reason()` failed to produce an answer (agent init, mutex
    /// poisoned, or the embedded `MiniAgent` returned an error).
    Agent(String),
    /// A `hardware.*` call was rejected or failed on the backend.
    Hardware(String),
    /// A script attempted something the sandbox forbids (unknown stdlib,
    /// budget exceeded, etc.). Reported through [`LuaHostError::Lua`] in most
    /// cases; kept as a distinct variant for host-side wiring errors.
    Sandbox(String),
    /// The host was misconfigured (bad backend wiring, failed binding).
    Config(String),
}

impl fmt::Display for LuaHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LuaHostError::Lua(m) => write!(f, "lua: {m}"),
            LuaHostError::Agent(m) => write!(f, "agent: {m}"),
            LuaHostError::Hardware(m) => write!(f, "hardware: {m}"),
            LuaHostError::Sandbox(m) => write!(f, "sandbox: {m}"),
            LuaHostError::Config(m) => write!(f, "config: {m}"),
        }
    }
}

impl std::error::Error for LuaHostError {}

#[cfg(feature = "mlua")]
impl From<mlua::Error> for LuaHostError {
    /// Map every `mlua` error (parse, runtime, budget, longjmp boundary) to a
    /// [`LuaHostError::Lua`]. This is the single choke-point that guarantees a
    /// Lua error becomes a `Result` and never a panic.
    fn from(e: mlua::Error) -> Self {
        LuaHostError::Lua(e.to_string())
    }
}

/// Convenience alias so the crate reads `Result<T>` instead of the verbose
/// `std::result::Result<T, LuaHostError>`.
pub type Result<T> = std::result::Result<T, LuaHostError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders_each_variant_with_prefix() {
        assert_eq!(LuaHostError::Lua("boom".into()).to_string(), "lua: boom");
        assert_eq!(LuaHostError::Agent("a".into()).to_string(), "agent: a");
        assert_eq!(
            LuaHostError::Hardware("h".into()).to_string(),
            "hardware: h"
        );
        assert_eq!(LuaHostError::Sandbox("s".into()).to_string(), "sandbox: s");
        assert_eq!(LuaHostError::Config("c".into()).to_string(), "config: c");
    }

    #[test]
    fn debug_is_derived() {
        assert!(format!("{:?}", LuaHostError::Agent("m".into())).contains("Agent"));
    }

    #[test]
    fn is_a_std_error_with_no_source() {
        let e = LuaHostError::Lua("x".into());
        // The error must satisfy `std::error::Error` (and have no `source`).
        assert!(std::error::Error::source(&e).is_none());
    }

    #[test]
    fn result_alias_works() {
        fn fallible(ok: bool) -> Result<u8> {
            if ok {
                Ok(1)
            } else {
                Err(LuaHostError::Config("nope".into()))
            }
        }
        assert_eq!(fallible(true).unwrap(), 1);
        assert!(fallible(false)
            .unwrap_err()
            .to_string()
            .starts_with("config:"));
    }

    #[cfg(feature = "mlua")]
    #[test]
    fn maps_mlua_error_to_lua_variant() {
        // `From<mlua::Error>` is the single choke-point guaranteeing a Lua
        // error surfaces as a `Result`, never a panic.
        let e = LuaHostError::from(mlua::Error::runtime("boom"));
        assert!(matches!(e, LuaHostError::Lua(_)));
        assert!(e.to_string().starts_with("lua:"));
    }
}
