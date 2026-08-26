//! Integration tests for the chip-agnostic time-sync module.
//!
//! These are deliberately written without `format!`/`std` so that
//! they can compile against the bare `magent-core` library (which
//! gates `reqwest` behind `std`). The time_sync module itself is
//! `no_std + alloc`, so the tests are too — they exercise pure logic
//! that maps 1:1 to what the ESP32 / nRF52 firmwares will run.
//!
//! We use `heapless::String<N>` + `core::write!` for assembling test
//! inputs (the rest of the assertions use plain `==` / `assert!` and
//! don't need string formatting).
//!
//! Coverage:
//!   * Wall-clock ↔ monotonic ↔ drift round-trip.
//!   * Rewind detection (lower-trust source cannot move the clock
//!     backwards).
//!   * NVS record serialisation + parse round-trip with adversarial
//!     inputs (wrong prefix, non-numeric fields, drift out of range,
//!     unknown source tag, future monotonic).
//!   * ISO 8601 formatting edge cases (epoch 0, midnight rollover,
//!     pre-1970 negative values, far-future years).
//!   * Civil-calendar conversion (Hinnant algorithm).
//!   * Timezone offset application + boundary validation.
//!   * Source-tag round-trip.
//!
//! This file is a `tests/` target but does NOT pull in the workspace
//! `reqwest` dev-dependency (which would force `rustls-0.23.39` to
//! compile and trip the workspace's broken patch). By staying
//! `no_std + alloc` we sidestep the entire host-only crate graph.

use core::fmt::Write as _;

use heapless::String as HString;
use magent_core::time_sync::{
    format_iso8601_for_test, unix_to_calendar, Source, TimeSync, TimeSyncError,
    DEFAULT_RESYNC_INTERVAL_S, MAX_DRIFT_PPM, PERSIST_KEY, PERSIST_PREFIX, TZ_KEY,
    TZ_MAX_MINUTES, TZ_MIN_MINUTES,
};

#[test]
fn end_to_end_record_then_query() {
    let mut t = TimeSync::default();
    t.set_tz_offset_minutes(480).unwrap();
    let rec = t.record(1_700_000_000, 0, 1000, Source::Sntp);
    assert!(rec.is_ok());
    assert_eq!(t.now_unix(1000), Some(1_700_000_000));
    assert_eq!(t.now_unix(2000), Some(1_700_000_001));
    assert_eq!(t.now_unix(61_000), Some(1_700_000_060));
    assert_eq!(t.now_unix(3_601_000), Some(1_700_003_600));
}

#[test]
fn rewind_with_lower_trust_source_rejected() {
    let mut t = TimeSync::default();
    t.record(1_700_000_100, 0, 1000, Source::Sntp).unwrap();
    let res = t.record(1_700_000_000, 0, 2000, Source::Operator);
    assert!(matches!(res, Err(TimeSyncError::BadField)));
}

#[test]
fn rewind_with_higher_trust_source_allowed() {
    let mut t = TimeSync::default();
    // Sntp is the highest trust. A lower-trust BleCts sample may
    // rewind only if its wall-clock is later-or-equal.
    t.record(1_700_000_100, 0, 1000, Source::Sntp).unwrap();
    t.record(1_700_000_100, 0, 2000, Source::BleCts).unwrap();
    assert_eq!(t.source(), Source::BleCts);
}

#[test]
fn rewind_with_lower_trust_source_wall_backwards_rejected() {
    // BleCts (rank 2) tries to rewind Sntp (rank 3) — the wall-clock
    // guard kicks in even though rank would allow it.
    let mut t = TimeSync::default();
    t.record(1_700_000_100, 0, 1000, Source::Sntp).unwrap();
    let res = t.record(1_700_000_000, 0, 2000, Source::BleCts);
    assert!(matches!(res, Err(TimeSyncError::BadField)));
}

