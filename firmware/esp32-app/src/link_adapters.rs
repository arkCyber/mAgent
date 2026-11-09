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
    /// The peripheral stopped accepting bytes mid-buffer, so only a prefix of
    /// the frame was sent. Distinct from `Io` (which is a driver error): this
    /// is a TX stall/timeout with a known partial-write. Returned instead of
    /// `Ok(())` so a truncated AT/agent reply is reported as a send failure
    /// rather than as "reply sent to host".
    Truncated,
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
            Self::Truncated => write!(f, "peripheral stalled before the whole buffer was sent"),
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
            Self::Truncated => ErrorKind::WriteZero,
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
    /// Line reassembly buffer. `poll` returns exactly one complete
    /// `\r`/`\n`-terminated line at a time.
    line: std::vec::Vec<u8>,
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
            line: std::vec::Vec::new(),
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
        //
        // Once bytes are available, drain them into the line buffer and
        // return **one complete line**. The gateway treats each `poll` result
        // as one frame, so returning a raw chunk split a long command into a
        // truncated prefix plus a remainder that was then routed to the LLM
        // as natural language, and merged two back-to-back commands into one
        // unparseable frame.
        loop {
            // A terminated line already buffered: hand it out.
            if let Some(pos) = self.line.iter().position(|b| *b == b'\n' || *b == b'\r') {
                let mut rest = self.line.split_off(pos + 1);
                let mut line: std::vec::Vec<u8> = std::mem::take(&mut self.line);
                // Drop the terminator (and a following `\n` of a CRLF pair,
                // which `split_off` left at the head of `rest`).
                line.truncate(pos);
                if rest.first() == Some(&b'\n') || rest.first() == Some(&b'\r') {
                    rest.remove(0);
                }
                self.line = rest;
                let n = line.len().min(buf.len());
                buf[..n].copy_from_slice(&line[..n]);
                return Ok(n);
            }

            if self.inner.rx_available() == 0 {
                return Ok(0);
            }
            let mut chunk = [0u8; 64];
            match self.inner.read(&mut chunk) {
                Ok(0) => return Ok(0),
                Ok(n) => {
                    self.line.extend_from_slice(&chunk[..n]);
                    // Bound the reassembly buffer: an unterminated flood must
                    // not grow without limit (~1 KiB is well above the 256-byte
                    // AT line cap).
                    const MAX_LINE: usize = 1024;
                    if self.line.len() > MAX_LINE {
                        self.line.clear();
                        return Ok(0);
                    }
                }
                Err(_) => return Err(AdapterError::Io),
            }
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
        crate::text_util::push_str_truncated(&self.port, &mut port);
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
        crate::text_util::push_str_truncated(&self.label, &mut label);
        IngressSource::Other { label }
    }

    fn is_connected(&self) -> bool {
        true
    }
}

// ===========================================================================
// Native USB-Serial/JTAG adapter (ESP32-S3)
// ===========================================================================

/// Adapter around the ESP32's **native** USB-Serial/JTAG console.
///
/// ## Why this exists
///
/// ESP32-S3 dev boards expose the chip's native USB peripheral
/// (USB-Serial/JTAG, D-/D+ = GPIO19/20) to the host — there is no USB-UART
/// bridge wired to the UART0 pins. The console log output is mirrored to that
/// peripheral (`CONFIG_ESP_CONSOLE_SECONDARY_USB_SERIAL_JTAG=y`), so the
/// operator *sees* everything on `/dev/cu.usbmodem*`, but the UART0 ingress
/// adapter only reads the physical U0RXD pin: any command typed on the USB
/// console (`AT+GMR`, `read the temperature`) used to be silently dropped.
/// Registering this adapter as a second ingress link makes that very same
/// console bidirectional — commands in, replies / `+CMDER:<n>` errors out.
///
/// ## Non-blocking contract
///
/// [`UartAdapter::poll`] can rely on `UartDriver::remaining_read`; the
/// USB-Serial/JTAG driver has no "bytes available" query, so `poll` calls the
/// driver with a **zero-tick timeout** (`delay::NON_BLOCK`). That returns
/// `Ok(0)` when nothing is buffered instead of blocking — the blocking
/// `embedded_io::Read` impl of the driver waits with `delay::BLOCK`, which
/// would stall the ingress loop and trip its watchdog heartbeat.
///
/// Gated on `board-s3`: the C61 DevKit's console goes through a USB-UART
/// bridge on UART0, so the UART0 adapter already covers it there.
#[cfg(feature = "board-s3")]
pub struct UsbSerialAdapter {
    inner: esp_idf_svc::hal::usb_serial::UsbSerialDriver<'static>,
    port: std::string::String,
}

#[cfg(feature = "board-s3")]
impl UsbSerialAdapter {
    /// Wrap the USB-Serial/JTAG driver. `port` is the human-readable
    /// identifier used in logs / audit records (e.g. `"USB-JTAG"`).
    pub fn new(inner: esp_idf_svc::hal::usb_serial::UsbSerialDriver<'static>, port: &str) -> Self {
        Self {
            inner,
            port: std::string::String::from(port),
        }
    }
}

#[cfg(feature = "board-s3")]
impl ErrorType for UsbSerialAdapter {
    type Error = AdapterError;
}

