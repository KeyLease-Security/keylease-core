# Design note: early consumer cancellation with a defined refund

- **Issue:** #135 — *Support early consumer cancellation with a defined refund*
- **Status:** Implemented on `fix/135-early-consumer-cancellation`; a maintainer
  must approve this note before the change is merged.
- **Scope:** `contracts/registry/src/{lib,types,storage,events,errors}.rs`, tests
  and docs.

## Problem

The only consumer exit on main is `revoke_expired_lease`: it requires the lease to
have expired **and** the full 24-hour provider grace window to elapse. A consumer
that stops using a service early — or a provider that agrees the remaining calls
will never be served — has to wait out that whole window before any funds move.

## Goals

1. Let a consumer end a lease early and receive the value of its unused calls,
   with the provider's consent.
2. Define the refund exactly, with checked arithmetic and no rounding ambiguity.
3. Never let the consumer take funds the provider has earned: consent is the
   provider's, and it includes the served-call count.
4. Keep the existing unconditional revoke path unchanged as the fallback when the
   provider is unresponsive.
5. Preserve single-release accounting: a cancelled lease cannot be settled or
   revoked again.

## Non-goals

- A unilateral consumer exit with a penalty. The issue allows either provider
  consent or a configurable penalty; consent is the smaller, safer change because
  it needs no new pricing parameter, no penalty treasury, and no assumption about
  what a fair penalty is.
- Partial cancellation, splitting a lease, or transferring unused calls to a
  different service.
- Changing the post-expiry revoke path.

## Design

### Flow

```text
consumer                      registry                       provider
   |-- request_early_cancel ---->|                               |
   |                             |-- lease_cancellation_requested ->|
   |                             |                               |
   |                             |<-- reject_early_cancel --------|
   |                             |    (request cleared)          |
   |                             |                               |
   |                             |<-- approve_early_cancel(used) -|
   |<-- refund unused calls -----|---- payout served calls ------>|
   |                             |-- lease_cancelled ----------->|
```

A request is a signal, not a state change to the lease: the lease stays active
and usable while the provider decides.

### Entry points

```rust
pub fn request_early_cancel(env: Env, consumer: Address, lease_id: u64) -> Result<(), Error>
pub fn withdraw_early_cancel(env: Env, consumer: Address, lease_id: u64) -> Result<(), Error>
pub fn reject_early_cancel(env: Env, provider: Address, lease_id: u64) -> Result<(), Error>
pub fn approve_early_cancel(env: Env, provider: Address, lease_id: u64, verified_calls: u32) -> Result<(), Error>
```

- **Request** (consumer auth): lease exists, caller is `lease.consumer`, not
  settled, `now < expires_at`, and no request is already pending.
- **Withdraw** (consumer auth): clears a pending request. The consumer may
  request again later.
