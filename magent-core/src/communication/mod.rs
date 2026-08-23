//! Communication layer for mAgent.
//!
//! Splits the agent's external data ingress into two layers with distinct
//! responsibilities:
//!
//! 1. **Link adapters** ([`link`]) — physical / protocol-level byte
//!    transports (BLE, MQTT, RS232, SPI, manual CLI input, …). Each
//!    adapter implements [`link::LinkAdapter`] which exposes a minimal
//!    `poll` / `send` API. New transports plug in by implementing the
//!    trait, without changing anything above this layer.
//!
//! 2. **Ingress gateway** (in `crate::ingress`) — application-level
//!    routing. Takes bytes from any number of adapters, tags them with
//!    their source, optionally wraps them in an Ed25519-signed
//!    [`crate::web3::SignedMessage`], and hands the result to the agent
//!    loop / blockchain tools. This is the "router" role previously
//!    discussed — kept separate from the link layer so that adding
//!    RS232 or SPI never requires touching the agent loop.
//!
//! ## Module map
//!
//! ```text
//! +-----------------+    +-----------------+    +-----------------+
//! | MqttAdapter     |    | Rs232Adapter    |    | ManualAdapter   |   ...
//! +--------+--------+    +--------+--------+    +--------+--------+
//!          |                      |                      |
//!          +----------------------+----------------------+
//!                                 v
//!                       +-----------------+
//!                       | IngressGateway  |  (ingress module)
//!                       +--------+--------+
//!                                v
//!                  +---------------------+
//!                  | web3::SignedMessage |
//!                  +-----------+---------+
//!                              v
//!                       Agent / chain tools
//! ```
//!
//! ## Feature flags
//!
//! * [`link`] (always compiled when the parent `communication` module is
//!   in scope) — exposes the `LinkAdapter` trait and shared source enum.
//!   It depends only on `core` / `alloc` so it compiles in `no_std`.
//! * `mqtt` / `manual` — gate individual concrete adapters. The `mqtt`
//!   adapter is a host-only stub that lets host tests push frames
//!   synchronously (the real broker client lives in `examples/` so
//!   `magent-core` doesn't have to depend on `tokio`); the `manual`
//!   adapter reads from stdin and is host-only.
//! * `ingress` (top-level feature on `magent-core`) — pulls in the
//!   `crate::ingress` module which owns [`crate::ingress::IngressGateway`].
//!
//! ## TODO(heapless-signed-message)
//!
//! `IngressGateway::ingest` currently calls
//! `SignedMessage::to_json() -> alloc::string::String`, which heap-
//! allocates per frame. On ESP32 the heap is fine for the 72 KiB
//! budget but a single-byte alloc per frame is wasteful. A future
//! change should add
//! `SignedMessage::to_json_into(&mut [u8]) -> Result<usize, Web3ErrorKind>`
//! so `IngressGateway::build_frame` can write into a stack-resident
//! `heapless::String<MAX_PAYLOAD>` without touching the allocator.

// BLE implementation lives in its own file; re-export so the public
// path `magent_core::communication::BleClient` (and the rest of the
// pre-existing surface) keeps working.
mod ble;
pub use ble::*;

// Link-layer abstractions: the `LinkAdapter` trait + shared
// `IngressSource` enum. This sub-module is always compiled (it only
// needs `core`/`alloc`) so embedded builds without `mqtt` / `manual`
// still get the trait surface they need to write their own adapters
// (e.g. an embassy-nrf SPI driver).
pub mod link;

// Host-only concrete adapters. Each is gated behind its own feature
// so embedded builds don't drag in `std::io` / `std::net`.
#[cfg(feature = "manual")]
pub mod manual;
#[cfg(feature = "mqtt")]
pub mod mqtt;