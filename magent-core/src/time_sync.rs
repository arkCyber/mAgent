//! Chip-agnostic time-synchronization support for mAgent.
//!
//! This module gives the firmware a single, coherent notion of "now"
//! even though the two target chips have very different clock sources:
//!
//!   * ESP32 (C61) keeps wall-clock time only when it has synced with
//!     an authoritative external source (SNTP / phone-pushed time via
//!     BLE / …). The RTC domain on the C61 is a 32 kHz LP-clock that
//!     does not survive deep sleep; monotonic millisecond time comes
//!     from `esp_timer_get_time`.
//!   * nRF52840 (watch) has a real 32.768 kHz LFCLK-backed RTC that
//!     keeps running through System ON sleep, but it does NOT carry
//!     wall-clock information across reboots — it starts at zero.
//!
//! Both chips therefore have the same model after this module is in
//! place:
//!
//! ```text
//!     wall-clock (Unix epoch seconds, UTC)
//!          ▲
//!          │  correction = wall_at_sync + drift_ppm / 1e6 * elapsed_ms
//!          ▼
//!     monotonic (milliseconds since boot)
//! ```
//!
//! The firmware is responsible for:
//!
//!   1. Getting a `(wall_at_sync, monotonic_at_sync)` sample from
//!      some authoritative source (SNTP, BLE Current Time Service,
//!      operator-typed time, …).
//!   2. Calling [`TimeSync::record`] on it.
//!   3. Calling [`TimeSync::now_unix`] whenever a consumer (AT
//!      dispatcher, web3 sign-and-log, freshness-check, …) needs the
//!      current wall-clock time.
//!
//! Persistence (`NVS_PERSIST_KEY`) is a *plain* NVS string so v0.2
//! doesn't have to depend on the DBO2 sealing machinery. v0.3 will
//! upgrade the record to a signed / sealed bundle so a tampered NVS
//! cannot feed fake wall-clock values into web3 signatures (today's
//! threat model says an attacker with NVS write access can already
//! rotate the device identity, so the time-record authenticity
//! benefit is marginal until we also sign `dev_identity`).
//!
//! # Aerograde guarantees
//!
//! * **No panic, no allocation.** All conversion math is checked; an
//!   arithmetic overflow returns [`TimeSyncError::Overflow`].
//! * **Bounded memory.** Module-level state lives in a single
//!   [`TimeSync`] value the firmware can place in static memory.
//! * **No floating-point.** All drift correction is done in integer
//!   parts-per-million with rounding to avoid the temptation of
//!   pulling in `libm` on the C61's RISC-V core.
//! * **Crash-loop aware.** `now_unix()` returns `None` until the
//!   firmware records at least one authoritative sample, so callers
//!   cannot accidentally publish a wall-clock time that started at
//!   epoch 0.
//!
//! # Wire formats
//!
//! ## NVS persistence
//!
//! ```text
//! TIM1:<wall_unix_s>:<wall_unix_ns>:<mono_ms_at_sync>:<drift_ppm>:<src_tag>
//! ```
//!
//! `TIM1` is the version tag (bump to `TIM2` if the format ever
//! changes). `wall_unix_ns` is the sub-second component (0..1e9) so
//! the recovered wall-clock has full nanosecond resolution instead of
//! being quantised to whole seconds. `src_tag` is the source name
//! (e.g. `"SNTP"`, `"BLE_CTS"`, `"OPERATOR"`); it's purely diagnostic,
//! carried over for audit purposes, and never trusted for security.
//!
//! ## AT surface
//!
//! See `magent_core::at::AtOp::Time` / `NtpSync` / `Timezone`. The
//! `AT+TIME?` reply is documented in `AT+TIME?` handlers in
//! `firmware/esp32-app/src/at_dispatch.rs`.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU32, Ordering};

use heapless::{String, Vec};

/// Maximum length of an NVS-persisted time-sync record.
///
/// Sized for the wire format documented in the module-level docstring
/// (`TIM1:` + 5 fields at ≤10 digits each + drift up to 7 chars incl.
/// sign + source tag ≤16 + colons) with generous margin. Any
/// generation that exceeds this is treated as a corrupt record and
/// discarded.
pub const MAX_RECORD_LEN: usize = 96;

/// Maximum length of the human-readable ISO 8601 timestamp emitted
/// over AT: `YYYY-MM-DDTHH:MM:SSZ` = 20 bytes.
pub const MAX_ISO_LEN: usize = 32;

/// Maximum length of the source-tag string we embed in the NVS
/// record and the audit log line. Longer tags are truncated.
pub const MAX_SRC_TAG_LEN: usize = 16;

/// Identifier prefix for the persisted record. Bump if the wire
/// format ever changes incompatibly.
pub const PERSIST_PREFIX: &str = "TIM1";

/// Default NVS key (in the `magent` namespace) the firmware reads /
/// writes on boot / on `record()` / on every periodic re-sync.
///
/// The `mag_at:` prefix is reserved for AT-side keys; this lives in
/// the same namespace as `dev_identity` so a single
/// `EspDefaultNvsPartition::take()` + `EspDefaultNvs::new()` pair is
/// enough to load everything during boot.
pub const PERSIST_KEY: &str = "time_sync";

/// Default NVS key for the timezone offset (in the `mag_at`
/// namespace, since `AT+TIMEZONE` is the writer). Encoded as the
/// number of minutes east of UTC, in the range `-720..=840` (which
/// covers every current real-world timezone including the
/// Pacific/Kiritimati +14:00 outlier).
pub const TZ_KEY: &str = "mag_at:timezone_min";

/// Lower bound on the timezone offset we'll accept (12 hours west of
/// UTC).
pub const TZ_MIN_MINUTES: i16 = -12 * 60;

/// Upper bound (14 hours east of UTC — Kiritimati at +14).
pub const TZ_MAX_MINUTES: i16 = 14 * 60;

