# Type-level duplication between `host/nrf52-simulator` and `magent-hal::nrf52::sim`

This is the analysis that needs to exist before any consolidation
refactor lands. Every entry is anchored to a file:line in the
current tree so the next refactor pass can act on it directly.

## How to read this

For each duplicated type, the table shows:

* **Host** — definition site in `host/nrf52-simulator/src/lib.rs`.
* **HAL** — definition site in `magent-hal/src/nrf52/sim.rs`.
* **Verdict** — one of:
  * `drop host` — fields/methods identical or strict superset; the
    host copy is pure dead weight.
  * `drop host + adapter` — semantics differ slightly (atomic vs
    plain field, missing field, etc.). Swap to the HAL type but
    add a thin adapter at the call site (typically inside
    `SmartwatchSimulator`) to bridge the API gap.
  * `merge` — neither type is a strict superset; both are needed in
    a richer merged definition in `magent-hal`.
  * `keep host` — host type is *not* duplicated; it's host-specific
    and shouldn't be moved.

When the verdict is `merge`, a concrete "merged shape" sketch is
given so the next pass doesn't have to re-invent it.

---

## 1. Byte-identical enums

| Type | Host | HAL | Verdict |
|------|------|-----|---------|
| `PinState` | `lib.rs:78` | `sim.rs:70` | **drop host** — HAL adds explicit discriminants (`= 0`, `= 1`) and omits `Serialize`/`Deserialize`, but no consumer in this file uses the discriminant values; nothing breaks |
| `PinDirection` | `lib.rs:83` | `sim.rs:79` | **drop host** — identical variants, HAL omits derives that aren't used in this file |
| `BleState` | `lib.rs:88` | `sim.rs:109` | **drop host** — identical variants |
| `PowerMode` | `lib.rs:92` | `sim.rs:276` | **drop host** — identical variants |

These four are the easy wins. Drop the host definitions and add a
`use magent_hal::nrf52::sim::{PinState, PinDirection, BleState, PowerMode};`
import. ~15 lines deleted.

## 2. Structurally-similar types

### 2.1 `HeartRateMeasurement`

* **Host** (`lib.rs:115`): `{ rate: u16, sensor_contact: bool, energy: u16 }`
* **HAL** (`sim.rs:189`): `{ rate: u16, sensor_contact: bool, energy: u16, rr_intervals: [u16; 2] }`
* **Verdict**: **drop host** — HAL is a strict superset, no host
  consumer in `SmartwatchAgent::think()` reads `rr_intervals` (it's
  just emitted to the agent context).
* **Hosts reading**: `lib.rs:1110` (formats `health.heart_rate.rate`)
* **Lines deleted**: ~7

### 2.2 `StepData`

* **Host** (`lib.rs:123`): `{ steps: u32, stride_length: u8, cadence: u16 }`
* **HAL** (`sim.rs:198`): `{ steps: u32, stride_length: u8, activity: ActivityType }`
* **Verdict**: **merge** — different `u16 cadence` vs `enum ActivityType`.
* **Hosts reading**:
  * `lib.rs:895` — `self.steps.read().steps` (field `steps`, identical)
  * `lib.rs:1110` — formats `health.steps.steps` (field `steps`, identical)
* **Issue**: HAL has no `cadence`, host has no `activity`. The
  `sim.StepCounter` in `magent-hal/src/nrf52/sim.rs:969` does emit
  cadence internally but doesn't expose it.
* **Recommended merged shape** (in `magent-hal`):
  ```rust
  pub struct StepData {
      pub steps: u32,
      pub stride_length: u8,
      pub cadence: u16,        // keep
      pub activity: ActivityType,  // new
  }
  ```
  Adding one field is backwards-compatible for everyone.

### 2.3 `SpO2Measurement`

* **Host** (`lib.rs:131`): `{ saturation: f32, confidence: u8 }`
* **HAL** (`sim.rs:217`): `{ saturation: f32, perfusion_index: f32, pulse_rate: u16 }`
* **Verdict**: **merge** — host's `confidence: u8` is missing from
  HAL; HAL's `perfusion_index`/`pulse_rate` are missing from host.
* **Hosts reading**:
  * `lib.rs:890` — `format!("{:.1}", self.spo2.read().saturation)`
  * `lib.rs:1110` — `health.spo2.saturation`
* **Recommended merged shape**:
  ```rust
  pub struct SpO2Measurement {
      pub saturation: f32,
      pub confidence: u8,           // keep
      pub perfusion_index: f32,     // new
      pub pulse_rate: u16,          // new
  }
  ```

