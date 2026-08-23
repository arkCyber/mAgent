//! Concrete [`LinkAdapter`] implementations for the ESP32 firmware.
//!
//! This module is the firmware-side complement of the chip-agnostic
//! `magent_core::communication::link` trait surface. The trait itself
//! lives in `magent_core` so a host-side test can drive it; the wiring
//! against `esp_hal` peripherals lives here so `magent_core` itself
//! doesn't depend on the SoC family.
//!
//! The module is built only for the ESP32 firmware (gated on the
//! `esp-hal` drivers being present). On the host side, the
//! `magent_core::communication::manual` adapter is used instead.
//!
//! ## Implementing an adapter
//!
//! Each adapter implements the synchronous [`LinkAdapter`] trait so it
//! can be plugged into the chip-agnostic `IngressGateway`. The
//! contract — from [`magent_core::communication::link::LinkAdapter`]:
//!
//! * `poll` must be **non-blocking on the firmware side**. A long
//!   blocking read inside an embassy task starves the cooperative
//!   executor. The [`UartAdapter`] below uses `embedded_io::ReadReady`
//!   + `embedded_io::Read::read` (which returns
//!   `Err(WouldBlock)` when no data is available) precisely to keep
//!   `poll` cheap.
//! * `Ok(0)` means "no data right now"; the gateway moves on to the
//!   next adapter rather than spinning.
//!
//! ## Single-thread executor caveats
//!
//! The ESP32 firmware runs a single-thread embassy executor. There is
//! **no separate worker thread** to push bytes into an inbound queue
//! in the background — every byte has to be drained by the main
//! `ingest` loop on every iteration. The `UartAdapter::poll` below
//! relies on this: it always returns as quickly as possible, so the
//! executor gets a chance to schedule the next embassy task before
//! the next `ingest` call.

use embedded_io::{ErrorKind, ErrorType, Read, Write};

use magent_core::communication::link::{IngressSource, LinkAdapter};

/// A driver that can report how many RX bytes are currently buffered.
///
/// Used by [`UartAdapter::poll`] to avoid blocking on `embedded_io::Read`
/// (which, for `esp-idf-hal`'s `UartDriver`, waits with `delay::BLOCK` — a
/// "read until at least one byte" that can block forever with no input,
/// stalling the ingress loop and its watchdog heartbeat).
pub trait UartReadReady {
    /// Number of bytes currently in the RX buffer (0 = no data to read).
    fn rx_available(&self) -> usize;
}

impl UartReadReady for esp_idf_svc::hal::uart::UartDriver<'_> {
    fn rx_available(&self) -> usize {
        self.remaining_read().unwrap_or(0)
    }
}

// ===========================================================================
// Errors
// ===========================================================================

/// Errors returned by the firmware-side link adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterError {
    /// The peripheral returned `WouldBlock`. Not really an error — the
    /// `IngressGateway` interprets it as "no data available" and moves
    /// on. Surfaced here only so adapter users can match on it
    /// explicitly if they want.
    #[allow(dead_code)]
    WouldBlock,
    /// Generic IO error from the underlying peripheral driver.
    Io,
    /// Buffer handed to `poll` was zero-length. Caller bug.
    EmptyBuffer,
}

impl core::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // PATCHED (MicroAgent): the firmware path treats
            // `WouldBlock` as "no data" so we don't surface it to
            // the user as an error — we use the same wording as
            // `std::io::Error` would (`"operation would block"`).
            Self::WouldBlock => write!(f, "operation would block"),
            Self::Io => write!(f, "peripheral I/O error"),
            Self::EmptyBuffer => write!(f, "poll buffer was zero-length"),
        }
    }
}

impl core::error::Error for AdapterError {}

impl embedded_io::Error for AdapterError {
    fn kind(&self) -> ErrorKind {
        match self {
            // PATCHED (MicroAgent): neither `embedded-io 0.6` nor
            // `0.7` has an `ErrorKind::WouldBlock` variant — their
            // traits are blocking-only. `WouldBlock` lives in the
            // async-only `embedded-io-async` crate. We map our
            // sentinel to `Other`; the gateway never sees
            // `AdapterError::WouldBlock` in the new firmware path
            // (the inner driver returns `Ok(0)` for "no data").
            Self::WouldBlock => ErrorKind::Other,
            Self::Io => ErrorKind::Other,
            Self::EmptyBuffer => ErrorKind::InvalidInput,
        }
    }
}

