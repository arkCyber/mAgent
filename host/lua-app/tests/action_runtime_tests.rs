//! Tests for the action grammar / dispatcher and the supervised `AppRuntime`.
//!
//! Original tests (C1-C27 from AUDIT.md) plus new coverage for the enhanced
//! action set (GPIO_WRITE, LED_SET, BUZZER, POWER_SET, BLE_SEND bounds).

#![cfg(feature = "mlua")]

use std::sync::{Arc, Mutex};

use magent_core::MiniAgent;
use magent_lua::action::{apply_action, Action};
use magent_lua::hardware::SimHardware;
use magent_lua::runtime::AppRuntime;
use magent_lua::{HardwareBackend, LuaHostError, LuaVm, SharedHardware};

fn new_runtime() -> (AppRuntime, SharedHardware) {
    let hardware: SharedHardware =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    // TRACE: REQ-LUA-SANDBOX — single-threaded VM design; see examples/demo.rs.
    #[allow(clippy::arc_with_non_send_sync)]
    let agent: Arc<Mutex<MiniAgent>> = Arc::new(Mutex::new(MiniAgent::with_defaults().unwrap()));
    let vm = LuaVm::new(hardware.clone(), agent).unwrap();
    let app = AppRuntime::new(vm, hardware.clone());
    (app, hardware)
}

// ---------------------------------------------------------------------------
// Action grammar (original)
// ---------------------------------------------------------------------------

#[test]
fn action_parse_plain_command() {
    let a = Action::parse("SET_COOLING").unwrap();
    assert_eq!(a.name, "SET_COOLING");
    assert_eq!(a.value, None);
}

#[test]
fn action_parse_with_value() {
    let a = Action::parse("SET_COOLING_PULSE:80").unwrap();
    assert_eq!(a.name, "SET_COOLING_PULSE");
    assert_eq!(a.value, Some("80"));
}

#[test]
fn action_parse_rejects_empty_and_whitespace() {
    assert!(Action::parse("").is_none());
    assert!(Action::parse("   ").is_none());
}

#[test]
fn action_is_case_insensitive_and_known() {
    let a = Action::parse("set_cooling").unwrap();
    assert!(a.is("SET_COOLING"));
    assert!(a.is_known());
    let unknown = Action::parse("FLY_AWAY").unwrap();
    assert!(!unknown.is_known());
}

#[test]
fn apply_known_action_writes_gpio() {
    let mut hw = SimHardware::default();
    let fan_on = Action::parse("FAN_ON").unwrap();
    apply_action(&mut hw, &fan_on).unwrap();
    assert_eq!(hw.gpio_read(1).unwrap(), 1);
}

#[test]
fn apply_pulse_action_parses_duty() {
    let mut hw = SimHardware::default();
    let pulse = Action::parse("SET_COOLING_PULSE:80").unwrap();
    apply_action(&mut hw, &pulse).unwrap();
    assert_eq!(hw.pwm_duty(2), 80);
}

#[test]
fn apply_unknown_action_errors() {
    let mut hw = SimHardware::default();
    let bad = Action::parse("DO_THE_IMPOSSIBLE").unwrap();
    let err = apply_action(&mut hw, &bad).unwrap_err();
    assert!(err.contains("unknown action"));
}

// ---------------------------------------------------------------------------
// Enhanced action set
// ---------------------------------------------------------------------------

#[test]
fn gpio_write_action_routes_to_pin() {
    let mut hw = SimHardware::default();
    let a = Action::parse("GPIO_WRITE:9,1").unwrap();
    apply_action(&mut hw, &a).unwrap();
    assert_eq!(hw.gpio_read(9).unwrap(), 1, "pin 9 must be high");

    let a = Action::parse("LED_SET:7,0").unwrap();
    apply_action(&mut hw, &a).unwrap();
    assert_eq!(hw.gpio_read(7).unwrap(), 0, "pin 7 must be low");
}

