# Design note: consumer challenge window before settlement pays out

- **Issue:** #132 — *Add a consumer challenge window before settlement pays out*
- **Status:** Implemented on `fix/132-consumer-challenge-window`; a maintainer
  must approve this note before the change is merged.
- **Scope:** `contracts/registry/src/{lib,storage,types,events,errors}.rs` and the
  public documentation. No `Lease` layout change is required.

## Problem

`settle_lease` is a single trusted report: the provider submits
`verified_calls`, and the contract immediately pays
`verified_calls × rate_per_call` and refunds the rest. The contract cannot
observe HTTP usage, so a provider can over-report up to `allocated_calls` and the
consumer has no way to dispute it. The fixed allotment bounds the loss, but it
does not give the consumer a remedy inside the protocol.

## Goals

1. Give the consumer a bounded window to dispute a proposed settlement using an
   on-chain evidence commitment (no evidence bytes on chain).
2. Keep the payout locked while a dispute is open; no party can withdraw it.
3. Define an unambiguous resolver and the two outcomes.
4. Preserve the existing provider and consumer guarantees:
   - a provider that never settles can still be revoked after the grace window,
   - a settlement can never be paid twice,
   - a consumer always has a path to the deposit.
5. Keep the `Lease` storage entry unchanged, so the upgrade is additive.

## Non-goals

- Adjudicating evidence on chain. The contract stores a 32-byte commitment; the
  admin evaluates the evidence off chain.
- Partial or formula-based awards. Resolution is binary: uphold (full refund) or
  reject (proposal stands).
- Changing the 24-hour post-expiry provider grace window or the revoke path.

## Design

### Actors

| Actor | Power |
| --- | --- |
| Provider | Proposes a settlement split for a lease it owns. |
| Consumer | Challenges a proposal inside the window, or withdraws the challenge. |
| Admin | Resolves an active challenge: uphold (full consumer refund) or reject (proposal stands). |
| Anyone | Finalizes a proposal after the window closes when no challenge is active. |

### Constants

`CHALLENGE_WINDOW_SECS = 86_400` (24 hours), the same length as the existing
settlement grace period. It is a protocol constant, not a per-service setting,
so indexers and clients can hard-code the settlement time without reading
service metadata.

### State machine

```
settle_lease (proposal)
        │
        ▼
   ┌─────────┐   challenge_settlement    ┌────────────┐
   │ Pending │ ────────────────────────▶ │ Challenged │
   └─────────┘ ◀──────────────────────── └────────────┘
        │        cancel_challenge              │
        │                                      │ resolve_challenge (admin)
        │ now ≥ challengeable_until            │
        ▼                                      ▼
   ┌───────────────────────────────────────────────┐
   │                  Finalized                    │
   └───────────────────────────────────────────────┘
```

- `settle_lease` records a `Settlement` with `state = Pending`,
  `challengeable_until = now + 24h`, and moves no funds.
- The lease's `settled` flag is set at proposal time, so it can no longer be
  settled again or revoked. Funds remain in escrow.
- `challenge_settlement(consumer, lease_id, evidence_hash)` requires consumer
  auth, a non-zero evidence hash, and `now < challengeable_until`.
- `cancel_challenge(consumer, lease_id)` clears an active challenge and returns
  the settlement to `Pending`. The original deadline is unchanged, so
  cancellation cannot extend the window. A consumer may challenge again while
  the window is still open.
- `finalize_settlement(lease_id)` is permissionless and requires
  `state == Pending` and `now >= challengeable_until`. It pays the recorded
  payout and refund.
- `resolve_challenge(admin, lease_id, upheld)` requires the stored admin. It may
  run before or after the deadline while a challenge is active:
  - `upheld = true`: payout `0`, refund `locked_deposit` (the proposal's payout
    plus refund),
  - `upheld = false`: the original payout and refund stand.
- `locked_balance(lease_id)` reports the full deposit while a settlement is
  pending or challenged, and `0` once finalized or revoked.

### Boundary semantics

| Timestamp | `challenge_settlement` | `finalize_settlement` |
| --- | --- | --- |
| `now < challengeable_until` | allowed | `ChallengeWindowStillOpen` |
| `now >= challengeable_until` | `ChallengeWindowClosed` | allowed if `Pending` |

`challengeable_until` is an exclusive deadline for challenges and an inclusive
start for finalization, so there is no ledger at which both are allowed.

### Storage

A new persistent entry keeps the upgrade additive:

```rust
DataKey::Settlement(u64) // persistent, keyed by lease_id
```

`Settlement` carries `lease_id`, `consumer`, `provider`, `verified_calls`,
`payout`, `refund`, `proposed_at`, `challengeable_until`, `state`,
`challenge_evidence` and `challenged_at`. The consumer and provider are copied
in, so finalization and resolution never need to read the `Lease` entry (whose
TTL is scoped to the lease term plus grace). Settlement entries are extended to
the standard ~30-day persistent TTL on every save and read.

