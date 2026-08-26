---
name: Feature request
about: Propose a new capability for the mAgent agent runtime
title: "[feat] "
labels: ["enhancement", "needs-triage"]
assignees: []
---

## Problem

What problem does this feature solve? If it is related to a defect, link the
issue.

## Proposed solution

A short description of the behavior you want, including the API shape (Rust
signatures, JSON tool-call schema, AT-command set, etc.).

## Alternatives considered

What other approaches did you consider, and why is the proposed solution
better?

## Scope and impact

- Which crates does this touch?
- Does it require a `TRACE: REQ-…` change in `Cargo.toml` `[workspace.lints]`?
- Does it introduce any new dependency?
- Does it change the on-the-wire protocol (BLE GATT, AT, MQTT, RPC)?

## Backwards compatibility

Does the change break existing API, JSON schemas, AT-command responses, or
NVS layouts?

## Disclosure reminder

This repository is the open-source codebase of the **mAgent** project
(target commercial brand: **arkChip-mAgent**). If your request concerns
**commercial roadmap** topics (target brand features, partner exclusivities,
funding-driven priorities), please coordinate via the project owner instead
of filing a public request.
