#!/usr/bin/env bash
#
# create-issues.sh — open the Drips Wave backlog for keylease-core in one run.
#
# Every issue below is written in the shape of the templates in
# .github/ISSUE_TEMPLATE/, uses the Wave point labels
# (wave-trivial/100pts, wave-medium/150pts, wave-high/200pts), and states
# acceptance criteria a reviewer can check.
#
# Usage
#   ./scripts/create-issues.sh              # create every issue
#   ./scripts/create-issues.sh --dry-run    # print what would be created
#   ./scripts/create-issues.sh --only high  # only one tier
#
# Requires the GitHub CLI, authenticated with `gh auth login`.

set -euo pipefail

REPO="${KEYLEASE_REPO:-KeyLease-Security/keylease-core}"
DRY_RUN=0
ONLY=""

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --only)    ONLY="${2:-}"; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

command -v gh >/dev/null 2>&1 || { echo "error: gh CLI not found" >&2; exit 1; }

created=0
skipped=0

# create_issue <tier> <title> <labels> ; body on stdin
create_issue() {
  local tier="$1" title="$2" labels="$3"
  local body
  body="$(cat)"

  if [ -n "$ONLY" ] && [ "$ONLY" != "$tier" ]; then
    return 0
  fi

  if gh issue list --repo "$REPO" --state all --search "$title in:title" \
       --json title --jq '.[].title' 2>/dev/null | grep -Fxq "$title"; then
    echo "skip (exists): $title"
    skipped=$((skipped + 1))
    return 0
  fi

  if [ "$DRY_RUN" = "1" ]; then
    echo "----------------------------------------"
    echo "title : $title"
    echo "labels: $labels"
    echo "----------------------------------------"
    echo "$body"
    created=$((created + 1))
    return 0
  fi

  local tmp
  tmp="$(mktemp)"
  printf '%s\n' "$body" > "$tmp"
  IFS=',' read -r -a label_args <<< "$labels"
  gh issue create --repo "$REPO" --title "$title" --body-file "$tmp" \
    $(for l in "${label_args[@]}"; do printf ' --label %s' "$l"; done) >/dev/null
  rm -f "$tmp"
  echo "created: $title"
  created=$((created + 1))
}

# ===========================================================================
# Trivial — 100 pts
# ===========================================================================

create_issue trivial \
  "[trivial] Enforce a Wasm size budget in CI" \
  "wave-trivial,good first issue,100pts" <<'EOF'
## Summary

The registry is deployed on-chain, where contract code is billed by size.
Nothing currently stops a pull request from silently inflating the release
Wasm. Add a CI guard that fails when `keylease_registry.wasm` exceeds a
documented budget.

## Where

- `.github/workflows/ci.yml` (new step in the `Build (wasm32v1-none)` job)
- `README.md` (document the budget)

## Acceptance criteria

- [ ] After the existing build step, the Wasm job fails if
      `target/wasm32v1-none/release/keylease_registry.wasm` is larger than a
      budget you justify in the PR description (start from the current size and
      add a small margin).
- [ ] The failure message prints the actual size and the budget.
- [ ] The budget is documented in `README.md`.
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes

## Out of scope

- Optimizing the contract to shrink it (file a separate issue if needed).
- Caching/artifact upload.

## Pointers

- The `wasm32v1-none` release profile is in the workspace `Cargo.toml`.
EOF

create_issue trivial \
  "[trivial] Add cargo-audit / dependency advisory scanning to CI" \
  "wave-trivial,good first issue,100pts" <<'EOF'
## Summary

The contract is `no_std` but still compiles a large dependency tree
(`soroban-sdk` and friends). Add an advisory scan so a known-vulnerable
dependency cannot ship unnoticed.

## Where

- `.github/workflows/ci.yml`
- `CONTRIBUTING.md` (document how to run it locally)

## Acceptance criteria

- [ ] A new CI job (or a step in an existing job) runs `cargo audit` (or
      `cargo deny check advisories`) against `Cargo.lock`.
- [ ] The job is named so it can be added to branch protection.
- [ ] `CONTRIBUTING.md` shows the exact local command.
- [ ] If the scan currently reports advisories, either fix them or record an
      explicit, justified ignore entry in the config file — do not silently
      skip the check.
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes

