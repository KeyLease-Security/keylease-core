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
cargo clippy --target wasm32v1-none --release --all -- -D warnings

# lint every tracked shell script (the CI Shellcheck job runs the same command)
git ls-files -z '*.sh' | xargs -0 shellcheck
```

Everything must be green **before** you open a PR — CI runs exactly the
commands above, plus `cargo build --target wasm32v1-none --release --all`,
[`./scripts/check-no-std.sh`](#unsafe-code-and-std) and a
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

## Dependency updates

`soroban-sdk` decides the contract's ABI and the Wasm artifact we deploy, so
dependency updates are not routine maintenance. This is the rule for how quickly
an update lands, who reviews it, and what an upgrade PR must contain.

**`soroban-sdk` is workspace-pinned.** The single version lives in
`[workspace.dependencies]` in [`Cargo.toml`](./Cargo.toml); every member declares
`soroban-sdk = { workspace = true }`. Never add a version requirement to a
member crate, and never let two SDK lines resolve at once — bump the workspace
pin, not the crates.

### Timing and review

| Update class | Typical trigger | When it lands | Review |
| --- | --- | --- | --- |
| **Security** — a RustSec advisory or a yanked crate in the build graph | The `Dependency Policy` job goes red, or an advisory is published upstream | Blocking. Merge as soon as the fix is green: **within 7 days** of publication, and **within 72 hours** if the affected code is reachable from a money path (`create_lease`, `settle_lease`, `revoke_expired_lease`, `set_token`, `upgrade`) | One maintainer, plus a second reviewer whenever `deny.toml` or the public interface changes |
| **Minor / patch** — non-security updates inside the pinned SDK line, and all transitive crates | The weekly Dependabot PR (`.github/dependabot.yml`) | Merge within **14 days** of opening, provided CI is green and the diff is mechanical | One maintainer |
| **Major `soroban-sdk` upgrade** — `26` → `27`, or any `soroban-env-*` major | A maintainer opens an issue; Dependabot does not file these | Planned work with an explicit target release — no automatic deadline, but it is tracked until merged | Two maintainers, and the Testnet redeploy recorded in [`docs/deployments.md`](./docs/deployments.md) before tagging |

A security update that needs a contract change (not just a version bump) is
exempt from the usual "design note first" step in
[`high` issues](#design-notes-for-high-200-pt-issues) so the fix can ship
inside the window above; the note and the adversarial test are added in the same
PR. Do not discuss the vulnerability in the public issue — follow
[SECURITY.md](./SECURITY.md).

### How the advisory scan enforces it

The **`Dependency Policy`** CI job runs `cargo deny check` against the committed
`Cargo.lock` and [`deny.toml`](./deny.toml). It fails the PR on any RustSec
advisory, yanked crate, unlicensed dependency, wildcard requirement or unknown
git/registry source that is not explicitly accounted for. Because it is a
required status check, **an advisory cannot reach `main` unless a human writes an
ignore entry** — so an ignore is a reviewed security decision, not a mute button:

- Every `[advisories].ignore` entry needs a reason, a linked tracking issue and a
  date by which it must be revisited.
- Adding or removing an ignore needs two maintainer approvals in addition to the
  green checks, and the PR must say why the vulnerable code is unreachable from
  the contract and what upstream release will clear it.
- Ignoring an advisory whose code *is* reachable is not allowed. Fix or pin the
  dependency instead; if there is no compatible release yet, `cargo update
  --precise` the smallest subtree that clears it, or document the block in the
  issue and hold the release.

The malware scan and the build do not replace this check: `Dependency Policy`
is the only job that inspects the dependency graph.

### Review requirements for any dependency PR

A dependency PR is merged when all of the following hold:

1. Every required check is green — `Format`, `Clippy`, `Test`,
   `Build (wasm32v1-none)`, `Dependency Policy` and `Malware Scan`.
2. The reviewer count for its class above has approved, and a maintainer signed
   off on any change under `deny.toml`.
3. `Cargo.lock` is the only diff, unless a manifest bump was genuinely required.
4. The Wasm hash check below is recorded in the PR description.

### Upgrade PR checklist

Copy this into the PR body for any change to `Cargo.lock`, `Cargo.toml` or the
SDK pin:

- [ ] The update is one class only (security, minor/patch, or a major SDK bump);
      unrelated bumps are split into their own PRs.
- [ ] `cargo deny check` passes locally, and no `deny.toml` ignore was added — or
      every added ignore has a reason, a tracking issue and a revisit date, plus
      two maintainer approvals.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
      `cargo test --all` and `cargo build --target wasm32v1-none --release --all`
      all pass.
- [ ] **Wasm hash check** — build the release artifact and compare it with the
      deployed hash in [`docs/deployments.md`](./docs/deployments.md):

  ```bash
  cargo build --target wasm32v1-none --release --all
  sha256sum target/wasm32v1-none/release/keylease_registry.wasm
  ```

  Paste both hashes (new and the one currently recorded) in the PR. An unchanged
  hash means no on-chain action is needed; a changed hash must be recorded with
  the new version in `docs/deployments.md` before the release is tagged.
- [ ] The Wasm still exposes the same interface: `contractmeta!` and `version()`
      are bumped with the workspace version, and no entry point, event or error
      code was renumbered.
- [ ] For a major SDK bump: the workspace version is bumped, the release notes
      name the new SDK line, and the new Wasm is uploaded and deployed to
      Testnet with one full `create_lease` → `settle_lease` lifecycle replayed
      afterwards.
- [ ] `stellar contract info meta --id <REGISTRY> --network testnet` shows the
      new `version` and `rssdkver` after the redeploy.
- [ ] `rssdkver`-dependent docs and badges (`README.md`, `docs/developer-guide.md`,
      `docs/architecture.md`) are updated when the SDK line changes.

## Text and documentation checks

Five checks run in CI outside the Rust build, and each reads its configuration
from this repository, so the local command below is the same check and fails the
same way:

| Check | Command | Config | CI job |
| --- | --- | --- | --- |
| Spell check | `typos` | [`_typos.toml`](./_typos.toml) | `Spell Check` |
| Markdown lint | `npx markdownlint-cli2@0.24.0` | [`.markdownlint-cli2.jsonc`](./.markdownlint-cli2.jsonc) | `Markdown Lint` |
| Workflow lint | `actionlint` | none; it lints every file under `.github/workflows/` | `Action Lint` |
| Crate documentation | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items` | the doc comments themselves | `Rustdoc` |
| Nav, anchors, links | `mkdocs build --strict` | [`mkdocs.yml`](./mkdocs.yml) | `Docs → Build preview` (PR), `Build & deploy` (`main`) |

