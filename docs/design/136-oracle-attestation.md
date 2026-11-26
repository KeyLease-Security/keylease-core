# Design note — Optional oracle attestation for metered usage (#136)

> **Status:** Proposed — **awaiting maintainer approval**. No contract code is
> changed by this note. Per [CONTRIBUTING](https://github.com/KeyLease-Security/keylease-core/blob/main/CONTRIBUTING.md),
> a maintainer must approve this design before implementation begins.
>
> **Issue:** [#136 — Add optional oracle attestation for metered usage](https://github.com/KeyLease-Security/keylease-core/issues/136)
> **Target release:** v0.3.0 · **Touches:** `lib.rs`, `types.rs`, `storage.rs`,
> `events.rs`, `errors.rs`, `test.rs`, `docs/security.md`

## 1. Problem statement

`settle_lease(provider, lease_id, verified_calls)` trusts one number supplied by
the provider. The allotment bounds the damage — a provider can never be paid for
more than `allocated_calls` — but within that bound the provider is the only
witness to usage. A consumer that was served 120 of 500 calls and a provider
that reports 400 cannot be distinguished on-chain, and the consumer's only
recourse is to stop using the service.

Some providers and consumers already operate an independent usage oracle: a
gateway, an RPC operator, a metering service or a co-signed receipt log. This
note proposes an **optional per-service attestor** that must approve a
settlement before it pays out. A service that never configures one keeps the
current trust model and the current `settle_lease` path exactly.

### Goals

- Let a service bind settlement to an independent attestation of usage.
- Define a small, explicit attestor interface rather than one signature scheme.
- Bind every attestation to the registry deployment, lease id, service id and
  exact call count, so it cannot be replayed or retargeted.
- Make the opt-in **per lease at creation time**, so a provider cannot silently
  drop the requirement for an in-flight lease.
- Reject a missing, stale, mismatched or otherwise invalid attestation without
  touching escrow.
- Keep the consumer's refund and revocation path completely unchanged.
- Add no unbounded loops, no new trusted key inside the registry, and no change
  to the `Service` or `Lease` stored encoding.

### Non-goals

- Proving that an HTTP request actually happened. The registry cannot observe
  traffic; it enforces *whose* answer is required, not that the answer is true.
- Making attestation mandatory, protocol-wide, or retroactive to existing
  leases.
- Verifying one specific signature algorithm inside the registry. An attestor is
  a contract, so it may use ed25519, secp256k1, a quorum, its own storage or
  anything else.
- Multiple attestors, threshold policies or per-call attestations. A single
  attestor per lease is the smallest useful unit.
- A dispute process for a lying attestor. That is #137 (staking and slashing)
  territory.

## 2. Invariants

These must hold after the change, in both attestor and no-attestor modes:

1. `payout + refund == locked_deposit` for every settlement.
2. `settled` is persisted before any token transfer, and a settled lease never
   transitions again.
3. A settlement pays at most `allocated_calls × rate_per_call`.
4. **No attestation, no payout** for a lease created on an opted-in service.
5. The consumer's right to revoke after `expires_at + 24h` is never gated by an
   attestor, an attestor outage or an attestor configuration change.
6. An attestation is accepted only for the lease, service and call count it was
   created for, in this registry deployment, inside its validity window.
7. A lease's attestation requirement is fixed when the lease is created. Later
   provider configuration changes affect new leases only.
8. The no-attestor mode is byte-for-byte the current behaviour: same entry
   point, same event, same errors, same storage shape.

## 3. Proposed architecture

### 3.1 The attestor interface

An attestor is a Soroban contract address. The registry expects exactly one
method, declared next to the other contract types:

```rust
/// Payload the registry asks an attestor to approve at settlement time.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageAttestation {
    /// Registry deployment this attestation is valid for.
    pub registry: Address,
    /// Lease whose usage is being attested.
    pub lease_id: u64,
    /// Service the lease belongs to.
    pub service_id: u64,
    /// Calls the provider reports as consumed.
    pub verified_calls: u32,
    /// Ledger timestamp the attestor observed the usage.
    pub issued_at: u64,
    /// Ledger timestamp after which the attestation is no longer valid.
    pub valid_until: u64,
}

/// The interface an attestor contract must implement.
#[soroban_sdk::contractclient(name = "UsageAttestorClient")]
pub trait UsageAttestor {
    /// Return `true` only if `payload` is a genuine attestation for this
    /// registry. Implementations must check `payload.registry` (or the calling
    /// contract) and reject payloads they did not produce.
    fn attest(env: Env, payload: UsageAttestation) -> bool;
}
```

The interface is deliberately thin:

- The registry owns the **schema** of what is attested. An attestor cannot
  redefine what `verified_calls` means.
- The attestor owns the **proof**. It may verify an ed25519 signature, read its
  own stored observation, check a quorum, or return `false` unconditionally
  during an incident.
- The registry never verifies a signature itself. Soroban's
  `env.crypto().ed25519_verify` aborts the invocation on an invalid signature,
  which would turn a malformed attestation into an opaque host error rather than
  the documented `InvalidAttestation`. Keeping verification in the attestor
  gives callers a typed result and lets a provider rotate or replace the
  attestor's internals without a registry upgrade.

An attestor is configured per service:

```rust
/// Attestation requirement snapshotted for new leases of a service.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationConfig {
    /// Contract that must approve settlements for leases created under it.
    pub attestor: Address,
    /// Maximum accepted age of an attestation, in seconds.
    pub max_age_secs: u64,
}
```

### 3.2 Opt-in and per-lease snapshot

The requirement lives in two additive persistent keys:

| Key | Written by | Read by | Meaning |
| --- | --- | --- | --- |
| `ServiceAttestor(service_id)` | `set_service_attestor` | `create_lease`, `get_service_attestor` | Template for **new** leases. Absent means the legacy trust model. |
| `LeaseAttestor(lease_id)` | `create_lease` | both settlement paths, `get_lease_attestor` | Immutable requirement for **this** lease. |

`create_lease` reads `ServiceAttestor(service_id)` after the service is loaded.
If it exists, it writes the same `AttestationConfig` into
`LeaseAttestor(lease_id)` in the same transaction that writes the lease. If it
does not, no per-lease key is written and the lease is a legacy lease.

Why snapshot instead of reading the service config at settlement time:

- A provider cannot opt out of attestation for leases it has already sold.
- A consumer can read `get_lease_attestor(lease_id)` immediately after
  `create_lease` and know exactly which attestor must approve the payout.
- Clearing or replacing a service's attestor only changes the requirement for
  future leases, so it cannot strand a lease whose attestor has since been
  removed.
- Existing `Lease` and `Service` values do not change, so an upgrade does not
  rewrite live entries.

The per-lease entry costs one write per opted-in lease and is removed when the
lease settles or is revoked.

### 3.3 Settlement paths

The existing entry point keeps its exact signature:

```rust
pub fn settle_lease(env: Env, provider: Address, lease_id: u64, verified_calls: u32)
    -> Result<(), Error>
```

It gains one check, after the lease and service have been loaded and before any
state is written:

```text
if LeaseAttestor(lease_id) exists:
    return Err(AttestationRequired)
```

Everything else in `settle_lease` is unchanged, so every service without an
opt-in behaves exactly as today.

The attested path is a **new** entry point, not a new argument on the old one:

```rust
pub fn settle_lease_attested(
    env: Env,
    provider: Address,
    attestation: UsageAttestation,
) -> Result<(), Error>
```

Adding an `Option<UsageAttestation>` argument to `settle_lease` would break the
ABI for every existing client and indexer; a new function is additive.

`settle_lease_attested` performs, in order:

1. `provider.require_auth()`.
2. Reject `attestation.registry != env.current_contract_address()` with
   `AttestationMismatch`.
3. Load the lease by `attestation.lease_id`; reject if settled
   (`LeaseAlreadySettled`) or missing (`LeaseNotFound`).
4. Load `LeaseAttestor(lease_id)`; if absent, reject with
   `AttestationNotConfigured` (the caller used the wrong entry point).
5. Reject `attestation.service_id != lease.service_id` with
   `AttestationMismatch`.
6. Reject `attestation.verified_calls > lease.allocated_calls` with
   `InvalidCalls`.
7. Load the service and require `service.provider == provider` (`Unauthorized`).
8. Apply the unchanged settleability rule: the lease must be expired or
   `verified_calls == allocated_calls` (`LeaseNotSettleable`).
9. Validate freshness against `config.max_age_secs`:
   - `now >= issued_at`, otherwise `AttestationStale`;
   - `now - issued_at <= max_age_secs`, otherwise `AttestationStale`;
   - `valid_until > issued_at` and `now < valid_until`, otherwise
     `AttestationStale`.
10. Compute `payout` and `refund` with the same checked arithmetic.
11. Call `UsageAttestorClient::new(&env, &config.attestor).attest(&attestation)`.
    A `false` return is `InvalidAttestation`. A revert inside the attestor
    reverts the whole settlement; the registry does not retry or ignore it.
12. Persist `used_calls` and `settled = true`, remove `LeaseAttestor(lease_id)`,
    then perform the payout and refund transfers.
13. Publish the unchanged `lease_settled` event plus a new `lease_attested`
    event.

Step 11 happens **after** every registry check and **before** any registry
state write or token call. The attestor is untrusted code, but at that point it
can only return a boolean or revert. It cannot observe a half-written
settlement, and a re-entrant call into the registry fails because the nested
invocation has no provider authorization. The token transfers still happen only
after `settled = true` is persisted, so the re-entrant-token guarantee is
unchanged.

`revoke_expired_lease` is untouched except that it also removes
`LeaseAttestor(lease_id)` when it refunds. It never reads or calls the attestor,
so a dead or hostile attestor cannot block the consumer's exit.

### 3.4 Configuring and reading the requirement

```rust
pub fn set_service_attestor(
    env: Env,
    provider: Address,
    service_id: u64,
    attestor: Address,
    max_age_secs: u64,
) -> Result<(), Error>

pub fn clear_service_attestor(
    env: Env,
    provider: Address,
    service_id: u64,
) -> Result<(), Error>

pub fn get_service_attestor(env: Env, service_id: u64) -> Option<AttestationConfig>
pub fn get_lease_attestor(env: Env, lease_id: u64) -> Option<AttestationConfig>
```

- Only `service.provider` may set or clear, after `require_auth()` and the same
  ownership check as `set_service_active` (`Unauthorized`).
- `attestor` must not be the registry itself, otherwise settlement would
  recurse into `attest`; this is `InvalidAttestationConfig`.
- `max_age_secs` must be in `MIN_ATTESTATION_MAX_AGE_SECS` (`60`) to
  `MAX_ATTESTATION_MAX_AGE_SECS` (`7 * 24 * 60 * 60`). Zero would make every
  attestation stale, and an unbounded window defeats the staleness check.
- Setting replaces the template atomically. Clearing removes it. Neither touches
  existing `LeaseAttestor` snapshots.
- Both writers emit an event (see §5); both views are read-only and do not
  extend a TTL.

There is no admin override. The provider is the party that chooses its own
attestor, exactly as it chooses `rate_per_call`; the consumer's protection is
the snapshot and the unchanged refund path, not an admin promise.

## 4. Storage and resource changes

All keys are additive `DataKey` variants; `Service`, `Lease` and every existing
key keep their exact encoding.

| Key | Storage | Value | TTL policy |
| --- | --- | --- | --- |
| `ServiceAttestor(u64)` | persistent | `AttestationConfig` | Extended when `create_lease` snapshots it; 30-day horizon, matching `Service`. |
| `LeaseAttestor(u64)` | persistent | `AttestationConfig` | Extended with the lease: term + 24h grace + 1-day buffer, clamped to `MAX_ENTRY_TTL_LEDGERS` and the network `maxEntryTtl`. Removed on settle/revoke. |

- `settle_lease` gains one persistent read (`LeaseAttestor`) and one delete on
  the attested path.
- `create_lease` gains one read and, only for opted-in services, one write plus
  a TTL extension.
- No entry point iterates a user-supplied collection; the attestor call is a
  single cross-contract call.
- `bump_lease` also extends `LeaseAttestor` when present, so a long-lived lease
  cannot lose its requirement to eviction while its escrow is still claimable.
  `bump_service` also extends `ServiceAttestor` when present.

The attestor contract's own storage is outside this contract's rent model. A
provider choosing an attestor is choosing that dependency.

## 5. Events

New events follow the existing `schema_version` convention; no existing topic or
payload changes.

| Event | Topics | Data |
| --- | --- | --- |
| `service_attestor_set` | `service_id` | `schema_version`, `provider`, `attestor`, `max_age_secs` |
| `service_attestor_cleared` | `service_id` | `schema_version`, `provider` |
| `lease_attested` | `lease_id` | `schema_version`, `service_id`, `attestor`, `verified_calls`, `issued_at`, `valid_until` |

`lease_attested` is emitted immediately after the unchanged `lease_settled`, in
the same transaction. It carries the attestor address and the exact validity
window, so an indexer can reconcile the payout with the attestation without a
second RPC read. Neither event contains secret material.

`LeaseCreated` is intentionally unchanged. The per-lease requirement is
discoverable through `get_lease_attestor(lease_id)`, and changing `LeaseCreated`
would force every indexer to handle a new payload version for a field they can
already read.

## 6. Errors (appended, never renumbered)

| Code | Variant | Meaning |
| --- | --- | --- |
| 38 | `AttestationRequired` | `settle_lease` called for a lease that requires attestation |
| 39 | `AttestationNotConfigured` | `settle_lease_attested` called for a lease with no attestor snapshot |
| 40 | `AttestationMismatch` | Payload registry, lease or service does not match the stored lease |
| 41 | `AttestationStale` | `issued_at` is in the future, older than `max_age_secs`, or `valid_until` has passed |
| 42 | `InvalidAttestation` | The attestor returned `false` |
| 43 | `InvalidAttestationConfig` | Zero/out-of-range `max_age_secs`, or the registry itself as attestor |

`17–32` are proposed for #137 and `33–37` for #133. This note appends `38–43` so
the three proposals do not collide; the final numbers are assigned when the
designs are approved and before any of them becomes ABI.

## 7. Backward compatibility

- **Default mode.** No `ServiceAttestor` key means no `LeaseAttestor` key,
  `settle_lease` unchanged, `lease_settled` unchanged, no new transfers, no new
  errors. This is the path every current service takes.
- **Existing services and leases.** Neither has the new keys, so both are
  legacy. An upgrade to a v0.3.0 Wasm does not migrate or rewrite them.
- **Existing clients.** `settle_lease` keeps its signature. A client that never
  calls `set_service_attestor` cannot observe the feature at all.
- **Existing events.** `lease_created`, `lease_settled` and `lease_revoked`
  keep their topics and payloads. The new events are additional names.
- **Opting out.** `clear_service_attestor` stops requiring attestation for
  leases created afterwards. Leases already created keep their snapshot and
  must still be settled through `settle_lease_attested`; that is the point of
  the snapshot.
- **Consumer exit.** `revoke_expired_lease` never requires an attestation, so
  opting in cannot trap escrow.

## 8. Threat model and trust assumptions

### Added trust assumption

Choosing an attestor means trusting that address's code and operator to return
`true` only for genuine usage of that lease. The registry enforces *that the
attestor approved this exact lease, service, deployment and call count, recently
and once*; it cannot enforce *that the approval is truthful*. If the provider
controls the attestor, the opt-in is an operational integrity control, not a
trustless guarantee. Consumers should treat `get_lease_attestor` as the
description of who they must trust.

Two operational assumptions come with it:

- **Liveness.** If the attestor stops answering, an opted-in lease cannot be
  settled. The escrow is not stuck: after expiry plus the 24h grace the consumer
  can still revoke in full, and the provider loses the payout it failed to
  attest. Providers should choose an attestor with an availability story and
  should not opt in leases they cannot attest.
- **Code safety.** A malicious attestor can return `false` (denying the
  provider's payout) or revert. It cannot move escrow: the registry makes no
  token call before the attestor answers, and the attestor is never approved to
  spend the registry's balance.

### Adversarial cases

| Attack / failure | Handling |
| --- | --- |
| Provider bypasses attestation with `settle_lease` | `LeaseAttestor` exists, so `AttestationRequired`. |
| Provider settles on a legacy lease through the attested path | No snapshot, so `AttestationNotConfigured`. |
| Attestation from another lease or service | `lease_id`/`service_id` are checked against stored state; `AttestationMismatch`. |
| Attestation from another registry deployment | `attestation.registry` must equal `current_contract_address`; `AttestationMismatch`. |
| Call count differs from the attested count | The exact `verified_calls` is part of the payload; the attestor must approve it. A different count fails `InvalidAttestation`, an over-count fails `InvalidCalls`. |
| Replay after settlement | `settled` is checked first; `LeaseAlreadySettled`. |
| Replay on a second lease | Lease id is bound into the payload; the attestor should reject, and the registry rejects a mismatched lease. |
| Stale or future-dated attestation | `AttestationStale` on `issued_at`, `valid_until` and `max_age_secs`. |
| Provider rotates or clears the attestor mid-lease | Snapshot is immutable; the lease keeps its original requirement. |
| Provider configures a different attestor before settlement | Ignored for existing leases; only new leases see it. |
| Malicious attestor re-enters the registry | All checks precede the call, no registry write or transfer precedes it, and a nested settlement lacks provider auth. The settlement either gets one boolean or reverts. |
| Malicious attestor reverts | Whole settlement reverts; escrow stays in place; consumer revocation still available. |
| Attestor returns `true` for a false count | Not preventable on-chain. This is the declared trust assumption, not a bug. |
| Consumer cannot exit because attestation is missing | `revoke_expired_lease` is unchanged and attestor-independent. |
| Storage eviction removes the requirement | `LeaseAttestor` inherits the lease TTL policy and is extended by `bump_lease`. |

## 9. Rejected alternatives

1. **Add `Option<UsageAttestation>` to `settle_lease`.** Breaks the published
   ABI and forces every client, CLI invocation and indexer to change even when
   no attestor exists. Rejected; a new entry point is additive.
2. **Verify an ed25519 signature directly in the registry.** The host's
   `ed25519_verify` panics on an invalid signature, so the registry cannot
   return `InvalidAttestation`; it would also hard-code one algorithm and one
   key format into the money path. A contract attestor can still verify
   ed25519 internally and return `false`.
3. **Store a public key instead of an attestor contract.** Simpler storage, but
   it makes key rotation a provider transaction that can silently change the
   meaning of an already-created lease, and it cannot express a quorum or an
   oracle that keeps its own observation log.
4. **Read the service config at settlement time (no snapshot).** Lets a provider
   remove the requirement after selling the lease. Rejected because it defeats
   the consumer-visible guarantee.
5. **Attestor as a Stellar account with `require_auth()`.** The attestor would
   have to co-sign every settlement transaction, and there is no payload to
   carry `issued_at`/`valid_until`. Operationally worse and less expressive than
   a pre-issued attestation.
6. **Add an `attestor` field to `Service` or `Lease`.** Changes the stored value
   encoding and forces a migration of every live entry. Additive keys avoid
   that.
7. **Mandatory attestation for all services.** Changes the default trust model,
   which the issue explicitly excludes.

## 10. Test plan

New tests in `contracts/registry/src/test.rs`, with a small test-only attestor
contract defined in the test module (never deployed):

```rust
// Test-only: returns a configurable verdict and records the last payload.
#[contract]
pub struct MockAttestor;
```

**Configuration and views**

- `set_service_attestor` by the provider stores the config, extends the TTL and
  emits `service_attestor_set` with the exact topics and payload.
- a non-owner, an impostor and the admin all fail with `Unauthorized`.
- `max_age_secs` of `0`, `59`, or above 7 days fails
  `InvalidAttestationConfig`; the registry address as attestor fails the same
  way.
- `clear_service_attestor` removes the template, emits
  `service_attestor_cleared`, and leaves existing lease snapshots intact.
- `get_service_attestor` / `get_lease_attestor` return `None` for legacy and
  the stored config for opted-in entries.

**Lease snapshot**

- `create_lease` on an opted-in service writes `LeaseAttestor` with the config
  in force at creation;
- a later `set_service_attestor` / `clear_service_attestor` does not change that
  lease's snapshot;
- a lease created before the opt-in has no snapshot and is legacy;
- `create_lease` on a legacy service writes no `LeaseAttestor` key.

**The four acceptance cases**

| Case | Setup | Expected |
| --- | --- | --- |
| Missing | Opted-in lease; call `settle_lease(provider, id, calls)` | `AttestationRequired`; no transfer, lease unsettled |
| Stale | `issued_at` older than `max_age_secs`, `issued_at` in the future, or `valid_until` in the past | `AttestationStale`; no transfer, no state change |
| Mismatched | Wrong `registry`, wrong `service_id`, or an attestation for another lease | `AttestationMismatch` (or the other lease, if that is the lease being settled) |
| Valid | Attestor returns `true` inside the window | Settlement succeeds: exact payout/refund, `lease_settled` and `lease_attested` emitted |

**Settlement and failure paths**

- a valid attestation settles exactly once; a second call is
  `LeaseAlreadySettled`;
- `settle_lease_attested` on a legacy lease is `AttestationNotConfigured`;
- the attestor returning `false` is `InvalidAttestation` and moves nothing;
- `verified_calls > allocated_calls` is `InvalidCalls` before the attestor is
  called;
- settling before expiry with fewer than `allocated_calls` remains
  `LeaseNotSettleable`, even with a valid attestation;
- a lease whose attestor was cleared after creation still requires its
  snapshot and settles through the attested path;
- `revoke_expired_lease` ignores the attestor and refunds in full after the
  grace window; it also removes the snapshot.

**Adversarial and Stellar-specific**

- a hostile attestor that reverts makes the settlement revert and leaves escrow
  intact;
- a hostile attestor that re-enters `settle_lease_attested` cannot settle twice
  or move funds (auth failure);
- the real-SAC lifecycle test is repeated with an attested settlement;
- the existing re-entrant-token test still proves a lease cannot settle twice.

**Invariants asserted after every settlement test**

- `payout + refund == locked_deposit`, and `locked_balance == 0` afterwards;
- the registry token balance returns to zero;
- a revoked or settled lease never transitions again;
- `LeaseAttestor` is gone after settlement or revocation.

## 11. Rollout and documentation

- Workspace version moves to `0.3.0` with the #133/#137 work; this change is
  additive and needs no migration.
- `docs/security.md` gains the new trust assumption and a known limitation:
  attestation binds *who* may report usage, not whether the usage happened, and
  a provider-controlled attestor does not remove the provider's incentive to
  over-report.
- `docs/contract-reference.md` and `README.md` gain the new entry points,
  events, error codes and views once the code lands.
- Release notes describe the opt-in, the per-lease snapshot and the liveness
  risk.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all` and
  `cargo build --target wasm32v1-none --release --all` are the acceptance gate.

## 12. Open questions

1. **Attestor bound.** One attestor per service/lease is the smallest useful
   unit. *Question for maintainers:* is a single attestor enough, or should the
   first version allow a small `Vec` with an "all must approve" rule?
2. **Default `max_age_secs`.** The note proposes provider-chosen between 60s and
   7 days with no protocol default, because the useful window depends on the
   oracle. A shorter cap (for example 24h, matching the grace period) is safer
   but constrains outage recovery.
3. **Snapshot storage cost.** One extra persistent entry per opted-in lease.
   The alternative — a `Lease` field — avoids the key but breaks the stored
   encoding; this note keeps the additive key.
4. **Event schema.** No existing payload changes, so `EVENT_SCHEMA_VERSION`
   stays at `1`. New events are new names, not new versions.
5. **Error numbering.** `38–43` assumes #133 and #137 keep their proposed
   ranges. If either changes, these move before implementation.
6. **Attestation evidence.** The payload carries no usage digest or receipt
   hash. If maintainers want off-chain verifiability, a future version can add
   a commitment to the attested usage log without changing the settlement
   semantics.

## 13. Review request

Per `CONTRIBUTING.md`, this note needs maintainer approval before code:

- Approve the **contract-attestor interface** (`attest(payload) -> bool`) over a
  registry-verified signature.
- Confirm the **per-lease snapshot** so later config changes cannot downgrade
  an in-flight lease.
- Confirm the **new entry point** (`settle_lease_attested`) rather than an
  optional argument on `settle_lease`.
- Confirm the `max_age_secs` bounds and the `38–43` error range.

Once approved, implementation lands on the same feature branch as the #133 and #137
work, with the tests above and the full four-command validation from
`CONTRIBUTING.md`.
