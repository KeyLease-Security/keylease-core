# Design notes: Wave 11 contract features

These notes cover four related upgrades to `KeyLeaseRegistry`:

- [#124](https://github.com/KeyLease-Security/keylease-core/issues/124) lease top-up / renewal
- [#125](https://github.com/KeyLease-Security/keylease-core/issues/125) bounded batch settlement
- [#126](https://github.com/KeyLease-Security/keylease-core/issues/126) pull-payment claim model
- [#127](https://github.com/KeyLease-Security/keylease-core/issues/127) optional signed usage receipts

`CONTRIBUTING.md` asks `high` issues for a note with **Problem**, **Invariants**,
**Approach**, **Threat model** and **Test plan**, approved before implementation.
This page follows that structure for all four so the design can be reviewed
alongside the code. The implementation is submitted in the same PR; maintainers
can request changes to either before merge.

## Shared constraint: storage compatibility

The registry is live on Testnet, and upgrades preserve the contract id and
storage. The design therefore **does not change the encoded layout of any
existing value**:

- `Service` and `Lease` keep their exact field list and order.
- New state lives under two appended `DataKey` variants
  (`ServiceReceiptKey(u64)`, `Claimable(Address)`) that no existing entry uses.
- Top-up pricing reads the rate the lease already commits to
  (`locked_deposit / allocated_calls`) rather than adding a field to `Lease`.
- New error codes are appended at 17–24; existing numbers are unchanged.

Pre-upgrade leases remain readable and settleable; their settlement credits
claimable balances, so no migration is required.

Version `0.3.0` reflects the behavioural change (payouts become claims).

## #124 — Top up or renew a lease

### Problem

A consumer that needs more calls or time must create a second lease. That
creates a second id, a second storage entry and a second settlement path for one
logical session.

### Invariants

- An in-flight lease is never repriced.
- `locked_deposit == rate × allocated_calls` at all times.
- A lease's persistent entry outlives its term plus grace; expiry never exceeds
  `now + MAX_LEASE_DURATION_SECS`.
- `used_calls` is preserved, and a settled or expired lease cannot be extended.

### Approach

New entry point
`top_up_lease(consumer, lease_id, additional_calls, additional_duration_secs)`:

```text
rate             = locked_deposit / allocated_calls
added_deposit    = rate × additional_calls
allocated_calls += additional_calls
expires_at      += additional_duration_secs
locked_deposit  += added_deposit
used_calls       unchanged
```

The rate is derived, not read from the service, so a top-up can never reprice
access. At least one addition must be non-zero (`InvalidTopUp`). The extended
lease is persisted before the additional deposit is transferred; a
duration-only top-up skips the token. Emits
`lease_topped_up { lease_id, consumer, additional_calls,
additional_duration_secs, new_expires_at, added_deposit }`.

**Rejected alternatives:** adding a `rate_per_call` field to `Lease` (breaks the
encoded layout of existing leases); charging the service's current rate (allows
repricing in-flight access); allowing renewal of expired leases (revives a lease
after the provider's grace window and complicates the consumer's revoke race).

### Threat model

- A non-consumer top-up → `Unauthorized`.
- Floating-point or truncation drift in the derived rate → integer division is
  exact because the invariant is maintained; checked math guards overflow.
- Top-up just before expiry to grief the provider → only the consumer pays for
  it, and the added term is capped at 30 days from now.
- Storage eviction → `save_lease` recomputes TTL from the new expiry.

### Test plan

Calls-only, duration-only, both, empty parameters, wrong consumer, settled
lease, expired lease, max-duration bound, event payload, and partial settlement
after a top-up.

## #125 — Bounded batch settlement

### Problem

Providers settle one lease per transaction. A queue of hundreds is a long
operational tail and unnecessary fee overhead.

### Invariants

- All-or-nothing: an invalid entry settles nothing.
- No unbounded loop over caller input; at most `MAX_BATCH_SETTLE = 20`.
- Every settled lease still satisfies ownership, settleability and call bounds.
- One `lease_settled` event per lease.

### Approach

`settle_leases(provider, Vec<(u64, u32)>)` requires provider auth once, rejects
an empty batch (`EmptyBatch`), an oversize batch (`BatchTooLarge`) and duplicate
ids (`DuplicateLease`), then runs two passes: pass 1 validates every lease
without mutating state, pass 2 persists and credits each settlement. Batch
entries for receipt-protected services fail with `ReceiptRequired`; those settle
individually with receipts.

**Rejected alternatives:** partial success (makes the batch's accounting
ambiguous and complicates events); a configurable limit (storage/config surface
for no gain); per-lease auth inside the loop (trips the host's "frame is already
authorized" guard).

### Threat model

- One bad lease among good ones → nothing settles; tested.
- Duplicate ids to double-credit one deposit → `DuplicateLease`.
- Mixed ownership → `Unauthorized` before any write.
- Batch used to bypass receipt mode → `ReceiptRequired`.

### Test plan

Multiple leases and event count, empty batch, oversize batch, duplicate ids,
mixed ownership, one unknown id with a valid lease (all-or-nothing), and a
receipt-protected service.

## #126 — Pull-payment claim model

### Problem

Settlement and revocation transfer tokens inline, so a token failure can block
settlement and every money path must reason about transfer ordering and
re-entrancy. Batch settlement would also perform N transfers in one call.

### Invariants

- `payout + refund == locked_deposit` for every lease.
- Every credited stroop is withdrawable exactly once by its beneficiary.
- `settled` is persisted before any credit; the claim balance is removed before
  the token transfer.
- A settled lease never transitions again.

### Approach

Settlement credits the provider `verified_calls × rate_per_call` and the
consumer the remainder; revocation credits the consumer the full deposit.
Balances are keyed by beneficiary and accumulate across leases in
`Claimable(Address)`. `claim(beneficiary)` removes the entry, transfers, and
emits `claimed { beneficiary, amount }`; `claimable_balance(address)` exposes the
amount. `lease_settled` / `lease_revoked` keep their existing payloads, now
meaning credits.

**Rejected alternatives:** inline transfers with a separate withdrawal queue
(duplicates the accounting surface); per-lease claim entries (more rent, more
keys); allowing zero-value claims (hides stray calls; `NothingToClaim` is
clearer).

### Threat model

- Re-entrant token calls `claim` during transfer → entry already removed →
  `NothingToClaim`; adversarial test.
- Double settlement → `LeaseAlreadySettled` before any credit.
- Claiming someone else's balance → only the authenticated address's entry is
  touched.
- Conservation break via rounding → all amounts are integers computed with
  checked math from `locked_deposit`.

### Test plan

Zero balance, single use, accumulation across leases, conservation across both
beneficiaries, event payload, and the re-entrant-token test retargeted from
settlement to `claim`.

## #127 — Optional signed usage receipts

### Problem

The contract trusts the provider's `verified_calls` report entirely. Consumers
rely on the fixed allotment and refund path rather than any attestation of
delivery.

### Invariants

- Unsigned settlement remains the default and is unchanged for services without
  a key.
- A configured receipt is valid only for one contract, lease, service and call
  count.
- A settled lease cannot be settled again, with or without a receipt.
- Receipt mode cannot be bypassed through `settle_leases`.

### Approach

`set_service_receipt_key(provider, service_id, Option<BytesN<32>>)` requires
service ownership and stores the key under `ServiceReceiptKey(service_id)`;
`None` clears it. `settle_lease` returns `ReceiptRequired` for protected
services, and `settle_lease_with_receipt(provider, lease_id, verified_calls,
BytesN<64>)` returns `ReceiptNotConfigured` for unprotected ones. The signed
payload is exactly 101 bytes:

```text
"KeyLease/usage-receipt/v1"   25-byte domain separator
|| contract address StrKey    56 bytes
|| lease_id                   u64 big-endian
|| service_id                 u64 big-endian
|| verified_calls             u32 big-endian
```

Verification uses the host's `crypto().ed25519_verify`; an invalid signature
aborts the transaction, which is the on-chain rejection.

**Rejected alternatives:** adding the key to `Service` (breaks stored layout);
a global key (cannot be rotated per service or mixed with unsigned services); a
nonce per lease (a lease settles once, so there is no second transition to
protect); signing the raw XDR of a struct (harder to reproduce off-chain than a
documented fixed byte layout).

### Threat model

- Forged signature → verification aborts.
- Receipt moved to another lease/service/count/deployment → binding makes the
  signature invalid.
- Replay on the same lease → `LeaseAlreadySettled` before verification.
- Bypass through the unsigned path or batch → `ReceiptRequired`.
- Stolen signing key → the provider can still only claim within the lease's
  allotment, and can rotate the key with `set_service_receipt_key`.

**Limitation:** a signature proves the provider attested to a count, not that an
HTTP request was served. It is dispute evidence, not an independent oracle.

### Test plan

Key change event, key ownership, valid signature settlement, invalid signature,
cross-lease and cross-count binding, replay, unsigned rejection, unconfigured
service, key clearing, and per-service isolation.

## Verification

`cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --all` (72 tests) and
`cargo build --target wasm32v1-none --release --all` are green. The docs build
with `mkdocs build --strict`.