```bash
cargo install typos-cli --locked            # one-time
typos

brew install actionlint shellcheck          # one-time; see the note below
actionlint                                  # every file in .github/workflows

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

`actionlint` checks the workflow files themselves: expression syntax, `needs:`
targets, and, through `shellcheck`, every inline `run:` script. That last part is
delegated, so `actionlint` skips it silently when `shellcheck` is not on `PATH`;
the CI job installs both, and so must you if the local run is to match.
`cargo doc` with `RUSTDOCFLAGS=-D warnings` turns an intra-doc link that does not
resolve, or a manifest field cargo calls redundant, into a build failure. It does
not enable `missing_docs`: the Soroban macros generate items nobody can annotate,
and silencing them would weaken the check instead of documenting the contract.

The Docs workflow gates a pull request through its `Build preview` job, but only
when the PR touches `docs/**`, `mkdocs.yml`, `requirements-docs.txt` or the
workflow file itself. A documentation change therefore fails on the anchor
before it merges — which is why the local command above is still worth running,
since it is the same build and it answers in seconds. A change that touches none
of those paths gets no Docs job on the PR at all, so if you edit `nav:` in a
branch that only changes Rust, run it yourself. The checks that guard your branch
are the CI jobs `Format`, `Clippy`, `Test`, `Build (wasm32v1-none)`, `Rustdoc`,
`Spell Check`, `Markdown Lint`, `Action Lint`, `Dependency Policy` and the required
`Malware Scan`.

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
| `scripts/` | Deploy, no_std check, malware scan, issue generation, repo setup. |
| `docs/` | Protocol docs, contract reference, guides. |

Before your first change, read [`docs/developer-guide.md`](./docs/developer-guide.md)
and the [contract reference](./docs/contract-reference.md). The
[security model](./docs/security.md) lists the current known limitations, and
[`docs/integration.md`](./docs/integration.md) describes how Stellar clients
verify leases.

## Code standards

- `#![no_std]` and no `unsafe` — see [Unsafe code and std](#unsafe-code-and-std).
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

## Unsafe code and std

Contract crates (everything under `contracts/`) hold escrow, so the deployed
Wasm must contain only code the Soroban SDK and this repository vouch for.

- **No `unsafe`.** Each crate's `lib.rs` starts with:

  ```rust
  #![no_std]
  #![cfg_attr(target_family = "wasm", forbid(unsafe_code))]
  #![cfg_attr(not(target_family = "wasm"), deny(unsafe_code))]
  ```

  The deployed `wasm32v1-none` build uses `forbid`, which nothing can
  override. The host build (tests, clippy) uses `deny`: soroban-sdk's
  `testutils` feature registers contracts through the `ctor` crate, whose
  generated code carries its own `#[allow(unsafe_code)]`, and `forbid` would
  reject that. `deny` still rejects any `unsafe` written in the crate, on both
  targets. Do not add `#[allow(unsafe_code)]`; if you believe you need unsafe,
  open an issue first.
- **No `std`.** Contracts are `#![no_std]` and may use `core` and
  `soroban-sdk`. A new dependency must build for `wasm32v1-none`
  with `default-features = false` where it has a `std` feature. Test-only
  crates go in `[dev-dependencies]`, never `[dependencies]`.

`./scripts/check-no-std.sh` enforces both in CI (the `Build (wasm32v1-none)`
job). It fails if a crate drops these attributes, if the toolchain ships a
`std` for `wasm32v1-none`, if the release build fails (a crate that needs `std`
cannot build for that target), or if a built `.wasm` imports anything other
than Soroban host modules or contains `std` source paths. Run it locally
before opening a PR that touches dependencies or crate attributes.

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
- Dependency bumps follow the timing, review and checklists in
  [Dependency updates](#dependency-updates).
- Fill in `.github/pull_request_template.md`, including the security checklist.
- Reference the issue you are closing (`Closes #123`).
- A PR is merged once CI is green and a maintainer approves.
- Area labels (`area: registry`, `area: mock-token`, `area: docs`,
  `area: scripts`, `area: ci`) are added automatically from the paths you
  touch; see [`.github/labeler.yml`](./.github/labeler.yml). If you add a
  top-level directory, add a rule there and its label to
  `scripts/setup-repo.sh`.

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