#[test]
fn gpio_write_action_rejects_invalid_pin() {
    let mut hw = SimHardware::default();
    let a = Action::parse("GPIO_WRITE:notanumber,1").unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

#[test]
fn gpio_write_action_rejects_nonbinary_level() {
    let mut hw = SimHardware::default();
    let a = Action::parse("GPIO_WRITE:5,2").unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

#[test]
fn led_set_is_case_insensitive() {
    let mut hw = SimHardware::default();
    let a = Action::parse("led_set:3,1").unwrap();
    apply_action(&mut hw, &a).unwrap();
    assert_eq!(hw.gpio_read(3).unwrap(), 1);
}

#[test]
fn buzzer_sets_pwm_pin_3() {
    let mut hw = SimHardware::default();
    let a = Action::parse("BUZZER:75").unwrap();
    apply_action(&mut hw, &a).unwrap();
    assert_eq!(hw.pwm_duty(3), 75, "buzzer should drive pwm pin 3");
}

#[test]
fn buzzer_rejects_duty_over_100() {
    let mut hw = SimHardware::default();
    let a = Action::parse("BUZZER:150").unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

#[test]
fn buzzer_rejects_missing_duty() {
    let mut hw = SimHardware::default();
    let a = Action::parse("BUZZER").unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

#[test]
fn power_set_maps_all_valid_profiles() {
    let mut hw = SimHardware::default();
    for profile in 0u8..=3 {
        let cmd = format!("POWER_SET:{profile}");
        let a = Action::parse(&cmd).unwrap();
        assert!(
            apply_action(&mut hw, &a).is_ok(),
            "profile {profile} should be accepted"
        );
    }
}

#[test]
fn power_set_rejects_invalid_profile() {
    let mut hw = SimHardware::default();
    let a = Action::parse("POWER_SET:5").unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

#[test]
fn ble_send_action_rejects_overlong_payload() {
    let mut hw = SimHardware::default();
    let big = "x".repeat(4096 + 1);
    let cmd = format!("BLE_SEND:{big}");
    let a = Action::parse(&cmd).unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

#[test]
fn ble_send_action_accepts_max_payload() {
    let mut hw = SimHardware::default();
    let big = "x".repeat(4096);
    let cmd = format!("BLE_SEND:{big}");
    let a = Action::parse(&cmd).unwrap();
    assert!(apply_action(&mut hw, &a).is_ok());
}

#[test]
fn set_cooling_pulse_rejects_missing_duty() {
    let mut hw = SimHardware::default();
    let a = Action::parse("SET_COOLING_PULSE").unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

#[test]
fn set_cooling_pulse_rejects_out_of_range_duty() {
    let mut hw = SimHardware::default();
    let a = Action::parse("SET_COOLING_PULSE:120").unwrap();
    assert!(apply_action(&mut hw, &a).is_err());
}

// ---------------------------------------------------------------------------
// AppRuntime (original + new)
// ---------------------------------------------------------------------------

#[test]
fn runtime_ticks_and_dispatches_known_action() {
    let (mut app, _) = new_runtime();
    app.boot("function on_tick() return 'FAN_ON' end").unwrap();
    let t = app.tick().unwrap();
    assert_eq!(t.index, 1);
    assert!(t.dispatched);
    assert_eq!(t.result, "FAN_ON");
    assert_eq!(app.tick_count(), 1);
}

#[test]
fn runtime_tolerates_missing_on_tick() {
    let (mut app, _) = new_runtime();
    let t = app.tick().unwrap();
    assert!(!t.dispatched);
    assert!(t.result.is_empty());
}

#[test]
fn runtime_does_not_dispatch_informational_prose() {
    let (mut app, _) = new_runtime();
    app.boot("function on_tick() return 'Task: Tool result: 25.5C' end")
        .unwrap();
    let t = app.tick().unwrap();
    assert!(!t.dispatched);
    assert!(!t.result.is_empty());
}

#[test]
fn runtime_contains_per_tick_error() {
    let (mut app, _) = new_runtime();
    app.boot(
        "local n = 0 \
         function on_tick() n = n + 1 if n == 1 then error('first tick') end return 'FAN_OFF' end",
    )
    .unwrap();

    let first = app.tick().unwrap_err();
    assert!(matches!(first, LuaHostError::Lua(_)));

    let second = app.tick().unwrap();
    assert_eq!(second.result, "FAN_OFF");
    assert!(second.dispatched);
    assert_eq!(app.tick_count(), 2);
}

#[test]
fn runtime_watchdog_detects_stale_loop() {
    use std::time::Duration;
    let (mut app, _) = new_runtime();

    std::thread::sleep(Duration::from_millis(5));
    assert!(app.is_stale(Duration::from_millis(1)));

    app.boot("function on_tick() return '' end").unwrap();
    app.tick().unwrap();
    assert!(!app.is_stale(Duration::from_millis(1)));

    std::thread::sleep(Duration::from_millis(5));
    assert!(app.is_stale(Duration::from_millis(1)));
}

#[test]
fn runtime_boots_the_real_main_lua() {
    let (mut app, _) = new_runtime();
    app.boot(include_str!("../lua/main.lua")).unwrap();
    app.boot("function on_tick() return '' end").unwrap();
    for _ in 0..3 {
        let t = app.tick().unwrap();
        assert!(t.index >= 1);
    }
    assert_eq!(app.tick_count(), 3);
}

#[test]
fn runtime_health_tracks_errors() {
    use std::time::Duration;
    let (mut app, _) = new_runtime();
    app.boot(
        "local n = 0 \
         function on_tick() n = n + 1 if n == 1 then error('first') end return '' end",
    )
    .unwrap();

    let _ = app.tick().unwrap_err();
    let h = app.health(Duration::from_millis(100));
    assert_eq!(h.tick_count, 1);
    assert_eq!(h.error_count, 1);
    assert!(h.last_error.as_deref().unwrap_or("").contains("first"));

    app.tick().unwrap();
    let h = app.health(Duration::from_millis(100));
    assert_eq!(h.tick_count, 2);
    assert_eq!(h.error_count, 1, "error_count must be cumulative");
    assert!(!h.stale);
}

#[test]
fn runtime_health_reports_stale() {
    use std::time::Duration;
    let (mut app, _) = new_runtime();
    std::thread::sleep(Duration::from_millis(5));
    let h = app.health(Duration::from_millis(1));
    assert!(h.stale, "never ticked and over the timeout → stale");

    app.boot("function on_tick() return '' end").unwrap();
    app.tick().unwrap();
    let h = app.health(Duration::from_millis(100));
    assert!(!h.stale, "just ticked → not stale");
    assert_eq!(h.error_count, 0);
}

#[test]
fn runtime_can_stop_cleanly_via_shared_flag() {
    use std::sync::atomic::Ordering;
    let (mut app, _) = new_runtime();
    app.boot("function on_tick() return '' end").unwrap();
    let stop = app.stop_flag();
    app.tick().unwrap();
    assert!(!app.is_stop_requested());

    stop.store(true, Ordering::Release);
    assert!(app.is_stop_requested());
    app.request_stop(); // idempotent
    assert!(app.is_stop_requested());
}

#[test]
fn run_until_stop_honors_max_ticks() {
    use std::time::Duration;
    let (mut app, _) = new_runtime();
    let n = app.run_until_stop(Duration::ZERO, Some(3));
    assert_eq!(n, 3);
    assert_eq!(app.tick_count(), 3);
}

#[test]
fn run_until_stop_respects_stop_flag() {
    use std::time::Duration;
    let (mut app, _) = new_runtime();
    app.request_stop();
    let n = app.run_until_stop(Duration::ZERO, None);
    assert_eq!(n, 0, "must exit immediately once stop is requested");
}

#[test]
fn run_until_stop_counts_errors_even_with_many_failures() {
    use std::time::Duration;
    let hardware: SharedHardware =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    #[allow(clippy::arc_with_non_send_sync)]
    let agent: Arc<Mutex<MiniAgent>> = Arc::new(Mutex::new(MiniAgent::with_defaults().unwrap()));
    let vm = LuaVm::new(hardware.clone(), agent).unwrap();
    let mut app = AppRuntime::new(vm, hardware);
    app.boot("function on_tick() error('always fails') end")
        .unwrap();
    let ticks = app.run_until_stop(Duration::ZERO, Some(10));
    assert_eq!(ticks, 10);
    let h = app.health(Duration::from_secs(1));
    assert_eq!(h.tick_count, 10);
    assert_eq!(h.error_count, 10, "every tick must be counted as an error");
}

#[test]
fn run_until_stop_stops_immediately_when_stop_flag_pre_set() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let hardware: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    #[allow(clippy::arc_with_non_send_sync)]
    let agent: Arc<Mutex<MiniAgent>> = Arc::new(Mutex::new(MiniAgent::with_defaults().unwrap()));
    let vm = LuaVm::new(hardware.clone(), agent).unwrap();
    let mut app = AppRuntime::new(vm, hardware);
    app.boot("function on_tick() error('x') end").unwrap();
    app.stop_flag().store(true, Ordering::Release);
    let ticks = app.run_until_stop(Duration::ZERO, None);
    assert_eq!(ticks, 0, "must not run any ticks when stop is pre-set");
}

#[test]
fn reload_replaces_app_state() {
    use std::time::Duration;
    let (mut app, _) = new_runtime();
    app.boot("function on_tick() return 'A' end").unwrap();
    assert_eq!(app.tick().unwrap().result, "A");

    app.reload("function on_tick() return 'B' end").unwrap();
    let t = app.tick().unwrap();
    assert_eq!(t.result, "B");
    assert_eq!(app.tick_count(), 1, "counters reset on reload");
    assert_eq!(app.health(Duration::from_millis(100)).error_count, 0);
}

#[test]
fn reload_resets_error_health() {
    use std::time::Duration;
    let (mut app, _) = new_runtime();
    app.boot("function on_tick() error('boom') end").unwrap();
    let _ = app.tick().unwrap_err();
    assert_eq!(app.health(Duration::from_millis(100)).error_count, 1);

    app.reload("function on_tick() return '' end").unwrap();
    let h = app.health(Duration::from_millis(100));
    assert_eq!(h.error_count, 0, "health must reset on reload");
    assert_eq!(h.tick_count, 0);
}

#[test]
fn runtime_heartbeat_is_monotonic() {
    let (mut app, _) = new_runtime();
    app.boot("function on_tick() return '' end").unwrap();
    let t1 = app.tick().unwrap();
    let t2 = app.tick().unwrap();
    assert!(t2.index > t1.index);
    assert!(t2.uptime_ms >= t1.uptime_ms);
    assert_eq!(app.tick_count(), 2);
}

#[test]
fn app_runtime_tick_returns_correct_index_sequence() {
    let (mut app, _) = new_runtime();
    app.boot("function on_tick() return '' end").unwrap();

    for i in 1..=5 {
        let t = app.tick().unwrap();
        assert_eq!(t.index, i as u64, "tick {i} must have index {i}");
    }
    assert_eq!(app.tick_count(), 5);
}