/// Default interval between resync attempts after a successful sync
/// (1 hour, per the design decision). The actual cadence is owned by
/// the platform backend (firmware-side SNTP supervisor / nRF52 CTS
/// subscriber); this constant is here so tests + audits have a single
/// source of truth.
pub const DEFAULT_RESYNC_INTERVAL_S: u64 = 3600;

/// Drift magnitudes above this (parts per million) are rejected as
/// unphysical — a real RTC drifts at most a few hundred ppm; anything
/// beyond that is almost certainly a parsing error or a deliberate
/// poisoning of the persisted record. 10% is a comfortable ceiling.
pub const MAX_DRIFT_PPM: i32 = 100_000;

/// Authoritative time source. Carried in the persisted record so a
/// future audit can see where the wall-clock came from, and so the
/// firmware can refuse to "downgrade" — e.g. once we've synced from
/// SNTP, a subsequent BLE CTS update that claims 25 minutes earlier
/// is treated as the BLE device lying (or us having previously lied).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Source {
    /// SNTP poll succeeded; `server_host` is the server we hit.
    Sntp,
    /// BLE Current Time Service update from the phone (nRF52 only).
    BleCts,
    /// Operator typed the time via `AT+TIME=<epoch>` (manual
    /// override). Trusted only on safe-mode + crash-loop conditions.
    Operator,
    /// No authoritative source yet — the firmware just rebooted and
    /// has never been able to reach SNTP / BLE.
    None,
}

impl Source {
    /// Short ASCII tag for the persisted record / log lines. Length
    /// is bounded by [`MAX_SRC_TAG_LEN`] so a truncated copy is still
    /// recoverable.
    pub const fn tag(self) -> &'static str {
        match self {
            Source::Sntp => "SNTP",
            Source::BleCts => "BLE_CTS",
            Source::Operator => "OPERATOR",
            Source::None => "NONE",
        }
    }

    /// Parse the persisted tag back into a `Source`. Unknown tags
    /// (e.g. a future `Source::HttpDate`) round-trip as `None` so the
    /// firmware refuses to trust an unrecognised source.
    pub fn from_tag(s: &str) -> Self {
        match s {
            "SNTP" => Source::Sntp,
            "BLE_CTS" => Source::BleCts,
            "OPERATOR" => Source::Operator,
            _ => Source::None,
        }
    }
}

/// The persistent time-sync state. One per firmware.
///
/// Holds:
///   * the wall-clock + monotonic sample from the last successful
///     authoritative sync,
///   * the drift in ppm the operator / backend wants to apply
///     (typically zero; a non-zero value lets a deployment tune
///     `now_unix()` to match a known-bad RTC),
///   * the timezone offset in minutes east of UTC (for AT display
///     only — the canonical wall-clock is always UTC).
#[derive(Debug, Clone)]
pub struct TimeSync {
    /// Last authoritative wall-clock sample (Unix seconds, UTC).
    wall_unix_s: i64,
    /// Sub-second component of the wall-clock sample (0..1_000_000_000).
    wall_unix_ns: u32,
    /// Monotonic milliseconds at the moment `wall_unix_s` was sampled.
    mono_at_sync_ms: u64,
    /// Drift applied on top of the elapsed-monotonic calculation,
    /// in ppm. Positive means the wall clock is running *fast*
    /// relative to monotonic.
    drift_ppm: i32,
    /// Source of the last sample.
    source: Source,
    /// Timezone offset in minutes east of UTC. Affects AT display
    /// only; `now_unix()` always returns UTC.
    tz_offset_minutes: i16,
    /// Monotonic counter incremented on every `record()` call. Lets
    /// the firmware detect "the backend re-recorded while we were
    /// still serving the previous sample" without locks.
    generation: u32,
}

impl Default for TimeSync {
    fn default() -> Self {
        Self {
            wall_unix_s: 0,
            wall_unix_ns: 0,
            mono_at_sync_ms: 0,
            drift_ppm: 0,
            source: Source::None,
            tz_offset_minutes: 0,
            generation: 0,
        }
    }
}

/// All the ways the chip-agnostic API can fail. The firmware maps
/// these to `+CMDER:<n>` codes; tests assert on them directly.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TimeSyncError {
    /// Persisted record has the wrong prefix or is too short.
    BadFormat,
    /// A field failed to parse (negative epoch, non-decimal, …).
    BadField,
    /// Source tag is not recognised (record from a newer / older
    /// firmware).
    UnknownSource,
    /// Drift magnitude exceeds [`MAX_DRIFT_PPM`].
    DriftOutOfRange,
    /// Arithmetic overflow in the wall-clock ↔ monotonic conversion.
    Overflow,
    /// Output buffer was too small for the rendered ISO 8601 string.
    /// Callers typically size their buffer to [`MAX_ISO_LEN`].
    OutputOverflow,
    /// Timezone offset is outside [`TZ_MIN_MINUTES`]..=[`TZ_MAX_MINUTES`].
    TzOutOfRange,
}

impl core::fmt::Display for TimeSyncError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            TimeSyncError::BadFormat => "bad_format",
            TimeSyncError::BadField => "bad_field",
            TimeSyncError::UnknownSource => "unknown_source",
            TimeSyncError::DriftOutOfRange => "drift_out_of_range",
            TimeSyncError::Overflow => "overflow",
            TimeSyncError::OutputOverflow => "output_overflow",
            TimeSyncError::TzOutOfRange => "tz_out_of_range",
        };
        f.write_str(s)
    }
}

/// The wire format for `serialize_for_nvs`. Kept as a separate type
/// so the firmware can `mem::forget` the buffer once it's copied into
/// NVS without dragging the whole `TimeSync` along.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedRecord {
    /// The raw record bytes. Lifetime is `'static` because callers
    /// (firmware) typically render into a stack-resident buffer,
    /// hand it to NVS, and discard it.
    bytes: String<MAX_RECORD_LEN>,
}

impl PersistedRecord {
    /// Borrow the rendered bytes for the NVS write.
    pub fn as_str(&self) -> &str {
        self.bytes.as_str()
    }
}

