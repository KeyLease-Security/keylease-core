# Design note: transferable leases with consumer authorization

- **Issue:** #134 — *Make leases transferable with consumer authorization*
- **Status:** Implemented on `fix/134-transferable-leases`; a maintainer must
  approve this note before the change is merged.
- **Scope:** `contracts/registry/src/{lib,events,errors}.rs`, tests and docs. No
  storage change is required.

## Problem

A lease is bound to the `consumer` that created it. The provider authenticates
requests against `lease.consumer`, and `revoke_expired_lease` refunds that same
address. A consumer that over-bought, changed accounts, or wants to resell unused
access has no protocol-level way to hand the lease over. Off-chain key sharing is
the only workaround, and it transfers the account's identity rather than the
lease.

## Goals

1. Let the current consumer move a lease to another Stellar address.
2. Move the access right and the refund path atomically — one source of truth,
   no window where two addresses can both use or reclaim the lease.
3. Make the change observable so providers and indexers can follow it.
4. Refuse transfers that would corrupt accounting: settled/revoked leases, and
   self-transfers.
5. Stay compatible with the consumer challenge window (issue #132).

## Non-goals

- An offer/accept handshake. The issue allows assignment, and assignment is the
  smaller surface: no pending-offer state, no expiry, no cancellation.
- A marketplace, pricing, or fees. The protocol only moves ownership.
- Splitting a lease across consumers.

## Design

### Entry point

```rust
pub fn transfer_lease(
    env: Env,
    consumer: Address,
    lease_id: u64,
    new_consumer: Address,
) -> Result<(), Error>
```

- **Auth:** only the current `consumer`.
- **Preconditions:** the lease exists, `lease.consumer == consumer`,
  `!lease.settled`, and `new_consumer != consumer`.
- **Effect:** sets `lease.consumer = new_consumer` and saves the lease once.
  There is no second copy of ownership to keep in sync.
- **Event:** `lease_transferred { lease_id | schema_version, from, to }`.

The new consumer does not authorize the assignment. Being given a lease is
receiving an asset; requiring the recipient's signature would let a typo or a
lost key strand the lease, and it would need a pending-offer state to avoid
griefing. Assignment is final and the new consumer can immediately use the lease
or transfer it again.

### Why no storage change

`Lease.consumer` is already the single field every path reads:

- `revoke_expired_lease` checks and refunds `lease.consumer`,
- `settle_lease` refunds `lease.consumer`,
- providers authenticate callers against `lease.consumer`,
- `remaining_calls`, `lease_status` and `locked_balance` are ownership-agnostic.

Updating that one field is the atomic move the acceptance criteria ask for. A
`DataKey` index would add a second source of truth that could drift.

### Errors (appended, never renumbered)

| Code | Variant | Meaning |
| --- | --- | --- |
| 35 | `LeaseNotTransferable` | lease is settled or revoked |
| 36 | `InvalidConsumer` | `new_consumer` equals the current consumer |

`Unauthorized` still covers a caller that is not the current consumer.

### Expired but unsettled leases

Transfer is allowed after `expires_at` as long as the lease is unsettled. Access
has lapsed (`remaining_calls` is `0`), but the refund claim still has value and
should be sellable. The new consumer owns the revoke path from then on. This is
deliberate and covered by `test_transfer_after_expiry_moves_refund_right`.

### Interaction with the consumer challenge window (#132)

The challenge-window change makes `settle_lease` a proposal that sets
`lease.settled = true` immediately, with the payout released later by
`finalize_settlement` or `resolve_challenge`. Because `transfer_lease` rejects
any settled lease:

- a lease with a **pending** settlement cannot be transferred,
- a lease under an **active challenge** cannot be transferred,
- the settlement record keeps the consumer that owned the lease at proposal time,
  so a challenge always belongs to the address that filed it.

That is the correct composition: a provider's settlement proposal freezes
ownership just as it freezes revocation. A consumer who wants to sell a lease
must do so before the provider proposes settlement. If the challenge-window PR
merges first, this entry point needs no change; its `settled` check already
covers the new states. The design note for #132 should link here as the
transfer interaction.

### After a transfer

- The previous consumer cannot use, revoke or transfer the lease. Their
  `get_lease` reads show the new owner.
- The new consumer inherits both the remaining access (if unexpired) and the
  refund right.
- A provider that cached `lease.consumer` must refresh on a mismatch: transfers
  are on chain, and the cached value is only an optimisation.

## Safety analysis

- **Atomicity.** One field is written in one `save_lease` call; there is no
  intermediate state where both addresses can act.
- **No refund theft.** The old consumer cannot transfer and then revoke: the
  revoke path checks `lease.consumer`, which is now the recipient.
- **No post-settlement transfer.** `settled` is set before any payout or refund,
  so a lease cannot change hands after funds have been released.
- **No self-transfer.** Rejecting `new_consumer == consumer` avoids a meaningless
  ownership event that indexers would have to special-case.
- **Auth is not delegable.** The recipient never signs; only the current owner
  can move the lease. A stolen consumer key can transfer the lease, which is the
  same authority that key already has to use or revoke it.

## Alternatives considered

1. **Offer/accept handshake.** Safer against typos but requires pending-offer
   storage, offer expiry and cancellation, and doubles the transaction count.
   Not required by the issue; assignment is a smaller protocol.
2. **Transfer by provider co-signature.** Adds a provider liveness dependency and
   lets a provider block resale. Rejected.
3. **Splitting or partial transfer.** Needs per-consumer accounting inside one
   lease and breaks the single `consumer` field. Out of scope.
4. **Revoke-and-recreate.** The old consumer could revoke (only after expiry) and
   the recipient create a new lease, but that loses the term and requires the
   recipient to pay again. This issue exists because that is not a transfer.

## Test matrix

Covered in `contracts/registry/src/test.rs`:

- transfer moves ownership, access and the refund right, and emits
  `lease_transferred`,
- settlement after transfer pays the provider and refunds the new consumer,
- unauthorized attempts by an impostor and by the provider, plus self-transfer,
- transfer of a settled lease and of a revoked lease → `LeaseNotTransferable`,
- transfer after expiry but before revocation, after which only the new consumer
  can revoke.

## Rollout

- No storage layout or `Lease` shape change: existing leases are transferable
  immediately after the upgrade.
- Indexers should add `lease_transferred` to keep a consumer→lease projection in
  sync. `lease_created` alone is no longer the final word on ownership.
- Providers must treat `lease.consumer` as mutable. Re-read on an ownership
  mismatch instead of caching it for the whole lease term.
