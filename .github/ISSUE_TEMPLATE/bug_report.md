# Bug reports

> Thanks for taking the time to file a clear report. The project maintainers
> triage every issue; please fill in the sections that apply so we can route
> your report to the right owner on the first pass.

## Component

Which part of the workspace does the issue concern? (Pick one.)

- [ ] `magent-core` — chip-agnostic agent runtime (ReAct, tools, skills,
      web3/wallet)
- [ ] `magent-hal` — HAL trait surface + host-side nRF52840 simulator
- [ ] `firmware/nrf52-app` — nRF52840 firmware (BLE, sensors, watchdog)
- [ ] `firmware/esp32-app` — ESP32-C61 firmware (Wi-Fi, BLE, UART, NVS)
- [ ] `host/simulator` / `host/nrf52-simulator` — host simulators
- [ ] `host/email-mcp` / `host/mqtt-mcp` / `host/mcp-tool-executor` — host
      MCP tooling
- [ ] `cli` — `magent run …` CLI
- [ ] `tools` — dev tools, benchmarks, demos
- [ ] `examples/*` — application-case examples
- [ ] `docs/*` — documentation
- [ ] Build / CI (`.github/workflows/ci.yml`, in-tree patches under
      `.cargo-patches/`)
- [ ] Other (describe)

## Target

Which target were you building for when you hit the issue?

- [ ] Host (`cargo build` from the workspace root, no `--target` flag)
- [ ] nRF52840 (`thumbv7em-none-eabihf`)
- [ ] ESP32-C61 (`riscv32imac-esp-espidf`)
- [ ] Other (specify the toolchain / MCU)

## Environment

```
rustc --version   :  <output>
cargo --version   :  <output>
probe-rs --version:  <output>     (only if relevant)
ESP-IDF version   :  <version>    (only if relevant)
OS                :  <e.g. macOS 14.5 / Ubuntu 22.04>
Branch / commit   :  <git rev-parse HEAD>
```

## Reproduction

The smallest set of commands that reproduces the problem. For firmware issues
please also include the exact flash command and (if safe) the chip ID.

```bash
# 1. clone / checkout
# 2. install deps
# 3. build
# 4. flash (if applicable)
# 5. observe
```

## Expected behavior

What you expected to happen, with a short rationale if it is non-obvious.

## Actual behavior

What actually happened. Include logs — but **redact any API key, seed
phrase, Wi-Fi password, or Ed25519 secret** before pasting. See
`SECURITY.md` for how to report suspected vulnerabilities.

## Severity

How is this blocking you?

- [ ] Critical — data loss, bricked hardware, or live-system outage
- [ ] High — wrong behavior on a documented happy path
- [ ] Medium — wrong behavior only on edge cases, or a UX papercut
- [ ] Low — typo, doc improvement, refactor request

## Additional context

Screenshots, `defmt`/`panic-probe` backtraces, links to upstream issues, and
any related PRs you have already opened.

## Disclosure reminder

This repository is the open-source codebase of the **mAgent** project
(target commercial brand: **arkChip-mAgent**). Issues filed here are public.
For suspected **security vulnerabilities**, **customer data**, **funding**,
or **commercial-roadmap** topics, please use the channels listed in
`SECURITY.md` / `CONTRIBUTING.md` instead of filing a public issue.