impl TimeSync {
    /// Construct a fresh, never-synced instance with the given
    /// timezone offset. The drift defaults to zero.
    ///
    /// The firmware calls this once at boot with the value persisted by
    /// `AT+TIMEZONE=` (see `at_dispatch::load_tz_offset_from_nvs`), so an
    /// operator's timezone survives reboots. An out-of-range offset is
    /// clamped to the nearest valid bound rather than stored as-is, so a
    /// corrupt NVS value can't leave the handle in a half-built state.
    pub fn new(tz_offset_minutes: i16) -> Self {
        let mut s = Self::default();
        let clamped = tz_offset_minutes.clamp(TZ_MIN_MINUTES, TZ_MAX_MINUTES);
        // `clamp` guarantees the value is in range, so this cannot fail.
        let _ = s.set_tz_offset_minutes(clamped);
        s
    }

    /// Load state from a previously-persisted NVS record. Returns
    /// `Err` on any parse failure (which the firmware treats as
    /// "no prior state — start from zero"). The wall-clock is only
    /// trusted if the record's monotonic timestamp is `≤` the
    /// current monotonic — otherwise the firmware rebooted with a
    /// stale value and we discard the record.
    ///
    /// `now_monotonic_ms` is the value of `esp_timer_get_time()` /
    /// the equivalent chip-local monotonic clock at the moment of
    /// boot. The firmware must pass the same source for both
    /// `record()` and `now_unix()`.
    pub fn load(record: &str, now_monotonic_ms: u64) -> Result<Self, TimeSyncError> {
        let parsed = parse_record(record)?;
        if parsed.mono_at_sync_ms > now_monotonic_ms {
            // The persisted record claims to have been sampled in
            // the future relative to the current monotonic clock —
            // either the RTC was rewound (deep-sleep on the nRF52)
            // or the record is bogus. Start fresh.
            return Ok(Self {
                tz_offset_minutes: parsed.tz_offset_minutes,
                ..Self::default()
            });
        }
        Ok(parsed)
    }