## Out of scope

- Upgrading `soroban-sdk` itself.

## Pointers

- https://github.com/rustsec/rustsec
EOF

create_issue trivial \
  "[trivial] Cover the read-only views with unit tests" \
  "wave-trivial,good first issue,100pts" <<'EOF'
## Summary

`remaining_calls` and `locked_balance` encode protocol semantics (what a
consumer may still use, and what is still owed) but have no dedicated tests.
Pin their behaviour across every lease state.

## Where

- `contracts/registry/src/test.rs`
- Views under test: `remaining_calls`, `locked_balance`, `service_count`,
  `lease_count`, `lease_status`

## Acceptance criteria

- [ ] `remaining_calls` is asserted to equal `allocated_calls` immediately
      after `create_lease`.
- [ ] `remaining_calls` returns `0` at and after `expires_at`.
- [ ] `remaining_calls` returns `0` after `settle_lease` and after
      `revoke_expired_lease`.
- [ ] `locked_balance` equals `locked_deposit` while active and `0` once
      settled or revoked.
- [ ] `service_count` / `lease_count` are asserted after each kind of mutation.
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes

## Out of scope

- Changing any view's semantics.

## Pointers

- See the existing tests in `contracts/registry/src/test.rs` for the harness
  (`RATE`, `FUNDING`, `env.ledger().set_timestamp`).
EOF

# ===========================================================================
# Medium — 150 pts
# ===========================================================================

create_issue medium \
  "[medium] Emit structured events for the service and lease lifecycle" \
  "wave-medium,150pts" <<'EOF'
## Summary

The registry mutates escrow state without emitting a single event. Providers,
consumers, indexers and explorers therefore have no cheap way to observe
`create_lease`, `settle_lease` or `revoke_expired_lease`. Add Soroban contract
events so the whole lifecycle is observable off-chain.

## Context

There is currently no `#[contractevent]` (or `env.events().publish`) anywhere in
`contracts/registry`. Everything must be discovered by polling storage.

## Proposed approach

1. Define event payloads alongside `types.rs` (or a new `events.rs`).
2. Emit on: `register_service`, `set_service_active`, `create_lease`,
   `settle_lease`, `revoke_expired_lease`.
3. Include the ids and the money fields (deposit, payout, refund) so a
   subscriber never needs a second RPC read for the common case.
4. Update `README.md` with the event list and payload shapes.

## Acceptance criteria

- [ ] Every state-changing entry point emits at least one event.
- [ ] Events carry `service_id` / `lease_id` and the relevant addresses.
- [ ] `settle_lease` emits the `payout` and `refund` amounts.
- [ ] A test asserts each event is emitted, using the testutils event API.
- [ ] `README.md` documents the event names and payloads.
- [ ] New behaviour is covered by unit tests (happy path + failure path)
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] `cargo build --target wasm32v1-none --release --all` succeeds

## Security considerations

Events must never include secret material. The `endpoint_hash` is intentionally
a commitment, not a credential — do not change that.

## Out of scope

- A hosted indexer.

## Pointers

- https://developers.stellar.org/docs/learn/encyclopedia/contract-development/events
EOF

create_issue medium \
  "[medium] Let providers update a service's rate and endpoint commitment" \
  "wave-medium,150pts" <<'EOF'
## Summary

Once `register_service` is called, a provider can only toggle `is_active`. It
cannot change `rate_per_call` or `endpoint_hash` without abandoning the service
and registering a new one, which loses the id that consumers already reference.
Add an owner-only update entry point.

## Context

`set_service_active` is the only mutable service entry point. `register_service`
sets the rate once, and `service_id`s are monotonic and never reused.

## Proposed approach

1. Add `update_service(provider, service_id, rate_per_call, endpoint_hash)`
   (or two narrower entry points) to `contracts/registry/src/lib.rs`.
2. Authenticate `provider` and check it owns the service, as
   `set_service_active` does.
3. Reject non-positive rates with the existing `InvalidRate`.
4. Decide and document whether in-flight leases keep the old rate (they should —
   `locked_deposit` is already escrowed at the old rate) and test it.