// ===========================================================================
// UART (RS232) adapter
// ===========================================================================

/// Adapter around any `embedded_io::Read + ReadReady + Write` device,
/// typically `esp_hal::uart::Uart<'_, …>`.
///
/// The trait bounds deliberately accept the **synchronous**
/// `embedded_io` rather than the async `embedded_io_async` version.
///
///
/// Justification: the `LinkAdapter` trait itself is sync (see
/// `magent_core::communication::link`), so a sync peripheral plugs in
/// directly. The async version is intentionally kept out of the
/// `magent_core` API to avoid pulling `embassy-futures` into the host
/// test build.
///
/// ## Non-blocking contract
///
/// `poll` is the only place that needs to honour the non-blocking
/// contract:
///
/// ```text
/// let mut adapter = UartAdapter::new(uart);
/// loop {
///     // 64-byte scratch buffer; the gateway owns its own budget.
///     let mut buf = [0u8; 64];
///     match adapter.poll(&mut buf) {
///         Ok(0) => {/* nothing — yield */},
///         Ok(n) => {/* n bytes available in buf[..n] */},
///         Err(AdapterError::WouldBlock) => {/* nothing — yield */},
///         Err(e) => {/* real error */},
///     }
/// }
/// ```
pub struct UartAdapter<T>
where
    T: Read + Write + UartReadReady,
{
    inner: T,
    // PATCHED (MicroAgent): use `std::string::String` (the firmware
    // is `std`-only) instead of `heapless::String<16>`. This avoids
    // the heapless 0.7 (magent-core) vs 0.9 (transitive via
    // embedded-svc 0.29) dual-version problem. We bridge to the
    // 0.7 `IngressSource::Rs232.port` field in `source_kind`.
    port: std::string::String,
}

impl<T> UartAdapter<T>
where
    T: Read + Write + UartReadReady,
{
    /// Wrap a `Read + Write` peripheral. `port` is the
    /// human-readable identifier used in logs and audit records
    /// (e.g. `"UART0"`, `"UART1"`).
    #[allow(dead_code)]
    pub fn new(inner: T, port: &str) -> Self {
        Self {
            inner,
            port: std::string::String::from(port),
        }
    }

    /// Borrow the underlying peripheral. Useful for sending replies
    /// back through the same port.
    #[allow(dead_code)]
    pub fn inner(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T> ErrorType for UartAdapter<T>
where
    T: Read + Write + UartReadReady,
{
    type Error = AdapterError;
}

impl<T> LinkAdapter for UartAdapter<T>
where
    T: Read + Write + UartReadReady,
{
    type Error = AdapterError;

    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Err(AdapterError::EmptyBuffer);
        }
        // PATCHED (MicroAgent): avoid blocking on `Read::read`. For
        // esp-idf-hal's `UartDriver`, `embedded_io::Read::read` waits with
        // `delay::BLOCK` until at least one byte arrives — with no input that
        // blocks forever, stalling the ingress loop (and its watchdog
        // heartbeat). Check the RX buffer first: if empty, report "no data".
        if self.inner.rx_available() == 0 {
            return Ok(0);
        }
        match self.inner.read(buf) {
            Ok(0) => Ok(0),
            Ok(n) => Ok(n),
            Err(_) => Err(AdapterError::Io),
        }
    }

    fn send(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        if buf.is_empty() {
            return Ok(());
        }
        match self.inner.write_all(buf) {
            Ok(()) => Ok(()),
            Err(_) => Err(AdapterError::Io),
        }
    }

    fn source_kind(&self) -> IngressSource {
        // PATCHED (MicroAgent): `IngressSource::Rs232.port` is a
        // `heapless::String<16>` from magent-core's heapless 0.7,
        // while this firmware pulls heapless 0.9 transitively
        // (via embedded-svc 0.29). The two `String<N>` types are
        // nominal-distinct. We bridge through `std::string::String`
        // (the firmware is `std`-only) and truncate to 16 bytes
        // (the magent-core field's capacity).
        let mut port = heapless::String::<16>::new();
        let bytes = self.port.as_bytes();
        let take = bytes.len().min(16);
        // SAFETY-ish: `port` has capacity 16 and `bytes` is valid
        // UTF-8 (we got it from a `std::string::String` constructed
        // from `&str`), so `push_str` cannot fail here.
        let _ = port.push_str(
            core::str::from_utf8(&bytes[..take])
                .unwrap_or("?"),
        );
        IngressSource::Rs232 { port }
    }

    fn is_connected(&self) -> bool {
        // UART peripherals on ESP32 are always connected (they're
        // wired to pins, not negotiated like TCP). A real product
        // could check for `framing error` / `break` status here.
        true
    }
}