#[cfg(feature = "board-s3")]
impl LinkAdapter for UsbSerialAdapter {
    type Error = AdapterError;

    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Err(AdapterError::EmptyBuffer);
        }
        // `NON_BLOCK` (0 ticks) = "hand back whatever is buffered right now".
        match self.inner.read(buf, esp_idf_svc::hal::delay::NON_BLOCK) {
            // The vendored `UsbSerialDriver::read` returns `Ok(len as _)` from
            // the C `int` without mapping errors, so a driver error surfaces as
            // a huge `usize` instead of `Err`. Trust `buf`, not the count: a
            // count past `buf.len()` means nothing was written (or the buffer
            // was already full), and passing it on would make the gateway slice
            // `&scratch[..n]` out of range on the Core-1 ingress thread.
            Ok(n) if n <= buf.len() => Ok(n),
            Ok(n) => {
                log::warn!(
                    "[ingress] USB-JTAG read reported {n}B into a {}B buffer — treating as I/O error",
                    buf.len()
                );
                Err(AdapterError::Io)
            }
            Err(_) => Err(AdapterError::Io),
        }
    }

    fn send(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        if buf.is_empty() {
            return Ok(());
        }
        // Bounded, non-blocking write loop: if the host is not draining the
        // USB TX FIFO we give up after ~100 ms instead of blocking the ingress
        // thread forever (the blocking `Write` impl waits with `delay::BLOCK`).
        let mut offset = 0usize;
        let mut stalls = 0u32;
        while offset < buf.len() {
            match self
                .inner
                .write(&buf[offset..], esp_idf_svc::hal::delay::NON_BLOCK)
            {
                Ok(0) => {
                    stalls += 1;
                    if stalls > 50 {
                        log::warn!(
                            "[ingress] USB-JTAG TX stalled after {offset}/{}B — dropping remainder",
                            buf.len()
                        );
                        // Report the partial write as an error. Returning `Ok`
                        // here made the caller log "reply sent to host" for a
                        // reply whose tail was silently discarded — the host
                        // would then act on a truncated AT/agent response.
                        return Err(AdapterError::Truncated);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                // Same `-1 as usize` narrowing as `poll`: a count larger than
                // what we handed the driver means the driver errored. Advancing
                // `offset` by it would end the loop and report `Ok(())` — a
                // silent "reply sent" for a reply that was never transmitted.
                Ok(n) if n > buf.len() - offset => {
                    log::warn!(
                        "[ingress] USB-JTAG write reported {n}B for {}B remaining — treating as I/O error",
                        buf.len() - offset
                    );
                    return Err(AdapterError::Io);
                }
                Ok(n) => {
                    offset += n;
                    stalls = 0;
                }
                Err(_) => return Err(AdapterError::Io),
            }
        }
        Ok(())
    }

    fn source_kind(&self) -> IngressSource {
        // Same `std::string::String` → `heapless::String<16>` bridge as
        // `UartAdapter::source_kind`.
        let mut port = heapless::String::<16>::new();
        crate::text_util::push_str_truncated(&self.port, &mut port);
        IngressSource::Rs232 { port }
    }

    fn is_connected(&self) -> bool {
        // Report the peripheral's real host-attachment state. The ingress
        // gateway skips disconnected adapters, so a constant `true` made it
        // poll a port whose host was gone (the driver returns `Ok(0)` either
        // way, but the gateway's diagnostics then claimed a live link). The
        // vendored HAL's `UsbSerialDriver::is_connected` asks
        // `usb_serial_jtag_is_connected()`, which is exactly this bit.
        self.inner.is_connected()
    }
}

// ===========================================================================
// ESP32 ingress link (UART0 + native USB-Serial/JTAG)
// ===========================================================================

/// Sum type over the board's physical console links.
///
/// `IngressGateway` is generic over **one** adapter type, so a board that wants
/// to accept commands on more than one link registers both behind this enum.
/// Registration order defines the adapter index (0 = UART0, 1 =
/// USB-Serial/JTAG) and `source_kind()` reports a distinct `Rs232` port name per
/// variant, which the ingress loop uses to send each reply back out of the link
/// the command arrived on.
pub enum EspIngressLink {
    /// UART0 — the external USB-UART bridge on the C61 DevKit.
    Uart(UartAdapter<esp_idf_svc::hal::uart::UartDriver<'static>>),
    /// Native USB-Serial/JTAG console — ESP32-S3 dev boards.
    #[cfg(feature = "board-s3")]
    Usb(UsbSerialAdapter),
}

impl ErrorType for EspIngressLink {
    type Error = AdapterError;
}

impl LinkAdapter for EspIngressLink {
    type Error = AdapterError;

    fn poll(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            Self::Uart(a) => a.poll(buf),
            #[cfg(feature = "board-s3")]
            Self::Usb(a) => a.poll(buf),
        }
    }

    fn send(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        match self {
            Self::Uart(a) => a.send(buf),
            #[cfg(feature = "board-s3")]
            Self::Usb(a) => a.send(buf),
        }
    }

    fn source_kind(&self) -> IngressSource {
        match self {
            Self::Uart(a) => a.source_kind(),
            #[cfg(feature = "board-s3")]
            Self::Usb(a) => a.source_kind(),
        }
    }

    fn is_connected(&self) -> bool {
        match self {
            Self::Uart(a) => a.is_connected(),
            #[cfg(feature = "board-s3")]
            Self::Usb(a) => a.is_connected(),
        }
    }
}