5. Update the interface table in `README.md`.

## Acceptance criteria

- [ ] New entry point added, documented with its auth and preconditions.
- [ ] Non-owner callers revert with `Error::Unauthorized`.
- [ ] Non-positive rate reverts with `Error::InvalidRate`.
- [ ] Existing leases still settle at their original rate (test).
- [ ] New leases use the new rate (test).
- [ ] Interface table in `README.md` updated.
- [ ] New behaviour is covered by unit tests (happy path + failure path)
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] `cargo build --target wasm32v1-none --release --all` succeeds

## Security considerations

Only the owning provider may update. Do not allow a rate change to alter the
`locked_deposit` of an existing lease — that would let a provider retroactively
reprice escrowed funds.

## Out of scope

- A time-locked / two-step rate change.
EOF

create_issue medium \
  "[medium] Support provider key rotation by transferring a service" \
  "wave-medium,150pts" <<'EOF'
## Summary

A service is permanently bound to the address that registered it. If a provider
rotates or loses that key, the service can never be settled again and every
consumer's escrow is stuck until the 24h revoke window. Add an owner-only
transfer so the provider role can move.

## Context

`Service.provider` is set once in `register_service` and never mutated.
`settle_lease` checks `service.provider == provider`, so a rotated key cannot
settle any in-flight lease.

## Proposed approach

1. Add `transfer_service(provider, service_id, new_provider)`.
2. Require auth from the current owner; reject `new_provider == current`.
3. Document how in-flight leases behave (the new provider becomes the only
   party able to settle them).
4. Update the interface table in `README.md`.

## Acceptance criteria

- [ ] New entry point added and documented.
- [ ] Only the current owner can transfer (`Error::Unauthorized` otherwise).
- [ ] After transfer, the old address can no longer settle or deactivate.
- [ ] After transfer, the new address can settle existing leases (test).
- [ ] Interface table in `README.md` updated.
- [ ] New behaviour is covered by unit tests (happy path + failure path)
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] `cargo build --target wasm32v1-none --release --all` succeeds

## Security considerations

A transfer must be atomic and authorised by the current provider only — there
must be no path where a third party can seize a service, and no window where
neither party can settle.

## Out of scope

- Multi-sig provider accounts (achievable via Stellar account auth today).
EOF

# ===========================================================================
# High — 200 pts
# ===========================================================================

create_issue high \
  "[high] Harden the money paths against re-entrant SEP-41 tokens" \
  "wave-high,200pts,security" <<'EOF'
## Summary

`settle_lease` performs the token transfers **before** it persists
`lease.settled = true`. The documented invariant ("No double settlement ...
`settled` is set before any payout is recorded as complete") does not match the
implementation, so a malicious or upgradeable escrow token could re-enter
`settle_lease` for the same lease and be paid twice.

## Motivation

The escrow token is admin-configurable via `set_token`, so the registry's safety
must not assume the token is honest. This is the single highest-value threat to
escrowed funds in the protocol.

## Design requirements

- Follow checks-effects-interactions: write `used_calls` and `settled`, and
  persist them, before any external `transfer` call.
- The same audit must be applied to `create_lease` and `revoke_expired_lease`.
- Must not break the existing public entry points in `contracts/registry`.
- Must not introduce unbounded work per ledger entry.

## Threat model / invariants

- Invariant: no lease's total payout + refund can exceed `locked_deposit`.
- Invariant: `settled` is observably true on-chain before any value leaves the
  contract.
- Adversary: a token contract whose `transfer` re-enters `settle_lease` /
  `revoke_expired_lease` for the same `lease_id`.

## Acceptance criteria

- [ ] Design note approved by a maintainer before implementation begins
- [ ] State is persisted before any external call in `settle_lease`,
      `create_lease`, and `revoke_expired_lease`
- [ ] An adversarial test uses a malicious token that re-enters on `transfer`
      and asserts the second attempt reverts (`LeaseAlreadySettled`)
- [ ] A conservation assertion: `payout + refund == locked_deposit`
- [ ] `README.md` security-model wording matches the implementation
- [ ] Adversarial tests cover each listed invariant
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] `cargo build --target wasm32v1-none --release --all` succeeds

