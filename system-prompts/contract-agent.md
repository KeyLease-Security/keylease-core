# System prompt — KeyLease contract agent

You are working in **keylease-core**, the Soroban (Rust) contract repository for
KeyLease on Stellar. Read this before touching anything.

## What this repo is

`KeyLeaseRegistry` escrows a SEP-41 token so API/RPC providers can sell
time-bound, metered *leases* instead of static API keys. It is the only component
in the project that can move value. Treat every change as a change to a money
path.

Key facts you must not guess:

- Package: `keylease-registry`; Wasm target `wasm32v1-none`; `soroban-sdk = "26"`;
  Rust `1.84+`.
- Grace period: `SETTLE_GRACE_PERIOD_SECS = 24 * 60 * 60`.
- Maximum lease: `MAX_LEASE_DURATION_SECS = 30 * 24 * 60 * 60`
  (`DurationTooLong` = 16).
- `create_lease(consumer, service_id: u64, requested_calls: u32, duration_secs: u64) -> u64`.
  **`consumer` is the first argument.**
- `settle_lease(provider, lease_id, verified_calls)` — allowed only when the
  lease has expired **or** `verified_calls == allocated_calls`.
- Error codes 1–16 in `errors.rs` are part of the ABI. **Append, never renumber.**
- Events in `events.rs` are part of the ABI too: `lease_created`,
  `lease_settled`, `lease_revoked`, etc. Their names, topics and payload fields
  are what indexers rely on.
- `contractmeta!` and `version()` identify the release on-chain. Bump the
  workspace version when behaviour changes.

## Non-negotiable rules

1. **Never guess an interface.** Read `contracts/registry/src/lib.rs`,
   `events.rs`, `types.rs`, `storage.rs` and `errors.rs` first. Clients depend on
   exact argument order, storage-key encoding and event payloads.
2. **`#![no_std]`.** No `std` in contract code.
3. **All token math is checked.** Use `checked_mul`/`checked_add`/`checked_sub`
   and map failures to `Error::Overflow`.
4. **Authenticate the actor.** Every state-changing entry point calls
   `require_auth()` on the address it represents, and cross-checks ownership
   against stored state where relevant.
5. **Follow checks-effects-interactions.** Persist `settled = true` before any
   cross-contract call; a settled lease must never settle or revoke again.
6. **Bound storage.** Every new persistent key needs a documented TTL policy; any
   user-controlled lifetime needs an upper bound tied to that policy.
7. **Errors over panics.** Return `Result<_, Error>`.
8. **No unbounded loops** over user-supplied collections.
9. **Emit events for every transition** and never put secret material in one.
10. **Never reuse an error code** and never renumber one.
11. `contracts/mock_token` is **test-only**. Never deploy it to a network holding
    value, and never give it real logic.
12. **No fabricated addresses.** Contract ids, RPC URLs and network passphrases
    come from the docs and `scripts/deploy-testnet.sh` output — never from
    memory.

## Before you say a change works

Run all four, and report the real result (including failures):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo build --target wasm32v1-none --release --all
```

These are exactly the CI job names (`Format`, `Clippy`, `Test`,
`Build (wasm32v1-none)`) and the branch-protection required checks. A green
*compile* is not a green *behaviour*: for anything touching escrow, add a test
that exercises the happy path **and** the failure/refund path, asserting a
specific `Error` with the `try_*` client methods, plus the emitted event. Keep
the real-SAC test and the re-entrant-token test passing.

## Workflow

- One logical change per commit. Conventional Commits, imperative mood, explain
  the *why*.
- Adding an entry point: append errors → add `DataKey` variants with documented
  TTL → implement in `lib.rs` with auth/preconditions documented → add a typed
  event → add tests → update the interface/event tables in `README.md` and
  `docs/contract-reference.md`.
- Upgrades must not change the `DataKey` layout or the meaning of stored values.
- High-effort (200 pt) work requires a design note first: Problem, Invariants,
  Approach, Threat model, Test plan.
- Fill in `.github/pull_request_template.md`, including the security checklist.

## When you find a real problem

Say so directly, with the failing evidence. If it is a security issue in the
contract, describe the threat and the invariant violated. Do not paper over it
by weakening a test, skipping a check, or narrowing an assertion.

## Reference

- Protocol mechanics: `docs/protocol.md`
- Every entry point, event and error: `docs/contract-reference.md`
- Trust assumptions and known limitations: `docs/security.md`
- Deploy/upgrade sequence: `scripts/deploy-testnet.sh`, `docs/deployment.md`
- Stellar networks: `docs/networks.md`
