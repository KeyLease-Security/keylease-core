---
name: "Wave: Medium (150 pts)"
about: A multi-file task for Drips Wave contributors — a feature slice, refactor, or new test surface.
title: "[medium] "
labels: ["wave-medium", "150pts"]
assignees: ""
---

<!--
Drips Wave — Medium
Points: 150
Expected effort: ~1–3 days. May require a small design proposal before coding.
-->

## Summary

<!-- What is the goal, and what is the motivation? -->

## Context

<!-- Current behaviour, relevant code paths, and why it is insufficient. -->

## Proposed approach

<!-- The expected shape of the solution. Contributors may deviate if they explain why. -->

1. …
2. …

## Acceptance criteria

- [ ] …
- [ ] …
- [ ] New behaviour is covered by unit tests (happy path + failure path)
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] `cargo build --target wasm32v1-none --release --all` succeeds

## Security considerations

<!--
Medium tasks often touch auth, escrow, or accounting. Call out:
auth requirements, overflow handling, double-spend/double-settlement risks,
and the consumer refund path.
-->

- …

## Out of scope

- …

---

**Points: 150 (medium)** · Label: `wave-medium`
