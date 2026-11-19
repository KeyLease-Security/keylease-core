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

# lint every tracked shell script (the CI Shellcheck job runs the same command)
git ls-files -z '*.sh' | xargs -0 shellcheck
```

Everything must be green **before** you open a PR — CI runs exactly the
commands above, plus `cargo build --target wasm32v1-none --release --all` and a
ClamAV malware scan of the checkout and the built Wasm artifacts. The scan job
("Malware Scan") is a required check, so do not commit binaries or archives
that you cannot explain; CI will fail the PR if a signature matches.

Shellcheck runs at its default severity, which includes style notes, and fails
if no tracked `*.sh` file matches, so the job cannot pass without covering
something. Install it with `brew install shellcheck` or
`sudo apt-get install shellcheck`. Suppress a finding with a
`# shellcheck disable=SC…` comment on the offending line plus a reason; never
disable a check for a whole file.

## Local security checks

CI's dependency policy and malware scan can be reproduced locally:

```bash
cargo install cargo-deny --locked   # one-time
cargo deny check

sudo freshclam                      # one-time ClamAV signature update
./scripts/scan-malware.sh
```

`scan-malware.sh` scans the working tree and any built Wasm. Pass a path to scan
a downloaded artifact, and see `--help` for the other flags. The policy lives in
[`deny.toml`](./deny.toml).

## Text and documentation checks

Three non-Rust checks run in CI, and each reads its configuration from this
repository, so the local command below is the same check and fails the same way:

| Check | Command | Config | CI job |
| --- | --- | --- | --- |
| Spell check | `typos` | [`_typos.toml`](./_typos.toml) | `Spell Check` |
| Markdown lint | `npx markdownlint-cli2@0.24.0` | [`.markdownlint-cli2.jsonc`](./.markdownlint-cli2.jsonc) | `Markdown Lint` |
| Nav, anchors, links | `mkdocs build --strict` | [`mkdocs.yml`](./mkdocs.yml) | `Docs → Build & deploy` |

```bash
cargo install typos-cli --locked            # one-time
typos

pip install -r requirements-docs.txt        # one-time
mkdocs build --strict
```

`typos` with no arguments walks the tracked text files and excludes `Cargo.lock`
(see `[files] extend-exclude`); add a new domain term to
`[default.extend-words]` instead of weakening the check. `markdownlint-cli2`
picks up the glob and rule tuning from the `.jsonc` file, so running it with no
arguments lints exactly what CI lints. `mkdocs build --strict` promotes a broken
nav entry or a missing in-page anchor to an error, which is why internal links
must target real heading slugs.

The Docs workflow runs on `push` to `main` and `workflow_dispatch` only, so a
broken anchor will not fail your PR — run `mkdocs build --strict` locally before
opening one. The checks that guard your branch are the CI jobs `Format`,
`Clippy`, `Test`, `Build (wasm32v1-none)`, `Spell Check`, `Markdown Lint`,
`Dependency Policy` and the required `Malware Scan`.

## Repository layout

| Path | Purpose |
| --- | --- |
| `contracts/registry/src/lib.rs` | Entry points, `contractmeta!`, upgrade, protocol lifecycle. |
| `contracts/registry/src/events.rs` | Typed Soroban events, the indexing surface. |
| `contracts/registry/src/types.rs` | `Service`, `Lease`, `LeaseStatus`. |
| `contracts/registry/src/storage.rs` | `DataKey` layout, TTL policy, accessors. |
| `contracts/registry/src/errors.rs` | Stable `Error` codes (never reuse a number). |
| `contracts/registry/src/test.rs` | Lifecycle, SAC, event, upgrade and re-entrancy tests. |
| `contracts/mock_token/` | Test-only SEP-41 token. |
| `scripts/` | Deploy, issue generation, repo setup. |
| `docs/` | Protocol docs, contract reference, guides. |

Before your first change, read [`docs/developer-guide.md`](./docs/developer-guide.md)
and the [contract reference](./docs/contract-reference.md). The
[security model](./docs/security.md) lists the current known limitations, and
[`docs/integration.md`](./docs/integration.md) describes how Stellar clients
verify leases.

## Code standards

- `#![no_std]` — contracts target `wasm32v1-none`; no `std` in contract code.
- **All token math is checked.** Use `checked_mul` / `checked_add` /
  `checked_sub` and map failures to `Error::Overflow`.
- **Authenticate actors.** Every state-changing entry point calls
  `require_auth()` on the address it represents.
- **Follow checks-effects-interactions.** Any path that moves escrow must first
  check that the lease is not already settled and persist the settlement before
  calling the token.
- **Emit events.** Every state transition publishes a typed event from
  `events.rs`, and events never contain secret material.
- **Bound storage.** New persistent keys need a TTL policy; user-controlled
  lifetimes need an upper bound (see `MAX_LEASE_DURATION_SECS`).
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

Assert emitted events with `assert_single_event`, and keep the two Stellar-specific
safety nets green: the lifecycle test that escrows through the real Stellar Asset
Contract, and the re-entrant-token test that proves a lease cannot be settled
twice. Every security-sensitive change should include an adversarial test.

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

```text
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
