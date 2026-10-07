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

## Security checklist

- [ ] All state-changing entry points authenticate the relevant actor with `require_auth`.
- [ ] All token math uses checked arithmetic and handles overflow.
- [ ] No funds can be paid out twice (re-entrancy / double-settlement guard).
- [ ] Escrowed funds always have an exit path for the consumer.
- [ ] New storage keys have documented TTL semantics.
- [ ] Tests cover both the happy path **and** the failure/refund paths.

## Reviewer notes

<!-- Anything reviewers should pay special attention to, or follow-up work. -->