// ===========================================================================
// SPI adapter
// ===========================================================================

/// Adapter around any `embedded_io::Read + Write` device that is
/// driven by an external chip-select signal — typically
/// `esp_hal::spi::Spi<'_, …>` with a separately-held `OutputPin`
/// for CS.
///
/// `SpiAdapter` is constructed from an already-initialised SPI
/// peripheral plus a closure that toggles the chip-select pin.
/// Adapters that share a SPI bus with multiple slaves pass different
/// `cs_toggle` closures and rely on `IngressSource::Spi { cs_pin }`
/// to keep their frames distinguishable in audit records.
///
/// ## Read/write framing
///
/// SPI is a full-duplex, transaction-oriented bus. Each frame
/// consists of one `write` (driving the slave) followed by one
/// `read` (sampling the slave's reply). The adapter treats both
/// halves as a single ingress frame — the `write` half is the
/// "command" the gateway sends; the `read` half is the data the
/// gateway hands upstream.
///
/// The current implementation **does not auto-write a command
/// byte** — callers must drive `send` themselves before `poll` if
/// they want a full-duplex exchange. This keeps `poll` strictly
/// read-only, matching the [`LinkAdapter`] contract.
#[allow(dead_code)]
pub struct SpiAdapter<T, F>
where
    T: Read + Write,
    F: FnMut(bool),
{
    inner: T,
    cs_pin: u8,
    /// Toggles the chip-select pin (`true` = asserted / low). Held by
    /// the adapter so it can deselect after each transaction.
    cs_toggle: F,
}

impl<T, F> SpiAdapter<T, F>
where
    T: Read + Write,
    F: FnMut(bool),
{
    /// Wrap an SPI peripheral. `cs_pin` is the chip-select identifier
    /// (recorded in `IngressSource::Spi`); `cs_toggle` is a closure
    /// that the adapter calls with `true` to assert CS and `false`
    /// to deassert it.
    #[allow(dead_code)]
    pub fn new(inner: T, cs_pin: u8, cs_toggle: F) -> Self {
        Self {
            inner,
            cs_pin,
            cs_toggle,
        }
    }

    /// Run a full SPI transaction: assert CS → write `cmd` → read
    /// into `resp` → deassert CS. This is the most common use case
    /// for sensor reads; it is exposed here so callers don't have
    /// to borrow the inner peripheral directly.
    ///
    /// Returns `Err(AdapterError::Io)` on any underlying peripheral
    /// error. Does NOT retry — the gateway will treat the error as a
    /// transient adapter failure and move on.
    #[allow(dead_code)]
    pub fn transact(&mut self, cmd: &[u8], resp: &mut [u8]) -> Result<(), AdapterError> {
        (self.cs_toggle)(true);
        let write_result = self.inner.write_all(cmd);
        let read_result = if write_result.is_ok() && !resp.is_empty() {
            self.inner.read_exact(resp)
        } else {
            Ok(())
        };
        (self.cs_toggle)(false);
        if write_result.is_err() || read_result.is_err() {
            Err(AdapterError::Io)
        } else {
            Ok(())
        }
    }
}

impl<T, F> ErrorType for SpiAdapter<T, F>
where
    T: Read + Write,
    F: FnMut(bool),
{
    type Error = AdapterError;
}

