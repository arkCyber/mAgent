# Contributing to mAgent

Thank you for your interest in contributing to **mAgent** — the aerospace-grade
embedded AI agent platform targeting nRF52840 and ESP32-C61. This document
covers how to file issues, submit code, and what to expect from the review
process.

> **Disclosure reminder**: This repository is the open-source codebase of the
> **mAgent** project (target commercial brand: **arkChip-mAgent**). Any
> contribution you submit will be released under the project's MIT License and
> become part of the open codebase. Please do not submit proprietary
> information, customer data, or unreleased product roadmaps through this
> channel.

---

## Table of contents

1. [Code of Conduct](#code-of-conduct)
2. [Reporting issues](#reporting-issues)
3. [Pull requests](#pull-requests)
4. [Development environment](#development-environment)
5. [Building &amp; testing](#building--testing)
6. [Coding style](#coding-style)
7. [Aerospace-grade lint policy](#aerospace-grade-lint-policy)
8. [Commit messages](#commit-messages)
9. [Security disclosure](#security-disclosure)

---

## Code of Conduct

By participating, you agree to abide by the [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
Project maintainers enforce it; reports may be sent to **[conduct@arkchip.example]**.

---

## Reporting issues

Before opening an issue:

1. **Search existing issues** (open and closed) — your problem may already be
   triaged.
2. **Try the latest `main`** — the bug may already be fixed.
3. **Reproduce on the closest possible environment**:
   * For `magent-core` agent-runtime logic: the host simulator (`cargo run -p
     magent-simulator`) or the nRF52840 simulator (`cargo run -p
     magent-nrf52-simulator`) is usually enough.
   * For chip-specific bugs (BLE, GPIO, Wi-Fi): we strongly prefer
     hardware-in-the-loop reproductions on the real device. If you do not have
     the hardware, file the issue with the closest available logs from the
     host simulator and a clear description of the expected vs. observed
     behavior.

When opening an issue, please include:

- **Component**: `magent-core` / `magent-hal` / `firmware/nrf52-app` /
  `firmware/esp32-app` / `host/*` / `cli` / `tools` / `examples/*`
- **Target**: `host` / `nrf52840` / `esp32-c61`
- **Toolchain**: `rustc --version`, `cargo --version`, ESP-IDF version (if
  relevant), `probe-rs --version` (if relevant)
- **Repro command**: the exact `cargo build` / `cargo run` invocation
- **Expected vs. observed**: with log output (please redact any API keys or
  seed material; see "Security disclosure" below)
- **For crashes**: a `panic-probe` / `defmt` backtrace if available

For issues **only relevant to the project owner / commercial roadmap** (e.g.
funding, partnership, branded-chip delivery schedule), do **not** open a public
GitHub issue — see the project website for a contact channel.

---

## Pull requests

We follow a **fork + feature branch** workflow:

1. Fork the repository.
2. Create a topic branch from `main`: `git checkout -b fix/<short-slug>` or
   `feat/<short-slug>` or `audit/<short-slug>`.
3. Make your changes. Keep them focused — one logical change per PR. If your
   PR touches both an agent-runtime semantic and a chip-specific HAL path,
   please split it.
4. Make sure your branch is up to date with `main` and **rebased** (no merge
   commits in your branch — we rebase-merge in the GitHub UI).
5. Run the local checks listed in [Building &amp; testing](#building--testing).
   At minimum:
   * `cargo fmt --all -- --check`
   * `cargo clippy --workspace --all-targets --all-features -- -D warnings`
   * `cargo test --workspace`
6. Push the branch and open a PR. Fill in the PR template. If your change
   affects the **agent-runtime contract** (public API, JSON tool-call format,
   security-sensitive behavior), please call this out explicitly in the PR
   description — those get a maintainer review before merge.

### What we look for in review

- **Correctness**: tests cover the happy path **and** the failure path.
- **Bounded resources**: any new `Vec`, `String`, or buffer must be
  `heapless`-typed with an explicit capacity, unless it lives on the host
  (CLI / simulator) side and you have a written reason.
- **No panics in production code**: see [Aerospace-grade lint policy](#aerospace-grade-lint-policy).
- **No new direct calls to `unsafe`** outside the HAL boundary. If you
  genuinely need one, add a `// SAFETY:` comment and a unit test.
- **Traceability**: if the change addresses a `TRACE: REQ-…` requirement
  declared in the codebase, mention the requirement ID in the PR description.

### Out-of-scope for community PRs

- Changes to the in-tree vendored patches under `.cargo-patches/` — these
  mirror upstream crate sources with our local fixes and are updated through
  a separate process.
- Changes to brand assets, trademark references, or the commercial naming
  (mAgent vs. arkChip-mAgent) — coordinate via the project owner before
  opening a PR.

---

## Development environment

The workspace is a Rust `cargo` workspace with members spanning `no_std`
embedded targets and host-side tooling. Minimum toolchain:

| Component | Version | Notes |
|---|---|---|
| `rustup` | latest stable | |
| `rustc` | 1.70+ (host), 1.97+ (ESP32) | toolchain pinning lives in `rust-toolchain.toml` if present |
| `cargo` | bundled with `rustup` | |
| `rustfmt` + `clippy` | `rustup component add rustfmt clippy` | |
| `probe-rs` | latest | for nRF52840 flash / debug |
| `espflash` | latest | for ESP32 flash |
| `cargo-binutils` | latest | for `cargo size`, `cargo objcopy` |
| `cargo-llvm-cov` | latest | for coverage reports |

Embedded targets to install:

```bash
rustup target add thumbv7em-none-eabihf        # nRF52840 (ARM Cortex-M4F)
rustup target add riscv32imac-esp-espidf      # ESP32-C61 (RISC-V)
```

ESP32 builds additionally require the ESP-IDF toolchain; see
[`docs/ESP32_C61_BUILD.md`](docs/ESP32_C61_BUILD.md) for the full setup.

---

## Building &amp; testing

The project is a workspace; from the repo root:

```bash
# Format + lint (host-only, fast feedback)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Unit + integration tests (host)
cargo test --workspace

# Build the nRF52840 firmware (requires target above)
cargo build -p magent-nrf52-app --release \
  --target thumbv7em-none-eabihf

# Build the ESP32-C61 firmware (run from the firmware dir)
cd firmware/esp32-app
MCU=ESP32C61 cargo build --release

# Run the host simulator (no hardware required)
cargo run -p magent-simulator -- --task "read the temperature"
```

CI runs the same matrix on every PR. Pull requests are blocked from merge if
any of the following fail:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` on the host target
- the nRF52840 and ESP32-C61 build jobs (artifact-only — full flash tests are
  hardware-in-the-loop and run on a self-hosted runner pool)

---

## Coding style

- **`rustfmt` defaults** — do not fight the formatter; if you must, justify in
  the PR.
- **Naming**: `snake_case` for functions / variables; `PascalCase` for types
  and `SCREAMING_SNAKE_CASE` for constants. Module-private helpers that are
  only used inside one module: no `pub`.
- **Error handling**: every fallible operation returns `Result<T, AgentError>`.
  Do not introduce `unwrap()` / `expect()` in `magent-core` or in any firmware
  path; the workspace lints will reject your PR.
- **Heapless by default**: if you reach for `alloc::vec::Vec` or
  `alloc::string::String` inside `magent-core` or firmware, you almost
  certainly want `heapless::Vec<T, N>` instead. The host tooling
  (`host/*`, `cli`, `tools`) is allowed to use `alloc`.
- **Documentation**: every public item has at least a one-line `///` doc
  comment; the workspace warns on `missing_docs`.

---

## Aerospace-grade lint policy

The workspace enforces a small, deliberate set of `deny` lints under
[`Cargo.toml`](Cargo.toml) `[workspace.lints]`. These are non-negotiable for
code under `magent-core` and the firmware crates:

- `unsafe_op_in_unsafe_fn = "deny"` — every `unsafe` block inside an `unsafe
  fn` must be justified with a `// SAFETY:` comment.
- `panic_in_result_fn = "deny"` — no panic-from-a-`Result` paths.
- `mutex_atomic = "deny"` — no accidental `Mutex` where an `AtomicXxx` will
  do.

Style lints are at `warn`, not `deny`, so CI still builds but review surface
is preserved. **Demote a lint to `allow` only with a `TRACE: REQ-…`
comment** explaining why.

---

## Commit messages

We follow a lightweight Conventional Commits flavor:

```
<type>(<scope>): <subject>

<body>

<footer>
```

Where `<type>` is one of:

- `feat` — user-visible functionality
- `fix` — bug fix
- `audit` — security / safety hardening (no behavior change)
- `docs` — documentation only
- `refactor` — code change with no behavior change
- `test` — test additions or corrections
- `chore` — tooling, CI, dependencies

`<scope>` is optional and names the affected crate or area
(`agent`, `tools`, `skills`, `wallet`, `ci`, etc.).

The subject is **imperative** ("add", not "added"), **lowercase**, **no
period at the end**, and **≤ 72 characters**.

Example:

```
feat(tools): add parse_args() helper to fix substring bleed

The previous args.contains("…") heuristics mis-parsed inputs in two
realistic ways:
 * order coupling (hrv matched heart_rate first)
 * substring bleed ("10" matched state=high)

parse_args() is now the single source of truth for argument parsing
across all execute_* methods.
```

---

## Security disclosure

**Do not** open public GitHub issues for suspected vulnerabilities.

Send a private report to **[security@arkchip.example]** (PGP key on request)
with:

- A description of the issue and its impact
- A reproducer (firmware image, host command, or PoC code)
- The affected commit SHA / version

We aim to acknowledge within **3 business days** and to coordinate disclosure
on a 90-day clock (adjustable for legitimate complexity reasons). Security
fixes are tagged `audit/…` and shipped via the next patch release; critical
issues may fast-track an out-of-band release.

See [`SECURITY_AUDIT.md`](SECURITY_AUDIT.md) for the most recent internal
self-audit summary. *(Note: this is an internal AI-assisted self-audit; a
third-party audit is on the post-funding roadmap. See the commercial pitch
deck for the public timing commitment.)*

---

## License

By contributing, you agree that your contributions will be licensed under the
MIT License (see [`LICENSE`](LICENSE)). The project owner retains the right to
relicense the codebase as a whole for the commercial `arkChip-mAgent`
product line; your contributions remain under MIT for the open-source
distribution.
