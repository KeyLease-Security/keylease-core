# Contributing to keylease-core

Thanks for helping build KeyLease! This repository is a Soroban (Rust) smart
contract workspace. Please read this guide before opening a pull request.

## Prerequisites

```bash
rustup install stable
rustup component add rustfmt clippy
rustup target add wasm32v1-none
```

## Local workflow

```bash
git clone https://github.com/KeyLease-Security/keylease-core
cd keylease-core

cargo build --all                  # compile
cargo test --all                   # run the full test suite
cargo fmt --all                    # format
cargo clippy --all-targets -- -D warnings
```

Everything must be green **before** you open a PR — CI runs exactly the
commands above, plus `cargo build --target wasm32v1-none --release --all`.

## Repository layout

| Path | Purpose |
| --- | --- |
| `contracts/registry/src/lib.rs` | Entry points and protocol lifecycle. |
| `contracts/registry/src/types.rs` | `Service`, `Lease`, `LeaseStatus`. |
| `contracts/registry/src/storage.rs` | `DataKey` layout, TTL policy, accessors. |
| `contracts/registry/src/errors.rs` | Stable `Error` codes (never reuse a number). |
| `contracts/registry/src/test.rs` | Integration tests for the registry. |
| `contracts/mock_token/` | Test-only SEP-41 token. |
| `scripts/` | Deploy, issue generation, repo setup. |
| `docs/` | Protocol docs, contract reference, guides. |

Before your first change, read [`docs/developer-guide.md`](./docs/developer-guide.md)
and the [contract reference](./docs/contract-reference.md). The
[security model](./docs/security.md) lists the current known limitations.

## Code standards

- `#![no_std]` — contracts target `wasm32v1-none`; no `std` in contract code.
- **All token math is checked.** Use `checked_mul` / `checked_add` /
  `checked_sub` and map failures to `Error::Overflow`.
- **Authenticate actors.** Every state-changing entry point calls
  `require_auth()` on the address it represents.
- **Guard money paths.** Any path that moves escrow must first check that the
  lease is not already settled.
- **No unbounded loops** over user-supplied collections.
- **Errors over panics.** Return `Result<_, Error>`; do not `panic!` on
  recoverable conditions.
- **Don't reuse error codes.** Append new variants; codes are part of the ABI.
- Keep `lib.rs` entry points documented, including their auth and preconditions.

## Adding a new entry point

1. Add the variant(s) to `errors.rs` (append, never renumber).
2. Add any new `DataKey` variants to `storage.rs` with documented TTL policy.
3. Implement the entry point in `lib.rs`, documenting auth + preconditions.
4. Add tests covering the happy path **and** each failure/refund path.
5. Update the interface table in `README.md`.

## Tests

Prefer `env.mock_all_auths()` for happy-path flows, and assert specific
contract errors with the `try_*` client methods:

```rust
assert_eq!(
    client.try_settle_lease(&provider, &lease_id, &11),
    Err(Ok(Error::InvalidCalls))
);
```

Every security-sensitive change should include an adversarial test.

## Design notes for `high` (200 pt) issues

High-effort issues require a short design note **before** implementation.
Open a draft PR or comment on the issue with:

1. **Problem** — what is broken or missing, and its impact.
2. **Invariants** — what must remain true after the change.
3. **Approach** — the proposed design, and rejected alternatives.
4. **Threat model** — adversarial cases and how they are handled.
5. **Test plan** — the concrete tests that will be added.

A maintainer will approve the note before you begin coding.

## Pull requests

- Keep the diff focused; split unrelated changes into separate PRs.
- Fill in `.github/pull_request_template.md`, including the security checklist.
- Reference the issue you are closing (`Closes #123`).
- A PR is merged once CI is green and a maintainer approves.

## Commit messages

Use the imperative mood and explain the *why*:

```
Guard against double settlement in settle_lease

The payout path could be re-entered before `settled` was persisted,
allowing a provider to drain escrow twice. Set the flag prior to the
token transfers and cover it with an adversarial test.
```

## Reporting security issues

Please **do not** open a public issue for a security vulnerability. See
[SECURITY.md](./SECURITY.md) for private reporting and scope, or email
`security@keylease.dev` with a description and reproduction steps.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](./LICENSE).