## Deliverables

- [ ] Contract changes
- [ ] Tests
- [ ] Documentation update (`README.md` / module docs)
EOF

create_issue high \
  "[high] Fuzz the escrow invariant: value can never be created or destroyed" \
  "wave-high,200pts,security" <<'EOF'
## Summary

The money paths are covered by examples and a few adversarial unit tests, but
not by randomised or exhaustive exploration. Add property-based tests that
assert the protocol's conservation law over arbitrary sequences of operations.

## Motivation

Escrow bugs hide at boundaries: `verified_calls == allocated_calls`,
`now == expires_at`, `now == expires_at + SETTLE_GRACE_PERIOD_SECS`,
`u32::MAX` calls, `i128`-scale rates. Randomised operation sequences reach
combinations hand-written tests do not.

## Design requirements

- Assert the conservation law after **every** operation.
- Cover interleavings of two consumers, two services and one provider.
- Must not break the existing public entry points in `contracts/registry`.
- Must not introduce unbounded work per ledger entry (tests only).

## Threat model / invariants

- Invariant: for every lease, `payout + refund == locked_deposit`.
- Invariant: the registry's token balance equals the sum of unsettled
  `locked_deposit`s at all times.
- Invariant: a settled lease can never transition again.
- Adversary: an actor that settles, revokes, double-settles, over-claims, or
  advances ledger time adversarially.

## Acceptance criteria

- [ ] Design note approved by a maintainer before implementation begins
- [ ] A property test (e.g. `proptest` as a dev-dependency) drives randomised
      call sequences against the registry
- [ ] The two invariants above are asserted after every step
- [ ] A minimal failing case is persisted as a regression test
- [ ] Adversarial tests cover each listed invariant
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] `cargo build --target wasm32v1-none --release --all` succeeds

## Deliverables

- [ ] Contract changes (fixes for anything the fuzzer finds)
- [ ] Tests
- [ ] Documentation update (`README.md` / module docs)
EOF

create_issue high \
  "[high] Prove lease and service storage survive archival (TTL) under load" \
  "wave-high,200pts,security" <<'EOF'
## Summary

Leases and services live in **persistent** storage with a TTL bump of
`LEDGER_BUMP_EXTEND = 518_400` ledgers (~30 days). A lease may legitimately
outlive that window — a long lease plus the 24h grace period — and if its entry
is evicted while funds are still locked, the consumer's refund path breaks.
Establish the real limits and make them safe.

## Motivation

`get_lease` re-extends the TTL on every read, so an *active* lease stays alive.
But a lease that nobody touches between creation and (expiry + grace) can fall
off the ledger. Today that possibility is untested and undocumented.

## Design requirements

- Provide a documented bound on the maximum safe `duration_secs` (or a TTL
  policy that removes the bound).
- Prove it with tests that advance the ledger sequence past
  `LEDGER_BUMP_EXTEND`.
- Must not break the existing public entry points in `contracts/registry`.
- Must not introduce unbounded work per ledger entry.

## Threat model / invariants

- Invariant: for any lease created through the public API, either the provider
  can settle it or the consumer can revoke it, for its entire lifetime.
- Adversary: a griefing actor who creates a lease with a very long duration and
  then never touches it, and a provider who lets a lease go untouched.

## Acceptance criteria

- [ ] Design note approved by a maintainer before implementation begins
- [ ] A test advances `env.ledger().set_sequence_number(...)` past the extend
      window and asserts the lease is still readable (or documents the failure
      and fixes it)
- [ ] `create_lease` either rejects durations that cannot be supported, with a
      clear error, or its TTL policy guarantees them
- [ ] The TTL policy and its limits are documented in `storage.rs` and
      `README.md`
- [ ] Adversarial tests cover each listed invariant
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] `cargo build --target wasm32v1-none --release --all` succeeds

## Deliverables

- [ ] Contract changes
- [ ] Tests
- [ ] Documentation update (`README.md` / module docs)
EOF

echo
if [ "$DRY_RUN" = "1" ]; then
  echo "dry run: would create $created issue(s), $skipped already exist"
else
  echo "done: created $created issue(s), skipped $skipped existing"
fi
