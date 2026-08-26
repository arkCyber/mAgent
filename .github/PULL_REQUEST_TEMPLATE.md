## Summary

<!-- One-paragraph summary of the change. Why is it needed? What problem does it solve? -->

## Type of change

- [ ] Bug fix (`fix`)
- [ ] New feature (`feat`)
- [ ] Security / safety hardening (`audit`)
- [ ] Documentation (`docs`)
- [ ] Refactor (no behavior change)
- [ ] Test additions or corrections
- [ ] CI / tooling (`chore`)

## Affected components

- [ ] `magent-core`
- [ ] `magent-hal`
- [ ] `firmware/nrf52-app`
- [ ] `firmware/esp32-app`
- [ ] `host/*`
- [ ] `cli`
- [ ] `tools`
- [ ] `examples/*`
- [ ] `docs/*`
- [ ] `.github/workflows/*` or `.cargo-patches/*`
- [ ] Other (specify)

## Target

- [ ] host
- [ ] nRF52840
- [ ] ESP32-C61
- [ ] N/A

## Checklist

- [ ] I have run `cargo fmt --all -- --check` locally
- [ ] I have run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] I have run `cargo test --workspace`
- [ ] I have rebased my branch onto `main`
- [ ] I have updated `docs/*` if behavior changed
- [ ] I have updated `CHANGELOG.md` if user-visible behavior changed
- [ ] I have NOT introduced any `unwrap()` / `expect()` / `panic!()` in
      `magent-core` or firmware paths
- [ ] I have NOT introduced any new direct `unsafe` outside the HAL boundary;
      any retained `unsafe` carries a `// SAFETY:` comment
- [ ] If my change adds a new bounded buffer, it uses `heapless` types
- [ ] If my change touches the agent-runtime contract (public API, JSON
      tool-call format, AT-command set, security-sensitive behavior), I have
      called it out explicitly below

## Contract-impacting changes

<!-- If you checked the last item above, describe the change here. Maintainers
     use this to schedule a security review. -->

## Release notes

<!-- One sentence that the project owner can copy into the next release notes.
     If your change is purely internal, write "None". -->

## Disclosure reminder

By submitting this PR you agree to license your contribution under the MIT
License (see `LICENSE`) and to acknowledge that this repository is the open
source codebase of the **mAgent** project (target commercial brand:
**arkChip-mAgent**). PRs that touch brand assets, trademark references, or
the commercial naming must be coordinated with the project owner before
opening.