impl<T, F> LinkAdapter for SpiAdapter<T, F>
where
    T: Read + Write,
    F: FnMut(bool),
{
    type Error = AdapterError;

    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Err(AdapterError::EmptyBuffer);
        }
        // A real SPI driver typically needs a CS toggle and a
        // command byte before data comes back. To keep the adapter
        // a pure "read what's currently in the RX FIFO" helper (so
        // it matches the `LinkAdapter` contract), callers should
        // invoke `transact` *before* calling `poll`. If the caller
        // forgets, `poll` will simply return whatever is in the RX
        // buffer, which for a freshly-reset peripheral is typically
        // zero bytes — i.e. `Ok(0)`, signalling "no data".
        match self.inner.read(buf) {
            Ok(0) => Ok(0),
            Ok(n) => Ok(n),
            // embedded-io 0.7 has no `WouldBlock` variant. See
            // `UartAdapter::poll` for the full rationale.
            Err(_) => Err(AdapterError::Io),
        }
    }

    fn send(&mut self, _buf: &[u8]) -> Result<(), Self::Error> {
        // Outbound SPI commands are wrapped by `transact`; the
        // generic `send` is intentionally a no-op so the adapter
        // doesn't accidentally issue a CS-asserted write without a
        // matching read.
        Ok(())
    }

    fn source_kind(&self) -> IngressSource {
        IngressSource::Spi {
            cs_pin: self.cs_pin,
        }
    }

    fn is_connected(&self) -> bool {
        true
    }
}

// ===========================================================================
// GPIO button adapter (single-byte "press" events)
// ===========================================================================

/// Adapter that turns a GPIO button press into a single-byte ingress
/// frame. The byte is `0x01` for "pressed", `0x00` for "released".
///
/// The adapter keeps an internal state machine so a single button
/// press produces exactly one frame; the gateway de-duplicates
/// further polls while the button stays pressed.
///
/// Designed for ESP32 development boards that wire a momentary push
/// button to a single GPIO pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum ButtonState {
    Idle,
    Pressed,
}

#[allow(dead_code)]
pub struct ButtonAdapter<P>
where
    P: embedded_hal::digital::InputPin,
{
    pin: P,
    state: ButtonState,
    /// Logical label for `IngressSource::Other { label }`. Usually
    /// something like `"btn:boot"` so logs / audit entries can be
    /// correlated with the GPIO number.
    label: std::string::String,
    /// Optional press counter (incremented on every press event).
    press_count: u32,
}

impl<P> ButtonAdapter<P>
where
    P: embedded_hal::digital::InputPin,
{
    /// Wrap an `InputPin` (e.g. `esp_hal::gpio::Input<'static>`).
    #[allow(dead_code)]
    pub fn new(pin: P, label: &str) -> Self {
        Self {
            pin,
            state: ButtonState::Idle,
            label: std::string::String::from(label),
            press_count: 0,
        }
    }

    /// How many times the button has been pressed since boot.
    #[allow(dead_code)]
    pub fn press_count(&self) -> u32 {
        self.press_count
    }
}

impl<P> LinkAdapter for ButtonAdapter<P>
where
    P: embedded_hal::digital::InputPin,
{
    type Error = AdapterError;

    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Err(AdapterError::EmptyBuffer);
        }
        // `is_low()` returns `Result<bool, _>`; we treat any error
        // as "pin not yet initialised" and report `WouldBlock` so
        // the gateway moves on rather than tearing the agent down.
        let is_pressed = self.pin.is_low().unwrap_or(false);
        match (self.state, is_pressed) {
            (ButtonState::Idle, true) => {
                self.state = ButtonState::Pressed;
                self.press_count = self.press_count.saturating_add(1);
                buf[0] = 0x01;
                Ok(1)
            }
            (ButtonState::Pressed, false) => {
                self.state = ButtonState::Idle;
                buf[0] = 0x00;
                Ok(1)
            }
            _ => Ok(0),
        }
    }

    fn send(&mut self, _buf: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn source_kind(&self) -> IngressSource {
        // PATCHED (MicroAgent): bridge `std::string::String` (our
        // adapter field) to `heapless::String<32>` (magent-core's
        // heapless 0.7 type for `IngressSource::Other::label`).
        let mut label = heapless::String::<32>::new();
        let bytes = self.label.as_bytes();
        let take = bytes.len().min(32);
        let _ = label.push_str(
            core::str::from_utf8(&bytes[..take]).unwrap_or("?"),
        );
        IngressSource::Other { label }
    }

    fn is_connected(&self) -> bool {
        true
    }
}