- **Reject** (provider auth, owns the lease's service): clears a pending request.
  The lease is untouched and keeps running.
- **Approve** (provider auth, owns the lease's service): requires a pending
  request and `verified_calls ≤ allocated_calls`, then pays out and settles the
  lease in one transaction.

### Refund definition

Let `rate = service.rate_per_call`, `unused_calls = allocated_calls −
verified_calls`, `payout = rate × verified_calls`, and `refund = rate ×
unused_calls`.

- `verified_calls ≤ allocated_calls` is checked before the subtraction, and both
  multiplications use `checked_mul`.
- `payout + refund` is asserted to equal `locked_deposit` with `checked_add`; a
  mismatch (only possible if the stored rate no longer matches the deposit
  arithmetic) reverts with `Overflow` instead of releasing an inconsistent split.
- **Rounding:** there is none. `rate_per_call` is an integer price in the token's
  smallest unit and calls are whole units, so both the payout and the refund are
  exact integers. No division, basis points, or fee is applied. If a future
  version adds a penalty, it should be expressed in basis points with an explicit
  rounding rule; this design deliberately avoids that surface.

The refund is sent to `lease.consumer` and the payout to the service provider in
the same transaction. The lease is marked settled **before** either transfer
(checks-effects-interactions), so a re-entrant token cannot release it twice.

### Authorization and refusal

Only the current consumer can request or withdraw; only the owning provider can
approve or reject. A provider refusal is an explicit on-chain event, and the
consumer still has every existing path: keep using the lease, settle normally, or
wait for expiry + grace and revoke.

### Edge cases

| Situation | Result |
| --- | --- |
| Request after `expires_at` | `LeaseExpired`; use the revoke path after grace |
| Request when settled or revoked | `LeaseAlreadySettled` |
| Request twice | `CancellationAlreadyRequested` |
| Approve/reject without a request | `CancellationNotRequested` |
| Approve with `verified_calls > allocated_calls` | `InvalidCalls` |
| Approve after expiry | allowed, if the request was filed before expiry |
| Settle after approval | `LeaseAlreadySettled` |
| Revoke after approval | `LeaseAlreadySettled` |

Approval is allowed after expiry so a request filed in the last seconds of the
lease is not lost; the funds are still exactly the unused-call value.

### Storage

A new persistent entry, keyed by lease id, holds only the pending request:

```rust
DataKey::CancelRequest(u64)  // persistent
```

`CancelRequest { lease_id, requested_at }` is extended to the standard ~30-day
persistent TTL. The `Lease` layout is unchanged, so the upgrade is additive. The
request is removed on withdraw, reject, approve, or settlement; a stale request
carries no funds and is harmless.

### Events

| Event | Topics | Payload |
| --- | --- | --- |
| `lease_cancellation_requested` | `lease_id` | `schema_version`, `consumer` |
| `lease_cancellation_rejected` | `lease_id` | `schema_version`, `provider` |
| `lease_cancellation_withdrawn` | `lease_id` | `schema_version`, `consumer` |
| `lease_cancelled` | `lease_id` | `schema_version`, `provider`, `consumer`, `verified_calls`, `payout`, `refund` |

`lease_cancelled` is the money-moving event, so indexers can treat it like
`lease_settled` for reconciliation.

### Errors (appended, never renumbered)

| Code | Variant | Meaning |
| --- | --- | --- |
| 25 | `CancellationNotRequested` | no pending request |
| 26 | `CancellationAlreadyRequested` | a request is already pending |
| 27 | `LeaseExpired` | early cancellation is only for active leases |

## Safety analysis

- **Provider consent is required.** No unilateral path reduces the provider's
  payout; the provider signs the served-call count.
- **Conservation.** `payout + refund == locked_deposit` is enforced before any
  transfer.
- **Single release.** Approval sets `settled = true` before transfers, and both
  `settle_lease` and `revoke_expired_lease` reject a settled lease.
- **No expired-lease shortcut.** A request cannot be filed after expiry, so the
  consumer cannot use approval to bypass the provider's 24-hour grace window at
  the last moment. A request filed before expiry may be approved after it; the
  provider is the one choosing that timing.
- **No grief via pending requests.** The consumer can withdraw or request again,
  but a pending request never blocks use, settlement, or revocation. The provider
  can reject at any time.
- **Stale requests.** Requests are cleared on every terminal path; a request with
  no lease value left cannot move funds because approval re-reads the lease.

## Alternatives considered

1. **Unilateral cancel with a configurable penalty.** Would let a consumer exit
   without the provider, but requires a penalty parameter (per service or
   protocol-wide), a rounding rule, and a policy for where the penalty goes. It
   also weakens the provider's guarantee. Rejected for this change; the design
   note records it as future work.
2. **Consumer-specified refund amount / negotiation.** Adds a bid field and an
   acceptance rule, with no on-chain way to judge fairness. Rejected.
3. **Approve with no served-call report (full refund always).** Simple, but the
   provider could not be paid for calls already served. Rejected.
4. **Reuse `settle_lease` before expiry.** That would be a unilateral provider
   action with a different authorization story; keeping a separate consented
   path makes the intent and events explicit.

## Interaction with other protocol changes

- **Consumer challenge window (#132):** on that branch a settlement proposal sets
  `settled = true`, so `request_early_cancel` fails with `LeaseAlreadySettled`
  once a settlement is proposed. That is correct: a provider that has already
  reported usage should use the settlement flow, not early cancellation.
- **Transferable leases (#134):** the new consumer is `lease.consumer`, so only
  they can request or withdraw a cancellation. Ownership moves the right to
  cancel with the rest of the lease.

## Test matrix

Covered in `contracts/registry/src/test.rs` (48 tests total):

- consent path: request, approve with 4/10 calls served, correct payout/refund,
  lease settled and locked balance zero,
- conservation at both extremes (0 served → full refund; all served → no refund),
- refusal: request cleared, no funds release, lease still settles normally,
- ownership/authorization for request, withdraw, approve and reject, double
  request, approve without request, non-provider approval,
- cancellation after a partial settlement is rejected,
- request at expiry is rejected; a pre-expiry request approved after expiry works,
- events for request, reject, withdraw and approval.

## Rollout

- Additive `DataKey`; `Lease` and existing entries are unchanged. No migration.
- Indexers should add the four `lease_cancellation*` / `lease_cancelled` events.
  `lease_cancelled` carries the payout and refund for accounting.
- Consumers and providers need one extra transaction each compared with the
  post-expiry revoke path, but avoid the wait.