### 2.4 `BatteryState`

* **Host** (`lib.rs:138`): `{ voltage_mv: u32, percentage: u32, charging: bool, low_battery: bool, health: u8 }`
* **HAL** (`sim.rs:285`): `{ voltage_mv: AtomicU32, percentage: AtomicU32, charging: AtomicBool, low_battery: AtomicBool }`
* **Verdict**: **drop host + adapter** — same field names, different
  storage. HAL drops `health: u8` and uses atomic storage.
* **Hosts reading**:
  * `lib.rs:939` — `format!("{}% ({}mV)", self.battery.percentage, self.battery.voltage_mv)`
  * `lib.rs:941` — `self.battery.percentage`
  * Tests in `lib.rs:1395-1450` use `self.battery.percentage`, `.voltage_mv`
* **Migration cost**: ~6 lines. Replace direct field reads with
  `.percentage()`, `.voltage()`, `.is_charging()`, `.is_low()` method
  calls. Or keep atomic-load calls (they're public). The `health: u8`
  field is unused outside the host, so just drop it.

### 2.5 `SimulatedFlash`

* **Host** (`lib.rs:317`): `{ data: Vec<u8>, writes: Vec<u32>, sector_size: usize }`
  with `read()`, `write()` only.
* **HAL** (`sim.rs:546`): `{ data: StdVec<u8>, writes: StdVec<u32>, total_sectors: usize }`
  with `read()`, `write()`, `erase()`, `get_sector_writes()`, `is_sector_worn()`.
* **Verdict**: **drop host** — HAL is a strict superset, no host
  consumer calls `erase()`/`get_sector_writes()` but adding them is
  free.
* **Hosts reading**:
  * `lib.rs:917-928` — `flash_write` and `flash_read` tools
  * `lib.rs:1444` — `sim.flash.write(0, b"Test")`
* **Lines deleted**: ~25

### 2.6 `GpioController`

* **Host** (`lib.rs:345`): `{ pins: Vec<(PinDirection, PinState)> }`
  methods: `new()`, `set_state(pin, state) -> Result<(), String>`,
  `get_state(pin) -> Result<PinState, String>`, `set_direction()`.
* **HAL** (`sim.rs:648`): `{ pins: Vec<GpioConfig> }`
  methods: `new()`, `set(pin, state)`, `get(pin)`, `set_direction()`,
  `set_sense()`, `configure(pin, cfg)`, `num_pins()`,
  `count_active()`.
* **Verdict**: **drop host + adapter** — same shape, different method
  names. Host has `set_state`/`get_state`, HAL has `set`/`get`. The
  adapter is two-line `as` shims inside `SmartwatchSimulator`.
* **Hosts reading**:
  * `lib.rs:907-912` — `set_state`/`get_state` in `execute_tool`
  * `lib.rs:1429-1431` — tests using `set_state`/`get_state`
* **Lines deleted**: ~30

### 2.7 `BleController`

* **Host** (`lib.rs:378`): `{ state: BleState, connected_device: Option<String>, tx_count: u32, rx_count: u32 }`
  methods: `new()`, `connect(name)`, `disconnect()`,
  `send(data) -> Result<(), String>`.
* **HAL** (`sim.rs:710`): same fields but `tx_count: AtomicU64`,
  `rx_count: AtomicU64`, plus `BleAddress` (`bytes: [u8; 6]`),
  `ble_send(data)`, `get_ble_status()` returning `BleStatus`.
* **Verdict**: **drop host + adapter** — same shape, atomic vs
  plain fields, slight rename (`send` → `ble_send`).
* **Hosts reading**:
  * `lib.rs:932-933` — `ble_send` tool
  * `lib.rs:1437-1438` — tests using `connect`/`state`
* **Lines deleted**: ~30

### 2.8 `TemperatureSensor`

* **Host** (`lib.rs:401`): `{ base: f32, iter: u64 }`
  methods: `new()`, `read() -> f32`, `tick()`.
* **HAL** (`sim.rs:791` — `SimTemperatureSensor`): `{ current_temp: f32, base_temp: f32, variation: f32, iter: u64 }`
  methods: `new()`, `read() -> f32`, `tick()`.
* **Verdict**: **drop host** — both have `read() -> f32` and `tick()`.
* **Lines deleted**: ~30

### 2.9 `HeartRateSensor`

* **Host** (`lib.rs:418`): `{ rate: u16, iter: u64 }`
  methods: `new()`, `read() -> HeartRateMeasurement`, `tick()`.
* **HAL** (`sim.rs:894` — `SimHeartRateSensor`): `{ rate: u16, iter: u64, drift: f32, trend: f32 }`
  methods: `new()`, `read() -> HeartRateMeasurement`, `tick()`.
* **Verdict**: **drop host** — same public API.
* **Lines deleted**: ~25

### 2.10 `SpO2Sensor`

* **Host** (`lib.rs:434`): `{ sat: f32, iter: u64 }`
  methods: `new()`, `read() -> SpO2Measurement`, `tick()`.
* **HAL** (`sim.rs:934` — `SimSpO2Sensor`): `{ saturation: f32, perfusion: f32, pulse_rate: u16, iter: u64 }`
  methods: `new()`, `read() -> SpO2Measurement`, `tick()`.
* **Verdict**: **drop host + adapter** — host reads `self.sat` to
  build a `SpO2Measurement { saturation }`; HAL reads multiple
  fields. If `SpO2Measurement` is also merged (§2.3), the adapter
  shrinks to just constructor bridging.
* **Lines deleted**: ~25

### 2.11 `StepCounter`

* **Host** (`lib.rs:450`): `{ steps: u32 }`
  methods: `new()`, `read() -> StepData`, `tick(z_accel)`.
* **HAL** (`sim.rs:969`): `{ steps: u32, stride_length: u8, cadence: u16, activity: ActivityType, iter: u64 }`
  methods: `new()`, `read() -> StepData`, `tick(z_accel)`.
* **Verdict**: **drop host** — same `read()`/`tick()` signature.
* **Lines deleted**: ~25

### 2.12 `Accelerometer`

* **Host** (`lib.rs:463`): `{ x: f32, y: f32, z: f32, iter: u64 }`
  methods: `new()`, `read() -> (f32, f32, f32)`, `tick()`.
* **HAL** (`sim.rs:827` — `SimAccelerometer`): `{ x: f32, y: f32, z: f32, iter: u64 }`
  methods: `new()`, `read() -> (f32, f32, f32)`, `tick()`.
* **Verdict**: **drop host** — byte-identical.
* **Lines deleted**: ~25

## 3. Aggregator types (host-specific)

These are *not* in `magent-hal` and should stay local:

| Type | Site | Notes |
|------|------|-------|
| `HealthData` | `lib.rs:159` | Aggregates sensors into one struct for the agent context. Could be a thin shim over `magent_hal::SmartwatchData` (which has more fields) |
| `SystemInfo` | `lib.rs:165` | Watch-specific metadata (uptime, pin counts). Pure host concept |
| `VoiceState`, `SpeechRecognitionResult`, `SpeechSynthesisRequest` | `lib.rs:179-194` | Voice domain. Not in HAL |
| `SearchResult`, `SummaryResult` | `lib.rs:203-217` | Network domain. Not in HAL |
| `DeviceType`, `DeviceState` | `lib.rs:227-237` | Smart home domain. Not in HAL |

`HealthData` is the only candidate to bridge to `magent_hal::SmartwatchData`. The bridge is mechanical:

```rust
// Replace:
let h = self.read_health_data();
// with:
let s = self.hal_sim.read_all_sensors();
let h = HealthData {
    heart_rate: s.heart_rate,
    spo2: /* needs merge §2.3 to expose saturation+confidence */,
    steps: s.steps,
    battery: BatteryState {
        voltage_mv: s.battery.percentage as u32 * 10 + 3000, // synthetic
        percentage: s.battery.percentage as u32,
        charging: s.battery.charging,
        low_battery: s.battery.low,
        health: 100,
    },
    temperature: s.env.temperature,
    accelerometer: s.accelerometer,
};
```

Until `BatteryState` is merged (§2.4) and `SpO2Measurement` is merged (§2.3), this bridge has to reconstruct two types. After the merges, it shrinks.

## 4. Sub-simulators (host-specific)

These are pure domain types with no hardware abstraction and should
stay local:

* `VoiceProcessor` (`lib.rs:481`)
* `NetworkProcessor` (`lib.rs:569`)
* `SmartHomeController` (`lib.rs:687`)

## 5. The big one: `SmartwatchSimulator`

`SmartwatchSimulator` (`lib.rs:843`) is functionally identical to
`magent_hal::Nrf52Simulator` plus the three domain sub-simulators
above. The migration is **composition**:

```rust
pub struct SmartwatchSimulator {
    pub hal: Nrf52Simulator,           // ← magent-hal
    pub voice: VoiceProcessor,          // ← local
    pub network: NetworkProcessor,      // ← local
    pub smart_home: SmartHomeController, // ← local
    pub ble_connected_to: Option<String>, // for execute_tool
}
```

But this is non-trivial because:

1. The HAL type uses atomic cells for `BatteryState`, `BleController.tx_count`,
   `StepCounter.iter`. The host's `execute_tool` reads `self.battery.percentage`,
   `self.ble.state`, etc. — most reads can use the accessor methods, but
   `self.battery.percentage` as a field read needs to become
   `self.battery.percentage()` or `self.battery.percentage.load(...)`.
2. `execute_tool` (`lib.rs:882-985`) is 100+ lines and references
   every duplicated sensor/flash/GPIO/BLE type. It needs a parallel
   rewrite.
3. `SmartwatchAgent` (`lib.rs:1071`) holds `sim: SmartwatchSimulator`,
   not `Nrf52Simulator`. Either:
   * `SmartwatchAgent` switches to hold `hal: Nrf52Simulator` plus
     the three sub-simulators as separate fields, OR
   * `SmartwatchSimulator` becomes a thin facade that forwards most
     methods to the inner `Nrf52Simulator`.

The facade approach (option 2) is the least invasive: the agent
keeps the same call sites.

## 6. Estimated total win

| Bucket | LOC deleted |
|--------|-------------|
| §1 byte-identical enums | ~15 |
| §2 struct duplicates (including merge deltas) | ~225 |
| §5 SmartwatchSimulator facade | ~80 |
| §5 SmartwatchAgent → facade | ~50 |
| Tests updated | ~30 |
| **Total** | **~400 LOC** (out of 2,760 in `host/nrf52-simulator/src/`) |

Plus ~50 lines *added* (the facade body, the merge deltas in
`magent-hal/src/nrf52/sim.rs`). Net deletion: ~350 lines.

The `host/simulator/src/main.rs` side (§2.5 `FlashStorage`,
§2.6 `GpioController`, §2.7 `BleInterface`, §2.12 `GpioPinState`)
is a separate, smaller pass: ~250 LOC deletable on its own,
no merges needed (host `FlashStorage`/`GpioController` are
strictly older versions of the HAL types).

## 7. Order of operations (next session)

1. Apply §1 (4 enums) — trivial, ~15 LOC, zero risk.
2. Apply §2.1 (`HeartRateMeasurement`) — trivial, ~7 LOC.
3. Apply §2.4 (`BatteryState`) — small adapter, ~6 LOC changed.
4. Apply §2.5 (`SimulatedFlash`) — straightforward.
5. Apply §2.12 (`Accelerometer`) — byte-identical.
6. Apply §2.8 / §2.9 / §2.11 (`SimTemperatureSensor` /
   `SimHeartRateSensor` / `StepCounter`) — same shape.
7. Apply §2.6 / §2.7 (`GpioController` / `BleController`) — method
   rename (`set_state` → `set`, `send` → `ble_send`).
8. **Decide §2.2 / §2.3** (`StepData` / `SpO2Measurement`) — these
   require the merge into `magent-hal`. If the user wants zero
   changes to `magent-hal`, skip these two and keep local copies.
9. **Apply §5** (`SmartwatchSimulator` facade + `SmartwatchAgent`
   read-through) — biggest change, do last after the smaller wins
   are validated.
10. Update tests to match.

Step 1-7 should be a single `cargo check -p nrf52-simulator --tests`
turnaround. Step 9 needs its own session.

## 8. Open questions for the user

* Are we OK with merging `StepData`/`SpO2Measurement` into
  `magent-hal` (§2.2 / §2.3)? Or keep them local?
* Are we OK with `BatteryState` becoming an atomic type (HAL
  uses `AtomicU32`/`AtomicBool` because the simulator must be
  usable from `#![no_std]` contexts, where thread-safe access
  is conservative)? If not, the HAL type needs to keep two
  variants.
* `SmartwatchAgent::run()` (`lib.rs:1086`) currently has an Ollama
  branch that lives in `SmartwatchAgent::run_with_ollama`
  (`lib.rs:1853`) — does the user want this preserved as part of
  the facade, or extracted to a separate `OllamaBackend` struct?