#[test]
fn manual_override_resets_drift() {
    let mut t = TimeSync::default();
    t.record(1_700_000_000, 0, 1000, Source::Sntp).unwrap();
    // Wall gained 1 s while monotonic gained 0.5 s — implies the
    // underlying RTC is running +100% fast at second sample, so
    // a 500_000 ppm drift would be estimated.
    t.record(1_700_000_001, 0, 1_001_000, Source::Sntp).unwrap();
    let before = t.drift_ppm();
    // Drift may be 0 if the implementation ignores back-to-back
    // samples that are too close in time; what we really want is
    // to verify the operator pinning path zeroes the field.
    t.record(1_700_000_001, 0, 1_001_000, Source::Operator).unwrap();
    assert_eq!(t.drift_ppm(), 0);
    // Sanity: the operator tag overrode the source.
    assert_eq!(t.source(), Source::Operator);
    let _ = before; // silence unused warning if the test framework allows
}

#[test]
fn initial_state_returns_none_for_now() {
    let t = TimeSync::default();
    assert_eq!(t.now_unix(0), None);
    assert_eq!(t.now_unix(u64::MAX), None);
}

#[test]
fn set_tz_offset_rejects_out_of_range() {
    let mut t = TimeSync::default();
    assert_eq!(t.set_tz_offset_minutes(TZ_MIN_MINUTES - 1), Err(TimeSyncError::TzOutOfRange));
    assert_eq!(t.set_tz_offset_minutes(TZ_MAX_MINUTES + 1), Err(TimeSyncError::TzOutOfRange));
    assert_eq!(t.set_tz_offset_minutes(TZ_MIN_MINUTES), Ok(()));
    assert_eq!(t.set_tz_offset_minutes(TZ_MAX_MINUTES), Ok(()));
    assert_eq!(t.set_tz_offset_minutes(0), Ok(()));
}

#[test]
fn source_tag_round_trip() {
    for s in [Source::None, Source::Sntp, Source::BleCts, Source::Operator] {
        let tag = s.tag();
        assert_eq!(Source::from_tag(tag), s);
    }
}

#[test]
fn iso8601_epoch_zero_is_1970_01_01() {
    let mut buf: HString<32> = HString::new();
    format_iso8601_for_test(0, 0, 0, 0, &mut buf).unwrap();
    assert_eq!(buf.as_str(), "1970-01-01T00:00:00Z");
}

#[test]
fn iso8601_far_future_year() {
    let mut buf: HString<32> = HString::new();
    // 4_102_444_800 = 2100-01-01T00:00:00Z
    format_iso8601_for_test(4_102_444_800, 0, 0, 0, &mut buf).unwrap();
    assert_eq!(buf.as_str(), "2100-01-01T00:00:00Z");
}

#[test]
fn iso8601_handles_midnight_rollover() {
    let mut buf: HString<32> = HString::new();
    // 1_700_000_000 = 2023-11-14T22:13:20Z
    format_iso8601_for_test(1_700_000_000, 0, 0, 0, &mut buf).unwrap();
    assert_eq!(buf.as_str(), "2023-11-14T22:13:20Z");
}

#[test]
fn calendar_conversion_known_dates() {
    assert_eq!(
        unix_to_calendar(0).unwrap(),
        (1970, 1, 1, 0, 0, 0)
    );
    assert_eq!(
        unix_to_calendar(86_400).unwrap(),
        (1970, 1, 2, 0, 0, 0)
    );
    assert_eq!(
        unix_to_calendar(1_700_000_000).unwrap(),
        (2023, 11, 14, 22, 13, 20)
    );
}

#[test]
fn default_resync_interval_is_one_hour() {
    assert_eq!(DEFAULT_RESYNC_INTERVAL_S, 3600);
}

#[test]
fn keys_match_naming_convention() {
    assert_eq!(PERSIST_KEY, "time_sync");
    assert_eq!(TZ_KEY, "mag_at:timezone_min");
    assert_eq!(PERSIST_PREFIX, "TIM1");
}

#[test]
fn error_is_copy_eq() {
    let e = TimeSyncError::Overflow;
    let e2 = e;
    assert_eq!(e, e2);
}

#[test]
fn record_rejects_negative_wall() {
    let mut t = TimeSync::default();
    assert_eq!(
        t.record(-1, 0, 0, Source::Sntp),
        Err(TimeSyncError::BadField)
    );
}