    /// Record an authoritative sample. Subsequent calls to
    /// [`Self::now_unix`] return values monotonically increasing in
    /// wall-clock terms (modulo the operator-supplied drift), so a
    /// later SNTP update that claims to be *earlier* than the
    /// previous one (e.g. an SNTP server returning a stale response
    /// after a long retry) does NOT rewind the local clock.
    ///
    /// Returns `Err` on arithmetic overflow (e.g. epoch > year 9999
    /// with a 200 ppm drift correction applied).
    pub fn record(
        &mut self,
        wall_unix_s: i64,
        wall_unix_ns: u32,
        monotonic_ms: u64,
        source: Source,
    ) -> Result<(), TimeSyncError> {
        if wall_unix_ns >= 1_000_000_000 {
            return Err(TimeSyncError::BadField);
        }
        // Reject obviously bogus wall-clock values. The Unix-epoch
        // bottom of 0 is fine (1970-01-01) and is what we'd see on
        // a freshly-flashed device. Anything in the past 50 years
        // (1975) is fishy enough to warn about — but a *negative*
        // wall value would corrupt the monotonic arithmetic.
        if wall_unix_s < 0 {
            return Err(TimeSyncError::BadField);
        }
        // Monotonic-back-time travel: only record if the new sample
        // is strictly later than the previous one (in wall-clock
        // terms), OR if the source is strictly more authoritative.
        // We treat SNTP > BLE CTS > Operator > None as the trust
        // order, so:
        //   * a lower-trust sample cannot rewind a more-trusted one,
        //   * a same-trust sample cannot rewind at all (two
        //     operators can't agree to step backwards; only a higher
        //     trust "reset" is allowed, e.g. Operator → Sntp after
        //     the operator manually lost GPS lock).
        if self.source != Source::None && source.rank() <= self.source.rank() {
            let prev_wall = self.wall_unix_s as i128;
            let new_wall = wall_unix_s as i128;
            if new_wall < prev_wall {
                log::warn!(
                    "[time_sync] refusing rewind from {:?}@{} to {:?}@{} (not strictly more authoritative)",
                    self.source, self.wall_unix_s, source, wall_unix_s,
                );
                return Err(TimeSyncError::BadField);
            }
        }
        self.wall_unix_s = wall_unix_s;
        self.wall_unix_ns = wall_unix_ns;
        self.mono_at_sync_ms = monotonic_ms;
        self.source = source;
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    /// Wall-clock seconds since Unix epoch, UTC, at `monotonic_ms`.
    ///
    /// Returns `None` if no authoritative sample has ever been
    /// recorded (the firmware started up with no NVS record and
    /// couldn't reach any time source yet).
    pub fn now_unix(&self, monotonic_ms: u64) -> Option<i64> {
        if self.source == Source::None {
            return None;
        }
        let elapsed_ms = monotonic_ms.checked_sub(self.mono_at_sync_ms)?;
        apply_drift(self.wall_unix_s, elapsed_ms, self.drift_ppm).ok()
    }

    /// Same as [`Self::now_unix`] but also returns the sub-second
    /// component (0..1_000_000_000). Useful for the ISO 8601 renderer.
    pub fn now_unix_with_ns(&self, monotonic_ms: u64) -> Option<(i64, u32)> {
        let s = self.now_unix(monotonic_ms)?;
        // Sub-second component comes from the wall-clock sample
        // itself plus the monotonic's sub-second contribution
        // (modulo drift, which we ignore at the ns scale — ppm
        // drift over an hour is at most 360 ns).
        let elapsed_ms = monotonic_ms.checked_sub(self.mono_at_sync_ms)?;
        let extra_ns = ((elapsed_ms % 1000) as u32).saturating_mul(1_000_000);
        let ns = self.wall_unix_ns.saturating_add(extra_ns);
        let carry = ns / 1_000_000_000;
        let ns = ns % 1_000_000_000;
        let s = s.checked_add(carry as i64)?;
        Some((s, ns))
    }

    /// Render the current wall-clock as `YYYY-MM-DDTHH:MM:SSZ` into
    /// `out`. `monotonic_ms` is the chip-local monotonic clock.
    ///
    /// Returns [`TimeSyncError::OutputOverflow`] if `out` is shorter
    /// than [`MAX_ISO_LEN`], and [`TimeSyncError::Overflow`] if the
    /// wall-clock is large enough that the year / month / day
    /// arithmetic no longer fits in the i32 ranges we use.
    pub fn format_iso8601(
        &self,
        monotonic_ms: u64,
        out: &mut String<MAX_ISO_LEN>,
    ) -> Result<(), TimeSyncError> {
        let (s, ns) = self.now_unix_with_ns(monotonic_ms).ok_or(TimeSyncError::Overflow)?;
        let (y, mo, d, h, mi, sec) = unix_to_calendar(s).ok_or(TimeSyncError::Overflow)?;
        out.clear();
        // `YYYY-MM-DDTHH:MM:SSZ` — 20 chars; MAX_ISO_LEN is 32.
        write!(
            out,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            y, mo, d, h, mi, sec
        )
        .map_err(|_| TimeSyncError::OutputOverflow)?;
        // We deliberately omit the sub-second component (`.{:09}Z`)
        // because the AT reply is documented to end in `Z`; consumers
        // that want ns can use the `wall_unix_s + wall_unix_ns` pair
        // returned by `serialize_for_nvs`. We still record the
        // current `ns` for callers that DO want it.
        let _ = ns; // suppress unused warning
        Ok(())
    }

    /// Timezone offset in minutes east of UTC. Negative for west.
    pub fn tz_offset_minutes(&self) -> i16 {
        self.tz_offset_minutes
    }

    /// Configure the timezone offset. Validates against the
    /// [`TZ_MIN_MINUTES`]..=[`TZ_MAX_MINUTES`] band and rejects
    /// out-of-range values.
    pub fn set_tz_offset_minutes(&mut self, minutes: i16) -> Result<(), TimeSyncError> {
        if !(TZ_MIN_MINUTES..=TZ_MAX_MINUTES).contains(&minutes) {
            return Err(TimeSyncError::TzOutOfRange);
        }
        self.tz_offset_minutes = minutes;
        Ok(())
    }

    /// Drift currently applied to wall-clock ↔ monotonic conversion.
    pub fn drift_ppm(&self) -> i32 {
        self.drift_ppm
    }

    /// Set the drift. Out-of-range values are rejected.
    pub fn set_drift_ppm(&mut self, drift: i32) -> Result<(), TimeSyncError> {
        if drift.abs() > MAX_DRIFT_PPM {
            return Err(TimeSyncError::DriftOutOfRange);
        }
        self.drift_ppm = drift;
        Ok(())
    }

    /// Source of the last recorded sample.
    pub fn source(&self) -> Source {
        self.source
    }

    /// Number of successful `record()` calls since construction. The
    /// counter wraps at `u32::MAX`; we only use it for tests, not for
    /// correctness.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Render the current state as the NVS-persisted wire form.
    /// The output buffer must be at least [`MAX_RECORD_LEN`] bytes.
    pub fn serialize_for_nvs(&self, out: &mut String<MAX_RECORD_LEN>) -> Result<(), TimeSyncError> {
        out.clear();
        write!(
            out,
            "{}:{}:{}:{}:{}:{}",
            PERSIST_PREFIX,
            self.wall_unix_s,
            self.wall_unix_ns,
            self.mono_at_sync_ms,
            self.drift_ppm,
            self.source.tag(),
        )
        .map_err(|_| TimeSyncError::OutputOverflow)?;
        Ok(())
    }
}

/// Trust rank for rewind detection. Higher = more trustworthy.
impl Source {
    fn rank(self) -> u8 {
        match self {
            Source::None => 0,
            Source::Operator => 1,
            Source::BleCts => 2,
            Source::Sntp => 3,
        }
    }
}

/// Apply drift correction to a wall-clock sample.
///
/// Drift in ppm means `1_000_000` monotonic milliseconds offset the
/// wall-clock by `drift_ppm` milliseconds. We compute
/// `(elapsed_ms * drift_ppm) / 1_000_000` with explicit rounding
/// toward zero (matches POSIX `adjtimex` semantics well enough for
/// the 200-ppm regime we operate in).
fn apply_drift(wall_s: i64, elapsed_ms: u64, drift_ppm: i32) -> Result<i64, TimeSyncError> {
    let elapsed_s = (elapsed_ms / 1000) as i64;
    let raw = wall_s.checked_add(elapsed_s).ok_or(TimeSyncError::Overflow)?;
    if drift_ppm == 0 {
        return Ok(raw);
    }
    // Correction in seconds = (elapsed_ms * drift_ppm) / (1000 * 1_000_000)
    // We split it so the intermediate fits comfortably in i64 for any
    // realistic (drift, elapsed) pair (u64::MAX ms * 200 ppm < i64::MAX).
    let elapsed_ms_signed = if elapsed_ms > i64::MAX as u64 {
        return Err(TimeSyncError::Overflow);
    } else {
        elapsed_ms as i64
    };
    let correction_ms = elapsed_ms_signed
        .checked_mul(drift_ppm as i64)
        .ok_or(TimeSyncError::Overflow)?
        / 1_000_000;
    let correction_s = correction_ms / 1000;
    raw.checked_add(correction_s).ok_or(TimeSyncError::Overflow)
}

/// Parse a `TIM1:...` NVS record into a `TimeSync`. Used by both
/// `load()` and the unit tests; the source / drift / timezone fields
/// are all exposed so tests can fuzz individual slots.
fn parse_record(record: &str) -> Result<TimeSync, TimeSyncError> {
    let mut parts = record.split(':');
    let prefix = parts.next().ok_or(TimeSyncError::BadFormat)?;
    if prefix != PERSIST_PREFIX {
        return Err(TimeSyncError::BadFormat);
    }
    let wall_s_str = parts.next().ok_or(TimeSyncError::BadFormat)?;
    let wall_ns_str = parts.next().ok_or(TimeSyncError::BadFormat)?;
    let mono_str = parts.next().ok_or(TimeSyncError::BadFormat)?;
    let drift_str = parts.next().ok_or(TimeSyncError::BadFormat)?;
    let src_str = parts.next().ok_or(TimeSyncError::BadFormat)?;
    if parts.next().is_some() {
        return Err(TimeSyncError::BadFormat);
    }
    let wall_unix_s: i64 = wall_s_str.parse().map_err(|_| TimeSyncError::BadField)?;
    let wall_unix_ns: u32 = wall_ns_str.parse().map_err(|_| TimeSyncError::BadField)?;
    let mono_at_sync_ms: u64 = mono_str.parse().map_err(|_| TimeSyncError::BadField)?;
    let drift_ppm: i32 = drift_str.parse().map_err(|_| TimeSyncError::BadField)?;
    // Enforce the same invariants `record()` does, so a poisoned NVS
    // record cannot smuggle a negative wall-clock or an out-of-range
    // sub-second component past the loader.
    if wall_unix_s < 0 {
        return Err(TimeSyncError::BadField);
    }
    if wall_unix_ns >= 1_000_000_000 {
        return Err(TimeSyncError::BadField);
    }
    if drift_ppm.abs() > MAX_DRIFT_PPM {
        return Err(TimeSyncError::DriftOutOfRange);
    }
    let source = Source::from_tag(src_str);
    if source == Source::None && src_str != "NONE" {
        return Err(TimeSyncError::UnknownSource);
    }
    Ok(TimeSync {
        wall_unix_s,
        wall_unix_ns,
        mono_at_sync_ms,
        drift_ppm,
        source,
        tz_offset_minutes: 0,
        generation: 0,
    })
}

/// Convert Unix seconds (UTC) to `(year, month, day, hour, min, sec)`.
///
/// Uses Howard Hinnant's `days_from_civil` algorithm — a closed-form
/// inverse of the proleptic Gregorian calendar that fits in i64 and
/// covers years 1..=9999 without overflow. Sub-second resolution is
/// not relevant here.
///
/// Returns `None` for out-of-range inputs (year outside 1970..=9999
/// after drift correction).
pub fn unix_to_calendar(unix_s: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    if !(-62_167_219_200..=253_402_300_799).contains(&unix_s) {
        // Out of representable range for the algorithm below
        // (years 0001-01-01..=9999-12-31).
        return None;
    }
    let days = unix_s.div_euclid(86_400);
    let secs_of_day = unix_s.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    // Hinnant days_from_civil, inverted to civil_from_days:
    //   z = days + 719468
    //   era = (z >= 0 ? z : z - 146_096) / 146_097
    //   doe = z - era * 146_097                    [0..146_096]
    //   yoe = (doe - doe/1_460 + doe/36_524 - doe/146_096) / 365   [0..399]
    //   y = yoe + era * 400
    //   doy = doe - (365*yoe + yoe/4 - yoe/100)     [0..365]
    //   mp  = (5*doy + 2) / 153                    [0..9]
    //   d = doy - (153*mp + 2)/5 + 1               [1..31]
    //   m = mp + (mp < 10 ? 3 : -9)                [1..12]
    //   y = y + (m <= 2 ? 1 : 0)
    let z = days + 719_468;
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe = (z - era * 146_097) as u64; // [0..146_096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y_long = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    // Hinnant's `mp` lands in `[0..=9]`. We need `m` in `[1..=12]`:
    //   * `mp ∈ [0..=9]` → month = `mp + 3`  ⇒ Jan=3, …, Oct=12, Nov=1+12=13 → bad
    // The classic Hinnant trick is `m_raw = mp + (mp < 10 ? 3 : -9)`,
    // which is mixed-sign arithmetic. Doing it in `i64` (so `mp`
    // fits trivially — its u64 range is `[0..=9]` so signed cast is
    // safe) keeps the math panic-free even if a future refactor
    // widens `doy`.
    let m_raw: i64 = (mp as i64) + if mp < 10 { 3 } else { -9 };
    let y = y_long + if m_raw <= 2 { 1 } else { 0 };
    let month = m_raw as u32;
    let year = y as i32;
    Some((year, month, d, hour, min, sec))
}

/// Counter incremented on every successful [`TimeSync::record`]
/// across all instances. Lets tests assert that a particular sequence
/// of operations actually exercised the recording path without
/// holding a global mutex around the type.
pub static RECORD_COUNT: AtomicU32 = AtomicU32::new(0);

/// Convenience wrapper used by tests: increments `RECORD_COUNT` on a
/// successful record. Production firmware uses `record()` directly
/// because the counter would just track calls, not interesting
/// events.
pub fn record_with_counter(
    state: &mut TimeSync,
    wall_unix_s: i64,
    wall_unix_ns: u32,
    monotonic_ms: u64,
    source: Source,
) -> Result<(), TimeSyncError> {
    state.record(wall_unix_s, wall_unix_ns, monotonic_ms, source)?;
    RECORD_COUNT.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Re-export the namespace-prefixed TZ NVS key for firmware use.
pub const TZ_PERSIST_KEY: &str = TZ_KEY;

/// Default NVS key for the persisted record (firmware reads it on
/// boot, writes on every `record()`).
pub const DEFAULT_PERSIST_KEY: &str = PERSIST_KEY;

/// Convert a unix-seconds value into the wire-format source tag for
/// audit logging. Inverse of [`Source::from_tag`].
pub fn source_tag(source: Source) -> &'static str {
    source.tag()
}

/// Compute the wall-clock display string for AT (`+TIME:` line)
/// without owning a `TimeSync`. Used by the test harness; production
/// AT dispatchers go through `TimeSync::format_iso8601`.
pub fn format_iso8601_for_test(
    wall_unix_s: i64,
    wall_unix_ns: u32,
    drift_ppm: i32,
    monotonic_ms: u64,
    out: &mut String<MAX_ISO_LEN>,
) -> Result<(), TimeSyncError> {
    let elapsed_ms = monotonic_ms;
    let corrected = apply_drift(wall_unix_s, elapsed_ms, drift_ppm)?;
    let elapsed_ms_signed = if elapsed_ms > i64::MAX as u64 {
        return Err(TimeSyncError::Overflow);
    } else {
        elapsed_ms as i64
    };
    let correction_ms = elapsed_ms_signed
        .checked_mul(drift_ppm as i64)
        .ok_or(TimeSyncError::Overflow)?
        / 1_000_000;
    let extra_ns = ((elapsed_ms % 1000) as u32).saturating_mul(1_000_000);
    let ns = wall_unix_ns.saturating_add(extra_ns);
    let carry = ns / 1_000_000_000;
    let ns = ns % 1_000_000_000;
    let s = corrected.checked_add(carry as i64).ok_or(TimeSyncError::Overflow)?;
    let (y, mo, d, h, mi, sec) = unix_to_calendar(s).ok_or(TimeSyncError::Overflow)?;
    out.clear();
    write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, mo, d, h, mi, sec
    )
    .map_err(|_| TimeSyncError::OutputOverflow)?;
    // Carry the unused ns back to the caller via the unused-warning
    // trick — keeps the compiler from complaining about the extra
    // arithmetic when `drift_ppm == 0`.
    let _ = (ns, correction_ms);
    Ok(())
}

