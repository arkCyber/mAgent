# CHANGELOG

All notable changes to the **mAgent** open-source codebase will be documented
here. Dates use `YYYY-MM-DD` and follow ISO 8601. Versions follow
[Semantic Versioning](https://semver.org/).

> **Scope reminder**: This file tracks the **open-source codebase**
> (`mAgent` / `MicroAgent`). The target commercial product line is marketed
> under the brand **arkChip-mAgent**; commercial SKU release notes are kept
> in a separate, private changelog and are shared with partners under NDA.

---

## [Unreleased]

### Added
- **`AT+LLMCFG=` validation hardening** (`magent-core::at_validate::validate_llmcfg_set`):
  host-tested validator for the LLM backend model + API key that enforces
  length caps (model ≤64, key ≤128), valid UTF-8, and rejects NUL / control
  bytes / whitespace in the key. Wired into the ESP32 `llmcfg_dispatch` so a
  malformed config is rejected before it is written to NVS. 16 new host tests.
- **`AT+HTTPGET=` URL validation hardening** (`magent-core::at_validate::validate_httpget_set`):
  host-tested validator that whitelists `http://` / `https://` (case-insensitive),
  caps length at 512, and rejects NUL / control bytes / non-HTTP schemes —
  hardening the SSRF-sensitive URL surface before any worker thread is spawned.
  12 new host tests.
- **`AT+BLE=` validation hardening** (`magent-core::at_validate::validate_ble_set`):
  pure, host-tested decision helper that accepts only `ON` / `OFF` / `STATE`
  (case-insensitive) and rejects malformed forms (empty, quoted, `key=val`,
  numeric, unknown verb) with precise `+CMDER:4` / `:7` errors. Wired into the
  ESP32 dispatcher's `ble_dispatch` so a bad BLE control line is rejected
  before reaching the BLE stack. 16 new host unit tests cover every accept /
  reject path.
- **Open-source governance surface** (this PR): `LICENSE` (MIT),
  `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1), `CONTRIBUTING.md`,
  `SECURITY.md`, issue / PR templates under `.github/`, and a public
  `.github/workflows/ci.yml` covering host checks (fmt, clippy, test) plus
  nRF52840 and ESP32-C61 firmware builds.
- **Internal self-audit disclosure band**: a clear "this is an internal
  AI-assisted self-audit, not a third-party audit" callout at the top of
  `SECURITY_AUDIT.md` and `docs/AUDIT_AEROSPACE_2026.md`, with a written
  commitment that a third-party audit is on the post-funding roadmap
  (Trail of Bits / Cure53 / NCC Group — selection pending).
- CI status / license / audit-status badges in `README.md`.

### Changed
- `README.md` — added CI / License / Audit badges and a confidentiality
  notice; added cross-links to `SECURITY.md`, `SECURITY_AUDIT.md`,
  `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, and `LICENSE`.
- `SECURITY_AUDIT.md` — author is now labeled "Internal AI-assisted
  self-audit, performed by the project owner"; "Auditor Signature" replaced
  with "Internal self-audit signature"; "Next Audit" clarified into
  "Next internal review" + "Next independent audit" commitments.
- `docs/AUDIT_AEROSPACE_2026.md` — same disclosure band added at the top
  and at the conclusion; mapping to DO-178C / ISO 26262 / IEC 61508
  clarified as **informative only**.

### Security
- No code-level security fixes in this release. Disclosure-band changes
  are documentation-only and are not themselves CVEs.

### Fixed
- **`magent-core` `property_tests` target would not compile** — it uses
  `web3::wallet::Keystore`, so its `required-features` now includes `wallet`
  (previously only `std` + `web3`, so `cargo test -p magent-core` failed to
  build the target with `could not find 'wallet' in 'web3'`).
- **CLI test suite hung under interactive `cargo test`** — the
  `web3_blockchain::stdin_read_does_not_panic_on_eof` test blocked forever on
  `std::io::stdin().read()`. It now guards with `is_terminal()` so it only
  reads when stdin is piped/closed, and never hangs the suite.
- **CLI `email_executor::debug_impl_covers_all_variants` failed when both
  `web3` and `email-tools` features are enabled** — `CompositeExecutor::new(_)`
  then returns the `Full` variant, not `WithEmailTools`; the assertion now
  expects the variant the active feature set actually produces.
- **nRF52840 firmware (`magent-nrf52-app`) failed to compile** — the `BLE_STATE`
  static was typed as the `BleState` enum but initialised with `BleStateManager`
  struct fields (fixed to `BleStateManager` with a const-compatible literal);
  `handle_characteristic_read` used `Vec<u8>` in a `no_std` crate (added
  `extern crate alloc`); and `info!("{:?}", char_idx)` required the
  `defmt::Format` derive, which is not enabled (replaced with a `&'static str`
  name helper). The firmware now builds.
- **nRF52840 integration-test firmware failed to link** — it was missing the
  `memory.x` linker script and the `build.rs` that exposes it via
  `cargo:rustc-link-search` (plus `-Tdefmt.x`); both added so `rust-lld` can
  resolve the `INCLUDE memory.x` in `cortex-m-rt`'s `link.x`.

---

## [0.1.0] — Initial open release

First public release of the workspace. See [`README.md`](README.md) for the
overall architecture, [`docs/NRF52_BUILD_GUIDE.md`](docs/NRF52_BUILD_GUIDE.md)
and [`docs/ESP32_C61_BUILD.md`](docs/ESP32_C61_BUILD.md) for build
instructions, and [`SECURITY_AUDIT.md`](SECURITY_AUDIT.md) for the
self-audit baseline.

[Unreleased]: https://github.com/arkCyber/mAgent/compare/main...HEAD
[0.1.0]: https://github.com/arkCyber/mAgent/releases/tag/v0.1.0