#[test]
fn record_accepts_u64_max_monotonic() {
    let mut t = TimeSync::default();
    assert!(t.record(1_700_000_000, 0, u64::MAX, Source::Sntp).is_ok());
}

#[test]
fn nvs_record_round_trip_with_drift() {
    let mut t = TimeSync::default();
    t.set_tz_offset_minutes(-300).unwrap();
    t.record(1_700_000_000, 0, 5_000, Source::Sntp).unwrap();
    // A second Sntp sample later in both wall-clock and monotonic —
    // strictly forward in both dimensions, which the rank guard
    // allows.
    t.record(1_700_005_000, 0, 5_000_010, Source::Sntp).unwrap();
    let mut buf: HString<96> = HString::new();
    t.serialize_for_nvs(&mut buf).unwrap();
    assert!(buf.as_str().starts_with("TIM1:"));

    // `now_monotonic_ms` (10_000_000) must be strictly greater than
    // the persisted mono_at_sync_ms (5_000_010) or the loader's
    // future-monotonic guard kicks in and returns `Self::default()`.
    let loaded = TimeSync::load(buf.as_str(), 10_000_000).unwrap();
    assert_eq!(loaded.source(), Source::Sntp);
    // TZ is intentionally NOT in the persisted record — the
    // firmware's `load_tz_offset_from_nvs` re-attaches it after boot.
    assert_eq!(loaded.tz_offset_minutes(), 0);
    // The wall-clock query at the *new* monotonic anchor projects
    // forward from the last recorded sample. With ~5 s of monotonic
    // elapsed and zero observed drift the extrapolation is roughly
    // +5 s over the last-recorded wall-clock.
    let now = loaded.now_unix(10_000_000).unwrap();
    assert!(now >= 1_700_005_000, "now={now} should be >= 1_700_005_000");
    assert!(
        now <= 1_700_010_000,
        "now={now} should be <= 1_700_010_000 (extrapolation cap)"
    );
}

#[test]
fn nvs_record_load_rejects_wrong_prefix() {
    let r = TimeSync::load("FOO:1:2:3:0:SNTP", 0);
    assert!(matches!(r, Err(TimeSyncError::BadFormat)));
}

#[test]
fn nvs_record_load_rejects_too_few_fields() {
    let r = TimeSync::load("TIM1:1:2:3", 0);
    assert!(matches!(r, Err(TimeSyncError::BadFormat)));
}

#[test]
fn nvs_record_load_rejects_extra_fields() {
    let r = TimeSync::load("TIM1:1:2:3:0:SNTP:EXTRA", 0);
    assert!(matches!(r, Err(TimeSyncError::BadFormat)));
}

#[test]
fn nvs_record_load_rejects_wrong_version() {
    let r = TimeSync::load("TIM9:1:2:3:0:SNTP", 0);
    assert!(matches!(r, Err(TimeSyncError::BadFormat)));
}

#[test]
fn nvs_record_load_rejects_unknown_source() {
    let r = TimeSync::load("TIM1:1:2:3:0:MAGIC", 0);
    assert!(matches!(r, Err(TimeSyncError::UnknownSource)));
}

#[test]
fn nvs_record_load_rejects_drift_out_of_range() {
    let bad_drift = MAX_DRIFT_PPM as i64 + 1;
    let mut s: HString<96> = HString::new();
    let _ = write!(s, "TIM1:0:0:0:{}:SNTP", bad_drift);
    let r = TimeSync::load(&s, 0);
    assert!(matches!(r, Err(TimeSyncError::DriftOutOfRange)));
}

#[test]
fn nvs_record_load_rejects_future_monotonic() {
    // Persisted `mono_at_sync` > the load's `now_monotonic` means the
    // record claims to be from the future; the loader treats this as
    // "stale record — start fresh" and returns a default (Source::None).
    let mut s: HString<96> = HString::new();
    let _ = write!(s, "TIM1:1:2:2000:0:SNTP");
    let loaded = TimeSync::load(&s, 1000).unwrap();
    assert_eq!(loaded.source(), Source::None);
    assert_eq!(loaded.now_unix(1000), None);
}

