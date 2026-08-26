# Security Policy

> **Scope of this policy**: This document covers the **open-source mAgent
> codebase** published at `github.com/arkCyber/mAgent`. The target
> commercial product line is marketed under the brand **arkChip-mAgent**;
> security-relevant disclosures for the open codebase and the branded
> product are coordinated through the same channels below.

## Supported versions

The project maintains security fixes for the **latest released minor
version** of the workspace. Earlier versions receive fixes on a best-effort
basis at the maintainers' discretion.

| Workspace version | Supported |
|---|---|
| `0.1.x` (current `main`) | ✅ Active |
| `< 0.1.0`                | ❌ End of life |

> Until the first stable release (currently targeted at **v0.2.x**), the
> cadence is "fix forward to `main`"; backports to old releases are not
> provided.

## What counts as a security issue here

- **Memory unsafety** in `magent-core` / `magent-hal` / `firmware/*`
- **Unsafe cryptography** (non-constant-time paths, wrong padding, weak
  KDFs, RNG re-use)
- **Panics or undefined behavior** reachable from firmware inputs (BLE,
  UART, network) — these are normally caught by the workspace's
  `deny(panic_in_result_fn)` lint, but escape paths still exist
- **Wallet / keystore leakage**: any path that exposes plaintext key
  material, weakens the Argon2id parameters, or returns distinguishable
  error messages for "wrong passphrase" vs. "wrong ciphertext"
- **Supply-chain issues** in the in-tree vendored patches under
  `.cargo-patches/`
- **CI / build-system escapes**: workflow RCE, dependency-confusion,
  malicious update paths

The following are **out of scope** under this policy and should be filed
as regular issues:

- Reproductions that depend on physical access to a flashed device
  **without** any software-level exploit
- Theoretical weaknesses against an attacker who already controls the
  host that drives `esptool.py` / `probe-rs`
- Findings against third-party crates that have already been fixed
  upstream and whose fix the project has already pulled in (please
  reference the upstream CVE / advisory)

## Disclosure channel

**Do not** open a public GitHub issue for a suspected vulnerability. Use
one of:

- **Email**: **[security@arkchip.example]** (preferred)
- **GitHub private advisory**: Security tab → "Report a vulnerability" →
  "Open a private security advisory" — this keeps the report visible
  only to the maintainer team

If you need a PGP key for encrypted reports, request one in plain text
first and a maintainer will reply with a fresh subkey.

## What to include

A good report covers:

1. **Affected component and version** (workspace version, crate, commit
   SHA, chip target)
2. **Impact** (what does an attacker gain? confidentiality / integrity /
   availability; remote vs. local; user interaction required)
3. **Reproduction steps** — host command, firmware image, or PoC code
4. **Observed vs. expected behavior** — including a `defmt` / `panic-probe`
   backtrace when available
5. **Suggested mitigation** (optional but appreciated)
6. **Your disclosure plan / contact preference** (any 90-day clock you
   would like us to align with, embargo requirements, etc.)

Please **do not** include plaintext seed phrases, production wallet
contents, or live API keys in your report — even encrypted. If the
reproduction genuinely requires them, ask for a coordination channel
first.

## Our commitment

| Stage | Target |
|---|---|
| Acknowledge | within **3 business days** |
| Triage & severity assignment | within **10 business days** |
| Patch & coordinated disclosure | within **90 days** of acknowledgement, adjustable for legitimate complexity (e.g. firmware re-flash logistics) |
| Public advisory | at the time of the patch release; a CVE will be requested from MITRE where appropriate |
| Credit | the reporter is credited in the advisory and the release notes unless they ask to remain anonymous |

Critical issues (e.g. remote unauthenticated RCE on a default firmware
build, wallet seed recovery from at-rest images) may fast-track an
**out-of-band release** ahead of the next scheduled release.

## Internal self-audit baseline

The repository ships an **internal AI-assisted self-audit** at
[`SECURITY_AUDIT.md`](SECURITY_AUDIT.md). **This is NOT a third-party
audit.** It is published to:

1. Give users a starting-point checklist of the controls the project
   currently claims (memory safety, Result-only error paths, bounded
   budgets, watchdog integration, etc.);
2. Document the controls we plan to **independently verify** with a
   third-party firm (Trail of Bits / Cure53 / NCC Group — selection
   pending, committed to within 60 days of our next funding milestone);
3. Provide a before/after baseline so future third-party findings can be
   cross-referenced against what the team itself surfaced.

Treat the self-audit as **a roadmap of design intent**, not as a
certification. A third-party audit is on the post-funding roadmap.

## Bounty program

The project does not currently run a paid bug-bounty program. We do
credit reporters in release notes and advisories; commercial bounty
arrangements are evaluated on a case-by-case basis. If your organization
would like to sponsor or scope a bounty, contact **[security@arkchip.example]**.

## Out-of-band / brand-specific issues

For issues that **only** affect the commercially branded
**arkChip-mAgent** product line (e.g. partner-locked SKUs, customer
data-handling procedures, regulatory filings), please continue to use
**[security@arkchip.example]** — the same triage team handles both the
open codebase and the branded product line.
