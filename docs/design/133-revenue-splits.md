# Design note — Revenue splits across multiple providers (#133)

> **Status:** Proposed — **awaiting maintainer approval**. No contract code is
> changed by this note. Per [CONTRIBUTING](https://github.com/KeyLease-Security/keylease-core/blob/main/CONTRIBUTING.md),
> a maintainer must approve this design before implementation begins.
>
> **Issue:** [#133 — Allow revenue splits across multiple providers](https://github.com/KeyLease-Security/keylease-core/issues/133)
> **Target release:** v0.3.0 · **Touches:** `lib.rs`, `types.rs`, `storage.rs`, `events.rs`, `errors.rs`

## 1. Problem statement

A service has exactly one `provider` address, and `settle_lease` pays that
address the whole payout. Teams that share operational and infrastructure costs
— an API owner plus an RPC operator, a marketplace front end plus an upstream
data source — have to route money off-chain after settlement, which is manual,
opaque to indexers and outside the protocol's accounting.

This note proposes a **bounded revenue split** stored per service: an ordered
list of recipients with basis-point shares that always sums to 10 000. A service
with no split configured keeps the current single-provider behaviour exactly.

### Goals

- Pay multiple recipients atomically in the settlement transaction.
- Keep the existing single-provider layout and settlement flow as the default,
  with no migration and no change to `Service` or `Lease`.
- Bound the work per settlement: fixed maximum recipients, no unbounded loop.
- Make the split and every per-recipient payout observable off-chain.
- Guarantee the distributed total never exceeds the settlement payout and that
  rounding never strands or creates value.

### Non-goals

- Changing who may settle a lease (`service.provider` remains the only settler).
- Transferring service ownership or splitting governance rights; recipients get
  revenue only.
- Per-lease split snapshots or recipient-level escrow accounts.
- Splitting the consumer refund (it always goes to the consumer).

## 2. Proposed architecture

### 2.1 Representation

```rust
/// One entry in a service's revenue split.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevenueShare {
    pub recipient: Address,
    /// Share of every payout, in basis points. All shares sum to 10_000.
    pub share_bps: u32,
}
```

- **Basis points:** 1 bps = 0.01 %, and `10_000` bps = 100 %.
- **Recipient count:** `1..=MAX_SPLIT_RECIPIENTS`, with
  `MAX_SPLIT_RECIPIENTS = 10` (see §6 for why 10).
- **Sum:** `sum(share_bps) == 10_000` exactly, computed with `checked_add`.
- **Non-zero:** every `share_bps > 0`; a zero-share recipient is rejected rather
  than silently paid nothing.
- **Duplicates:** the same recipient address may appear once. A duplicate is
  rejected at configuration time, which also prevents duplicate payments at
  settlement.
- **Self-payment:** a recipient equal to the registry's own address is rejected,
  because a transfer to self would be a no-op that breaks the
  `distributed == payout` accounting story.

The split is stored **outside** `Service`:

| Key | Storage | Value | TTL |
| --- | --- | --- | --- |
| `RevenueSplit(u64)` | persistent | `Vec<RevenueShare>` | extended on write and read, 30-day horizon, matching `Service` |

`Service` keeps exactly its current five fields. That is what makes the change
upgrade-safe: existing `Service` entries decode unchanged, and the split key is
simply absent for every service that never configures one.

### 2.2 Backward compatibility: the single-provider default

An absent `RevenueSplit(service_id)` key **is** the legacy configuration:

```text
no split stored  =>  pay 100 % to service.provider, as today
```

Consequences:

- `register_service` is unchanged; a provider does not have to configure
  anything.
- Existing services continue to settle with one transfer and the exact same
  `lease_settled` event payload.
- Existing leases are unaffected; the split is read at settlement time, and a
  service without a split behaves identically to v0.2.x.
- A provider that wants the old behaviour back can call
  `set_revenue_split` with a single entry `{ provider, 10_000 }`, or
  `clear_revenue_split` to remove the key entirely. Both produce the same
  payout; the event differs.

### 2.3 Settlement

`settle_lease` keeps all of its current checks, ordering and refund logic. Only
the payout step changes:

```text
payout = service.rate_per_call * verified_calls      // unchanged, checked
refund = lease.locked_deposit - payout               // unchanged, checked

persist lease.used_calls, lease.settled = true       // unchanged, before transfers

if a split is stored for lease.service_id:
    distributed = 0
    for i in 0 .. shares.len():
        amount[i] = (payout * shares[i].share_bps) / 10_000    // floor, checked
        distributed += amount[i]                               // checked
    amount[last] += payout - distributed                       // remainder
    for each non-zero amount[i]:
        transfer(contract -> shares[i].recipient, amount[i])
else:
    transfer(contract -> service.provider, payout)             // unchanged
```

**Rounding rule.** The first `n-1` recipients receive
`floor(payout * share_bps / 10_000)`; the **last recipient in the configured
list** receives its own floor **plus the entire remainder**
`payout - sum(floors)`. Because the shares sum to 10 000,

```text
sum(floor(payout * bps_i / 10_000))  <=  payout  <  sum(floors) + n
```

so the remainder is in `0..n`, and the last payment remains at most `payout`
plus a few stroops of rounding. Total distributed is exactly `payout`; nothing
is stranded in the contract and nothing is minted. The rule is deterministic and
depends only on the stored order, so any indexer can reproduce every amount from
`lease_settled` plus the stored split. If `payout == 0`, no transfers occur.

**Ordering and atomicity.** The split is read **before** the first transfer and
is not re-read afterwards, so a re-entrant token cannot change recipients
mid-settlement. `settled = true` is still persisted before any transfer, so a
re-entrant `settle_lease` reverts. If any transfer fails, the whole Soroban
invocation reverts: no recipient is paid, the lease stays unsettled, and the
refund does not happen either.

**Refund.** The consumer refund is unchanged and is paid once, after the
payout distribution. It is never split.

### 2.4 Changing the split

`set_revenue_split` is callable only by the current `service.provider`. It
replaces the entire list atomically; there is no per-entry editing, so a partial
write can never leave the shares summing to something other than 10 000.

The split applies to settlements that happen **after** the change. In-flight
leases are paid under the split in force at settlement time. This is deliberate:
the escrow is owed to the provider, and routing that revenue among the
provider's own recipients is the provider's commercial decision. The consumer's
refund and the total payout are unaffected, so no consumer is harmed by a
mid-lease change. The trade-off against snapshotting per lease is discussed in
§8.

## 3. Storage changes

| Key | Storage | Value | Notes |
| --- | --- | --- | --- |
| `RevenueSplit(u64)` | persistent | `Vec<RevenueShare>` | new, additive; TTL extended on save and read |

No existing key changes, no field is added to `Service` or `Lease`, and the
`DataKey` discriminants keep their wire names. A v0.3.0 Wasm can be installed
over a deployment that holds live services and leases; services without the new
key settle exactly as before.

The stored vector is bounded by `MAX_SPLIT_RECIPIENTS`, so a split entry has a
fixed maximum encoded size (about ten address+bps pairs) and a predictable rent
cost.

## 4. Public interface changes

### Entry points

| Entry point | Auth | Description |
| --- | --- | --- |
| `set_revenue_split(provider, service_id, shares: Vec<RevenueShare>)` | provider (must own the service) | Validate and store a complete split. Emits `revenue_split_updated`. |
| `clear_revenue_split(provider, service_id)` | provider (must own the service) | Remove the split and fall back to 100 % to `service.provider`. Emits `revenue_split_cleared`. |
| `get_revenue_split(service_id)` | — | `Option<Vec<RevenueShare>>`; `None` means the single-provider default. |

Read-only helper (optional, recommended for indexers):

| View | Returns |
| --- | --- |
| `payees(service_id)` | `Vec<Address>` — the configured recipients, or `[service.provider]` when no split is stored |

### Events

| Event | Topics | Data |
| --- | --- | --- |
| `revenue_split_updated` | `service_id` | `provider`, `shares: Vec<RevenueShare>`, `recipient_count`, `total_bps` |
| `revenue_split_cleared` | `service_id` | `provider` |
| `lease_split_paid` | `lease_id` | `total_payout`, `recipients: Vec<Address>`, `amounts: Vec<i128>` |

- `revenue_split_updated` carries the **full configured list**, so an indexer
  can reconstruct every future payment from one event without extra RPC reads.
  `total_bps` is included (always 10 000) as a self-check for consumers.
- `lease_split_paid` is emitted **only when a split is configured** and carries
  the exact per-recipient amounts actually transferred (including the remainder
  assignment). Indexers therefore never have to re-implement the rounding rule
  to reconcile balances. It is emitted alongside the unchanged `lease_settled`;
  the two share the `lease_id` topic, and `lease_settled.payout` equals
  `lease_split_paid.total_payout`.
- For the legacy single-provider path neither new settlement event is emitted,
  so the existing event stream is unchanged.
- Neither event carries secret material: recipients and shares are already
  public on-chain configuration.

### Error codes (appended, never renumbered)

| Code | Variant | Meaning |
| --- | --- | --- |
| 33 | `EmptySplit` | Zero recipients supplied |
| 34 | `TooManyRecipients` | More than `MAX_SPLIT_RECIPIENTS` |
| 35 | `InvalidShares` | A share is zero, or the total is not exactly 10 000 |
| 36 | `DuplicateRecipient` | The same address appears twice |
| 37 | `InvalidRecipient` | A recipient is the registry itself |

(`17–32` are reserved by the #137 proposal; the final numbers are assigned when
both designs are approved so the two changes never collide.)

## 5. Authorization and governance model

| Actor | Can do | Cannot do |
| --- | --- | --- |
| `service.provider` | Set/clear the split for its service | Change another service's split |
| Recipient | Receive its share at settlement | Change the split, settle, or claim outside settlement |
| Admin | `set_token`, `upgrade` | Change a split or move payouts |
| Consumer | Create/revoke leases | Change the split or redirect the refund |

No new governance role is introduced. Setting a split is an ordinary
provider-signed transaction, exactly like `set_service_active`, and the service
owner remains the only account that can settle, so the payer and the person
choosing the payees are the same actor.

## 6. Security considerations

**Unauthorized changes.** `set_revenue_split` / `clear_revenue_split` call
`require_auth()` on `provider` and then check `service.provider == provider`.
A third party cannot redirect a service's revenue, and the admin cannot either.

**Invalid or excessive allocations.** Validation happens before any write:
non-empty, `<= 10` entries, every `share_bps > 0`, `checked_add` sum exactly
`10_000`, no duplicates, no self-recipient. A rejected call leaves the stored
split untouched. The store-then-overwrite design is atomic per call, so there is
never an intermediate sum other than the old or the new valid list.

**Duplicate payments.** Rejected at configuration time, and the settlement loop
walks each vector index exactly once. Because the payout is computed and
validated before the loop, no recipient can be paid more than its stored share
(plus the single remainder allocation to the last entry).

**No value creation or loss.** `sum(amounts) == payout`, enforced by the
remainder assignment and checked arithmetic; `payout + refund == locked_deposit`
still holds. `payout` and every `amount[i]` use `checked_mul`/`checked_add`; an
overflow reverts the transaction rather than truncating a recipient's payment.

**Reentrancy and atomicity.** `settled = true` is persisted before any transfer,
and the split vector is read once, before the transfers, so a hostile recipient
or token contract cannot alter the list or settle twice. All payments and the
refund happen in one transaction; a failure reverts the entire distribution and
the refund. The adversarial re-entrant-token test is extended to a split with
two recipients.

**Resource limits.** `MAX_SPLIT_RECIPIENTS = 10` bounds the settlement to at
most ten payout transfers plus one refund. That is the main cost driver: each
SEP-41 `transfer` is a cross-contract call, and Soroban charges instructions
and ledger entries per call. Ten keeps a split settlement comfortably within a
standard transaction while covering realistic team structures. The trade-off
against a larger bound is in §8; the implementation must include a test that
settles a 10-recipient split so the budget is measured, not assumed.

**Griefing and storage.** The list is provider-owned and bounded, so no one can
inflate a service's storage through the split. `RevenueSplit` is persistent with
the same 30-day TTL policy as `Service`; it cannot be evicted while the service
is being read or settled, and a split costs its provider a bounded rent.

**Rounding edge cases.** `payout < number of recipients` produces zeros for
some entries; zero-amount transfers are skipped and the last entry receives the
few stroops of remainder. A single-recipient split at 10 000 bps is
mathematically identical to the legacy path; a one-recipient split at any other
value is rejected by the sum check. `payout == 0` emits no split payments but
still emits `lease_settled` and the refund.

**Interaction with #137.** A stake is attached to `service.provider`, not to
split recipients. Slashing debits the provider's stake, never a recipient's
payout, and the revenue split does not affect stake availability or dispute
resolution. Only the provider can settle and therefore only the provider can
trigger a distribution.

## 7. Testing strategy

New tests in `contracts/registry/src/test.rs`, keeping the existing suite green:

**Configuration**

- a service with no split settles exactly as before: one transfer to the
  provider, unchanged `lease_settled` payload, no `lease_split_paid`;
- `set_revenue_split` with one entry `{provider, 10_000}` behaves like the
  default and emits `revenue_split_updated`;
- two and three recipients with valid shares are stored and readable through
  `get_revenue_split` / `payees`;
- the full event payload (topics + shares + count + total) is asserted with
  `assert_single_event`;
- invalid totals (9 999, 10 001), a zero share, an empty list, eleven
  recipients, a duplicate address and a self-recipient each revert with the
  documented error and leave the previous split intact;
- a non-owner, an impostor and the admin all fail to set or clear a split
  (`Unauthorized`);
- `clear_revenue_split` restores the legacy behaviour and emits the event.

**Settlement**

- a two-recipient split (e.g. 7 000 / 3 000) pays each the right amount and the
  consumer gets the correct refund; `escrow == 0` afterwards;
- three recipients with a payout that does not divide evenly (e.g. payout 100,
  shares 3 333 / 3 333 / 3 334) pays `33 / 33 / 34` and emits matching
  `lease_split_paid.amounts`;
- a rounding case where every floor truncates (payout 1, two recipients) gives
  the remainder to the last recipient, and the total still equals the payout;
- `verified_calls == 0` (full refund) performs no payout transfers and emits no
  `lease_split_paid`;
- a 10-recipient split settles successfully (resource-limit test) with the
  total exactly equal to `payout`;
- the real-SAC lifecycle test is repeated with a split token, proving the
  distribution works against the SAC;
- the re-entrant token cannot settle twice when a split is configured and the
  first recipient is hostile;
- a recipient that is a contract is paid exactly once.

**Invariants asserted after every settlement test**

- `sum(recipient payments) + refund == locked_deposit`;
- `lease.locked_deposit`, `lease.settled` and the provider's stake (when #137 is
  present) are unchanged by the split;
- the registry's token balance returns to zero.

## 8. Open questions and trade-offs

1. **Recipient bound.** 10 covers most teams and bounds settlement to 11
   cross-contract calls. A larger bound (e.g. 20) is more flexible but raises
   the fee of every settlement and the chance of exceeding the transaction
   budget. *Question for maintainers:* is 10 right, or should it be 5?
2. **Snapshot vs live split.** Reading the split at settlement lets a provider
   re-route revenue mid-lease; snapshotting it into the lease would require a
   new `Lease` field (breaking the stored encoding) or a new per-lease key and
   more storage. This note chooses live configuration because only the payer's
   own routing changes and the consumer's refund is untouched. *Alternative:*
   `LeaseSplit(lease_id)` written at `create_lease` for full per-lease
   determinism.
3. **Remainder policy.** Paying the remainder to the last configured entry is
   the simplest deterministic rule and easy to reproduce. *Alternatives:* the
   largest share, or largest-remainder (Hamilton) apportionment, which is
   marginally fairer but more code and more subtle to test.
4. **Dust when all shares floor to zero.** With `payout < n`, early recipients
   can receive 0 while the last receives the whole payout. That is exact and
   harmless, but a provider may be surprised. *Alternative:* reject settlements
   whose payout is smaller than the recipient count — rejected here because it
   would couple settlement to the split and could block a refund.
5. **Recipients as contracts.** Paying a contract recipient is allowed; whether
   it should require the recipient to implement a specific interface (or be
   rejected) is a policy question. This note allows any `Address`, consistent
   with the rest of the protocol.
6. **Per-recipient events.** One `lease_split_paid` carrying parallel arrays is
   compact and cheap; `n` separate events would be easier for naive indexers but
   costs `n` event emissions per settlement. This note prefers the single
   event; the schema should be documented before release.
7. **Split plus future service transfer.** If service transfer ships (#issue
   for key rotation), the new owner inherits the split. The design should state
   that explicitly, and whether a transfer must clear or re-confirm the split.
8. **Versioning.** The change is additive, but the workspace version should move
   to `0.3.0` with the #137 work and the release notes must describe the split
   semantics and rounding rule.

## 9. Review request

Per `CONTRIBUTING.md`, this note needs maintainer approval before code:

- Confirm `MAX_SPLIT_RECIPIENTS` (10 vs 5) and the one-event-with-arrays shape.
- Confirm that the split is read at **settlement time** rather than snapshotted
  per lease.
- Confirm the rounding rule (remainder to the last configured recipient).
- Confirm the error-code range so it does not collide with #137.

Once approved, implementation lands on the same feature branch as #137, with
the tests above and the full four-command validation from `CONTRIBUTING.md`.