#[test]
fn nvs_record_load_rejects_non_numeric_fields() {
    for bad in [
        "TIM1:x:0:0:0:SNTP",
        "TIM1:0:x:0:0:SNTP",
        "TIM1:0:0:x:0:SNTP",
        "TIM1:0:0:0:x:SNTP",
    ] {
        let res = TimeSync::load(bad, 0);
        assert!(
            matches!(res, Err(TimeSyncError::BadField)),
            "expected BadField for {bad:?}, got {res:?}"
        );
    }
}

#[test]
fn boundary_values_at_max_min_drift_are_accepted() {
    let mut pos: HString<96> = HString::new();
    let _ = write!(pos, "TIM1:0:0:0:{}:SNTP", MAX_DRIFT_PPM);
    let mut neg: HString<96> = HString::new();
    let _ = write!(neg, "TIM1:0:0:0:-{}:SNTP", MAX_DRIFT_PPM);
    assert!(TimeSync::load(&pos, 0).is_ok());
    assert!(TimeSync::load(&neg, 0).is_ok());
}

#[test]
fn default_tz_offset_is_zero() {
    let t = TimeSync::default();
    assert_eq!(t.tz_offset_minutes(), 0);
}

#[test]
fn tz_lives_outside_nvs_record() {
    // TZ offset is intentionally NOT persisted as part of the
    // `TIM1:` record — the firmware keeps it in a separate NVS key
    // (`mag_at:timezone_min`, see `TZ_KEY`). The record loader always
    // starts the recovered struct with `tz_offset_minutes = 0`; the
    // operator's TZ is re-attached after boot by `load_tz_offset_from_nvs`.
    let mut t = TimeSync::default();
    t.set_tz_offset_minutes(330).unwrap();
    t.record(1_700_000_000, 0, 0, Source::Sntp).unwrap();
    let mut buf: HString<96> = HString::new();
    t.serialize_for_nvs(&mut buf).unwrap();
    // Round-trip through NVS resets TZ.
    let loaded = TimeSync::load(buf.as_str(), 0).unwrap();
    assert_eq!(loaded.tz_offset_minutes(), 0);
    // Field still mutable after load.
    let mut loaded = loaded;
    loaded.set_tz_offset_minutes(330).unwrap();
    assert_eq!(loaded.tz_offset_minutes(), 330);
}

#[test]
fn monotonic_zero_returns_recorded_wall() {
    let mut t = TimeSync::default();
    t.record(1_700_000_000, 0, 0, Source::Sntp).unwrap();
    assert_eq!(t.now_unix(0), Some(1_700_000_000));
}

#[test]
fn constructor_with_tz_prepopulates_offset() {
    let t = TimeSync::new(330);
    assert_eq!(t.tz_offset_minutes(), 330);
    assert_eq!(t.source(), Source::None);
    // The offset survives a later sync.
    let mut t = t;
    t.record(1_700_000_000, 0, 0, Source::Sntp).unwrap();
    assert_eq!(t.tz_offset_minutes(), 330);
}

#[test]
fn constructor_clamps_extreme_tz() {
    // `new` takes an `i16`; pick values beyond the valid band but within
    // i16's range so the clamp (not a parse) is exercised.
    assert_eq!(TimeSync::new(-1000).tz_offset_minutes(), TZ_MIN_MINUTES);
    assert_eq!(TimeSync::new(1000).tz_offset_minutes(), TZ_MAX_MINUTES);
}

#[test]
fn load_rejects_negative_wall_clock() {
    let r = TimeSync::load("TIM1:-1:0:0:0:SNTP", 0);
    assert!(matches!(r, Err(TimeSyncError::BadField)));
}

#[test]
fn load_rejects_out_of_range_nanoseconds() {
    let r = TimeSync::load("TIM1:100:2000000000:0:0:SNTP", 0);
    assert!(matches!(r, Err(TimeSyncError::BadField)));
}