`Lease` itself is unchanged.

### Events

| Event | Topics | Payload |
| --- | --- | --- |
| `lease_settlement_proposed` | `lease_id` | `provider`, `verified_calls`, `payout`, `refund`, `challengeable_until` |
| `lease_challenged` | `lease_id` | `consumer`, `evidence_hash` |
| `lease_challenge_cancelled` | `lease_id` | `consumer` |
| `lease_challenge_resolved` | `lease_id` | `admin`, `upheld` |
| `lease_settled` | `lease_id` | `provider`, `verified_calls`, `payout`, `refund` |

`lease_settled` keeps its payload but now means **funds have been released**; it
is emitted only by finalize and resolve, not by the proposal. Indexers that
watched it as "provider was paid" remain correct; they should add
`lease_settlement_proposed` if they need the proposal time.

### Errors (appended, never renumbered)

| Code | Variant |
| --- | --- |
| 17 | `SettlementNotFound` |
| 18 | `SettlementFinalized` |
| 19 | `ChallengeWindowClosed` |
| 20 | `SettlementChallenged` |
| 21 | `NoActiveChallenge` |
| 22 | `ChallengeWindowStillOpen` |
| 23 | `InvalidEvidence` |

## Safety analysis

- **No double release.** `Lease.settled` is set before the settlement entry is
  written, and the settlement state is persisted as `Finalized` before any token
  transfer. A re-entrant token sees a finalized settlement and cannot pay again.
- **No revocation race.** Because the lease is marked settled at proposal time,
  `revoke_expired_lease` fails with `LeaseAlreadySettled` once a proposal exists.
  The consumer cannot take both the refund and the provider's payout.
- **No payout while disputed.** `finalize_settlement` rejects `Challenged`
  regardless of the timestamp.
- **No admin theft.** The admin can only choose between the proposal and a full
  consumer refund; it cannot direct funds to itself or to an arbitrary address.
  Its address is fixed at `init`.
- **Conservation.** Every path pays `payout + refund == locked_deposit`
  (upholding sets `payout = 0`, `refund = locked_deposit`).
- **No dangling challenge.** Resolution has no deadline, but settlement entries
  live for the ~30-day persistent TTL and are re-extended on read. A dispute
  should be resolved well inside that horizon; this is documented as a known
  operational bound.

## Trade-offs

- **Settlement is slower.** Providers wait 24 hours for the payout unless they
  settle with `verified_calls = 0`. This is the cost of a dispute path; it
  matches the existing 24-hour grace period, so the consumer's and provider's
  clocks are symmetric.
- **The admin becomes an adjudicator.** The trust model shifts from "the
  provider is trusted on usage" to "the provider and the consumer can escalate
  to the admin". A compromised admin can zero a legitimate payout or refuse to
  resolve. The mitigation is the same as for upgrades: a cold or multisig admin
  and public on-chain events.
- **Binary resolution.** There is no partial award. A future version could let
  the admin set `awarded_calls`, but that adds another trust dimension and more
  storage; the issue asks for a dispute path, not an arbitration engine.

## Alternatives considered

1. **Challenge directly against a finalized payout (clawback).** Rejected: the
   token has no clawback guarantee, and a provider that withdraws immediately
   would be outside the contract's reach.
2. **Consumer challenge with no resolver (auto-refund on any challenge).**
   Rejected: a consumer could always avoid paying by challenging, which breaks
   the provider's guarantee.
3. **Optimistic challenge with an admin-settable award.** Deferred: more complex
   and not required to close the over-reporting gap.
4. **Per-service challenge window.** Rejected: adds a configuration surface and
   makes indexer expectations harder to reason about. A protocol constant keeps
   the ABI simple.

## Test matrix

Covered in `contracts/registry/src/test.rs`:

- no-challenge fast path: funds locked before the boundary, finalized at exactly
  `challengeable_until`,
- window boundary: challenge accepted at `challengeable_until - 1`, rejected at
  `challengeable_until`,
- active challenge blocks finalization,
- consumer cancellation returns the settlement to pending and never pays early,
- cancellation without a challenge, double challenge, non-consumer actions,
- zero evidence hash rejected,
- admin uphold (full consumer refund) and admin reject (proposal stands),
- resolution before the window closes,
- resolver authorization and "nothing to resolve" cases,
- events for proposal, challenge, cancellation, resolution and payout,
- existing lifecycle, SAC, re-entrancy and revocation tests updated for the
  two-phase settlement.

## Rollout

- Deploy as a new version of the registry Wasm; the new `DataKey` variant is
  additive and `Lease` is unchanged, so in-flight leases remain readable.
- In-flight leases that were settled under the old code are already finalized
  and have no `Settlement` entry; views return `SettlementNotFound`, which
  clients must handle.
- Indexers should subscribe to `lease_settlement_proposed` and
  `lease_challenge_*` in addition to `lease_settled`.