/// Convert a UTC hour/min/sec triple to a local-time triple given a
/// UTC offset in minutes east. Used by the test harness; firmware
/// directly displays ISO 8601 UTC and lets the operator-submitted
/// `AT+TIMEZONE` only shift the human-readable "local" column if it
/// decides to render one (we don't today).
pub fn apply_tz_offset(secs_of_day: u32, offset_minutes: i16) -> u32 {
    let total = secs_of_day as i32 + (offset_minutes as i32) * 60;
    let normalized = ((total % 86_400) + 86_400) % 86_400;
    normalized as u32
}

/// Build an `&[u8]` of a constant for storage in a `Vec<u8, N>` test
/// buffer.
pub fn empty_persisted_record() -> Vec<u8, MAX_RECORD_LEN> {
    let mut v: Vec<u8, MAX_RECORD_LEN> = Vec::new();
    let _ = v.extend_from_slice(PERSIST_PREFIX.as_bytes());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_at(state: &TimeSync, ms: u64) -> Option<i64> {
        state.now_unix(ms)
    }

    #[test]
    fn record_then_now_returns_baseline() {
        let mut t = TimeSync::default();
        t.record(1_700_000_000, 0, 1000, Source::Sntp).unwrap();
        assert_eq!(ts_at(&t, 1000), Some(1_700_000_000));
        assert_eq!(ts_at(&t, 2000), Some(1_700_000_001));
        assert_eq!(ts_at(&t, 60_000), Some(1_700_000_059));
        assert_eq!(ts_at(&t, 3661_000), Some(1_700_003_660));
    }

    #[test]
    fn no_record_yields_none() {
        let t = TimeSync::default();
        assert_eq!(t.now_unix(0), None);
        assert_eq!(t.now_unix(1_000_000), None);
    }

    #[test]
    fn drift_correction_adds_milliseconds() {
        let mut t = TimeSync::default();
        // +200 ppm means after 1000s of monotonic time, the wall
        // clock should be 200 ms ahead. With 1000-ms elapsed and
        // 200 ppm, correction = 1000 * 200 / 1e6 = 0 ms — wait,
        // drift over 1 s is 200 us, so we'd need to wait 1000 s for
        // a 200 ms correction. Use 1_000_000 ms = 1000 s.
        t.record(1_700_000_000, 0, 0, Source::Sntp).unwrap();
        t.set_drift_ppm(200).unwrap();
        assert_eq!(ts_at(&t, 0), Some(1_700_000_000));
        // 1e6 ms × 200 ppm = 200 ms drift → floor to whole seconds = 0.
        // now_unix returns whole seconds so the sub-second drift
        // doesn't show in the returned i64 (we still advance by the
        // 1000 s of elapsed monotonic time, regardless of drift).
        assert_eq!(ts_at(&t, 1_000_000), Some(1_700_001_000));
        // 1e9 ms × 200 ppm = 200 s of drift on top of the 1e9 ms
        // elapsed monotonic time. 1e9 ms = 1_000_000 s.
        assert_eq!(ts_at(&t, 1_000_000_000), Some(1_701_000_200));
    }

    #[test]
    fn negative_drift_subtracts() {
        let mut t = TimeSync::default();
        t.record(1_700_000_000, 0, 0, Source::Sntp).unwrap();
        t.set_drift_ppm(-200).unwrap();
        // 1e9 ms × -200 ppm = -200 s of drift.
        assert_eq!(ts_at(&t, 1_000_000_000), Some(1_700_999_800));
    }

    #[test]
    fn drift_magnitude_too_large_rejected() {
        let mut t = TimeSync::default();
        assert!(matches!(
            t.set_drift_ppm(MAX_DRIFT_PPM + 1),
            Err(TimeSyncError::DriftOutOfRange)
        ));
        assert!(matches!(
            t.set_drift_ppm(-(MAX_DRIFT_PPM + 1)),
            Err(TimeSyncError::DriftOutOfRange)
        ));
        // Boundary values are accepted.
        t.set_drift_ppm(MAX_DRIFT_PPM).unwrap();
        t.set_drift_ppm(-MAX_DRIFT_PPM).unwrap();
    }

    #[test]
    fn rewind_from_lower_trust_source_rejected() {
        let mut t = TimeSync::default();
        t.record(1_700_000_100, 0, 1000, Source::Sntp).unwrap();
        // Operator cannot move us backwards.
        let res = t.record(1_700_000_000, 0, 2000, Source::Operator);
        assert!(matches!(res, Err(TimeSyncError::BadField)));
        // Same-rank source with later-or-equal wall-clock is allowed.
        t.record(1_700_000_100, 0, 3000, Source::Sntp).unwrap();
        // BleCts (rank 2) may move forward to a later wall-clock.
        t.record(1_700_000_100, 0, 4000, Source::BleCts).unwrap();
        // But a BleCts trying to move us backwards past an earlier
        // Sntp is rejected.
        let res = t.record(1_700_000_000, 0, 5000, Source::BleCts);
        assert!(matches!(res, Err(TimeSyncError::BadField)));
    }

    #[test]
    fn negative_wall_rejected() {
        let mut t = TimeSync::default();
        assert!(matches!(
            t.record(-1, 0, 0, Source::Sntp),
            Err(TimeSyncError::BadField)
        ));
    }

    #[test]
    fn nanoseconds_out_of_range_rejected() {
        let mut t = TimeSync::default();
        assert!(matches!(
            t.record(1_700_000_000, 1_000_000_000, 0, Source::Sntp),
            Err(TimeSyncError::BadField)
        ));
    }

    #[test]
    fn serialize_then_load_round_trip() {
        let mut t = TimeSync::default();
        t.set_tz_offset_minutes(480).unwrap();
        t.set_drift_ppm(123).unwrap();
        t.record(1_700_000_000, 123_456_789, 42_000, Source::Sntp)
            .unwrap();
        let mut buf = String::new();
        t.serialize_for_nvs(&mut buf).unwrap();
        let recovered = TimeSync::load(buf.as_str(), 42_000).unwrap();
        // The wire record carries wall + nano + mono + drift + source;
        // TZ lives in a separate NVS key (see `TZ_KEY`) and so is
        // not part of the round-trip.
        assert_eq!(buf.as_str().contains("1700000000"), true,
            "serialised buf missing wall-clock field: {buf}");
        assert_eq!(recovered.source(), Source::Sntp);
        // now_unix uses the *recovered* monotonic-anchored value.
        assert_eq!(recovered.now_unix(42_000), Some(1_700_000_000));
        assert_eq!(recovered.now_unix(43_000), Some(1_700_000_001));
    }

    #[test]
    fn load_rejects_wrong_prefix() {
        let r = TimeSync::load("NOPE:1:2:3:4:5", 1000);
        assert!(matches!(r, Err(TimeSyncError::BadFormat)));
    }

    #[test]
    fn load_rejects_unknown_source() {
        let r = TimeSync::load("TIM1:1:2:3:4:HTTP", 1000);
        assert!(matches!(r, Err(TimeSyncError::UnknownSource)));
    }

    #[test]
    fn load_rejects_drift_out_of_range() {
        let mut s: heapless::String<96> = heapless::String::new();
        let _ = core::write!(
            s,
            "TIM1:1:2:3:{}:NONE",
            MAX_DRIFT_PPM + 1
        );
        let r = TimeSync::load(&s, 1000);
        assert!(matches!(r, Err(TimeSyncError::DriftOutOfRange)));
    }

    #[test]
    fn load_rejects_future_monotonic() {
        // Persisted record claims mono_at_sync=2_000 but current
        // monotonic is 1_000 → discard, fall back to default.
        let loaded = TimeSync::load("TIM1:1:2:2000:0:SNTP", 1000).unwrap();
        assert_eq!(loaded.source(), Source::None);
        assert_eq!(loaded.now_unix(1000), None);
    }

    #[test]
    fn load_rejects_non_numeric_fields() {
        for bad in [
            "TIM1:x:0:0:0:SNTP",
            "TIM1:0:x:0:0:SNTP",
            "TIM1:0:0:x:0:SNTP",
            "TIM1:0:0:0:x:SNTP",
        ] {
            let res = TimeSync::load(bad, 1000);
            assert!(
                matches!(res, Err(TimeSyncError::BadField)),
                "expected BadField for {bad:?}, got {res:?}"
            );
        }
    }

    #[test]
    fn iso8601_format_basic() {
        let mut t = TimeSync::default();
        // 2025-08-24T08:00:00Z = 1_756_022_400
        t.record(1_756_022_400, 0, 0, Source::Sntp).unwrap();
        let mut out = String::new();
        t.format_iso8601(0, &mut out).unwrap();
        assert_eq!(out.as_str(), "2025-08-24T08:00:00Z");
    }

    #[test]
    fn iso8601_format_rolls_over_to_next_day() {
        let mut t = TimeSync::default();
        // Recorded at 2025-08-24T08:00:00Z, query 25 hours later
        // -> 2025-08-25T09:00:00Z.
        t.record(1_756_022_400, 0, 0, Source::Sntp).unwrap();
        let mut out = String::new();
        t.format_iso8601(25 * 3600 * 1000, &mut out).unwrap();
        assert_eq!(out.as_str(), "2025-08-25T09:00:00Z");
    }

    #[test]
    fn iso8601_format_at_epoch() {
        let t = TimeSync::default();
        let mut out = String::new();
        let res = t.format_iso8601(0, &mut out);
        // Never recorded → can't render.
        assert!(res.is_err());
    }

    #[test]
    fn calendar_epoch_is_1970_01_01() {
        let (y, mo, d, h, mi, s) = unix_to_calendar(0).unwrap();
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn calendar_y2038_is_2038_01_19() {
        // 2_147_483_647 = 2038-01-19T03:14:07Z (classic Y2038 upper bound).
        let (y, mo, d, h, mi, s) = unix_to_calendar(2_147_483_647).unwrap();
        assert_eq!((y, mo, d, h, mi, s), (2038, 1, 19, 3, 14, 7));
    }

    #[test]
    fn calendar_y2106_doesnt_overflow() {
        // A real mAgent may live past Y2038; spot-check.
        let (y, mo, d, h, mi, s) = unix_to_calendar(4_102_444_800).unwrap();
        assert_eq!((y, mo, d, h, mi, s), (2100, 1, 1, 0, 0, 0));
    }

    #[test]
    fn calendar_far_past_returns_none() {
        // Below the algorithm's `0001-01-01..=9999-12-31` window.
        assert!(unix_to_calendar(-100_000_000_000).is_none());
    }

    #[test]
    fn calendar_negative_input_still_valid_year_in_range() {
        // -1 second is just before 1970-01-01T00:00:00Z; the API
        // supports years 0001..=9999, so this maps cleanly.
        let (y, mo, d, h, mi, s) = unix_to_calendar(-1).unwrap();
        // 1969-12-31T23:59:59Z.
        assert_eq!((y, mo, d, h, mi, s), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn timezone_offset_validates_bounds() {
        let mut t = TimeSync::default();
        assert!(t.set_tz_offset_minutes(TZ_MIN_MINUTES).is_ok());
        assert!(t.set_tz_offset_minutes(TZ_MAX_MINUTES).is_ok());
        assert!(t
            .set_tz_offset_minutes(TZ_MIN_MINUTES - 1)
            .is_err());
        assert!(t
            .set_tz_offset_minutes(TZ_MAX_MINUTES + 1)
            .is_err());
    }

    #[test]
    fn apply_tz_offset_basic() {
        // 12:00 UTC + 8h = 20:00
        let secs = 12 * 3600;
        assert_eq!(apply_tz_offset(secs, 480), 20 * 3600);
        // 23:00 UTC - 5h = 18:00 (same day)
        assert_eq!(apply_tz_offset(23 * 3600, -300), 18 * 3600);
        // 02:00 UTC + 5h = 07:00
        assert_eq!(apply_tz_offset(2 * 3600, 300), 7 * 3600);
        // 02:00 UTC - 5h wraps to 21:00 previous day.
        assert_eq!(apply_tz_offset(2 * 3600, -300), 21 * 3600);
    }

    #[test]
    fn rewind_only_rejected_when_strictly_earlier() {
        let mut t = TimeSync::default();
        t.record(1_700_000_100, 0, 1000, Source::Sntp).unwrap();
        // Same value, higher monotonic → allowed.
        t.record(1_700_000_100, 0, 2000, Source::Sntp).unwrap();
        // Operator trying to set exact same value → rewind check sees
        // 100 == 100 → allowed.
        t.record(1_700_000_100, 0, 3000, Source::Operator).unwrap();
    }

    #[test]
    fn record_with_counter_increments() {
        let before = RECORD_COUNT.load(Ordering::Relaxed);
        let mut t = TimeSync::default();
        record_with_counter(&mut t, 1, 0, 0, Source::Sntp).unwrap();
        let after = RECORD_COUNT.load(Ordering::Relaxed);
        assert_eq!(after, before + 1);
    }

    #[test]
    fn empty_persisted_record_starts_with_prefix() {
        let v = empty_persisted_record();
        assert_eq!(&v[..4], b"TIM1");
    }

    #[test]
    fn record_bumps_generation() {
        let mut t = TimeSync::default();
        assert_eq!(t.generation(), 0);
        t.record(1, 0, 0, Source::Sntp).unwrap();
        assert_eq!(t.generation(), 1);
        t.record(2, 0, 1, Source::Sntp).unwrap();
        assert_eq!(t.generation(), 2);
    }

    #[test]
    fn source_tag_round_trips() {
        for src in [Source::Sntp, Source::BleCts, Source::Operator, Source::None] {
            assert_eq!(Source::from_tag(src.tag()), src);
        }
    }

    #[test]
    fn now_unix_with_ns_carries_subsecond() {
        let mut t = TimeSync::default();
        t.record(1_700_000_000, 500_000_000, 0, Source::Sntp).unwrap();
        let (s, ns) = t.now_unix_with_ns(0).unwrap();
        assert_eq!(s, 1_700_000_000);
        assert_eq!(ns, 500_000_000);
        // After 1500 ms of monotonic, sub-second addition is
        // 500_000_000 + (500 ms × 1_000_000 ns/ms) = 1_000_000_000,
        // which carries one second. The whole-seconds component
        // gains a further 1 s for the 1500 ms elapsed.
        let (s, ns) = t.now_unix_with_ns(1500).unwrap();
        assert_eq!(s, 1_700_000_002);
        assert_eq!(ns, 0);
    }

    #[test]
    fn new_constructor_sets_tz_offset() {
        let t = TimeSync::new(480);
        assert_eq!(t.tz_offset_minutes(), 480);
        // A freshly constructed handle has never synced.
        assert_eq!(t.source(), Source::None);
        assert_eq!(t.now_unix(0), None);
    }

    #[test]
    fn new_constructor_clamps_out_of_range_tz() {
        assert_eq!(
            TimeSync::new(TZ_MIN_MINUTES - 100).tz_offset_minutes(),
            TZ_MIN_MINUTES
        );
        assert_eq!(
            TimeSync::new(TZ_MAX_MINUTES + 100).tz_offset_minutes(),
            TZ_MAX_MINUTES
        );
        assert_eq!(TimeSync::new(0).tz_offset_minutes(), 0);
    }

    #[test]
    fn load_rejects_negative_wall_clock() {
        let r = TimeSync::load("TIM1:-1:0:0:0:SNTP", 0);
        assert!(matches!(r, Err(TimeSyncError::BadField)));
    }

    #[test]
    fn load_rejects_out_of_range_nanoseconds() {
        // ns = 2_000_000_000 ≥ 1e9 — `record()` would reject it, so a
        // poisoned NVS record must not smuggle it past the loader.
        let r = TimeSync::load("TIM1:100:2000000000:0:0:SNTP", 0);
        assert!(matches!(r, Err(TimeSyncError::BadField)));
    }
}
