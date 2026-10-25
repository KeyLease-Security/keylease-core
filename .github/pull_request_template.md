# Pull Request

## Summary

<!-- One or two sentences describing what this PR does and why. -->

Closes #

## Wave / Points

<!-- Which Wave issue does this close, and for how many points? -->

- Wave points: `trivial (100) / medium (150) / high (200)`

## Type of change

- [ ] Bug fix (non-breaking change that fixes an issue)
- [ ] New feature (non-breaking change that adds functionality)
- [ ] Breaking change (fix or feature that changes existing behaviour)
- [ ] Documentation / tooling only
- [ ] Security hardening

## Contracts touched

- [ ] `contracts/registry`
- [ ] `contracts/mock_token`
- [ ] CI / tooling / docs

## How was this tested?

<!-- Describe the tests you ran, and include the exact commands. -->

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo build --target wasm32v1-none --release --all
```

## Soroban checklist

- [ ] Every state transition emits a typed event (`contracts/registry/src/events.rs`) with no secret material.
- [ ] New or changed entry points are reflected in `docs/contract-reference.md` and the README interface table.
- [ ] New storage keys have documented TTL semantics, and user-controlled lifetimes have a bound.
- [ ] `contractmeta!` / `version()` are updated when behaviour changes (workspace version bump).
- [ ] Any upgrade keeps the `DataKey` layout and stored-value semantics unchanged.
- [ ] Event names, topics and error codes are additive; nothing is renumbered or reused.

## Security checklist

- [ ] All state-changing entry points authenticate the relevant actor with `require_auth`.
- [ ] All token math uses checked arithmetic and handles overflow.
- [ ] No funds can be paid out twice (state persisted before any token call).
- [ ] Escrowed funds always have an exit path for the consumer.
- [ ] Tests cover the happy path, the failure/refund paths **and** the emitted event.
- [ ] The real-SAC test and the re-entrant-token test still pass.

## Reviewer notes

<!-- Anything reviewers should pay special attention to, or follow-up work. -->
