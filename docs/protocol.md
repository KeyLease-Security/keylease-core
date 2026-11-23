# How the protocol works

## Actors

| Actor | Address | Can do |
| --- | --- | --- |
| **Admin** | one Stellar account | `init` once, `set_token`, `propose_upgrade`/`execute_upgrade`/`cancel_upgrade` (24h delay). Cannot move escrowed funds. |
| **Provider** | per service (account or contract address) | Register/pause services, settle leases it owns. |
| **Consumer** | per lease (account or contract address) | Create leases, revoke expired unsettled leases. |
| **Delegate** | optional, any account | Sponsor `create_lease_for` for a consumer. Pays the transaction fee only — never the deposit, and holds no claim on the lease. |

The admin is a deployment role. It can point the registry at a token address and
install a new Wasm, and nothing else. It has no path to user funds.

Because every actor is a Soroban `Address`, the same protocol works for classic
Stellar accounts (`G…`), multisig accounts and smart contracts (`C…`).

## State

### Service

```rust
pub struct Service {
    pub service_id: u64,
    pub provider: Address,
    pub rate_per_call: i128,   // smallest unit of the escrow token
    pub is_active: bool,
    pub endpoint_hash: BytesN<32>,
}
```

A service is *not* an endpoint URL and *not* a credential. It is a price and a
commitment. `endpoint_hash` is a 32-byte digest of the provider's off-chain
descriptor, so the on-chain record proves what the endpoint *should* be without
publishing anything secret.

### Lease

```rust
pub struct Lease {
    pub lease_id: u64,
    pub service_id: u64,
    pub consumer: Address,
    pub allocated_calls: u32,
    pub used_calls: u32,
    pub expires_at: u64,       // unix seconds, ledger time
    pub locked_deposit: i128,  // rate_per_call * allocated_calls
    pub settled: bool,
}
```

## Lifecycle

```mermaid
stateDiagram-v2
    [*] --> Active: create_lease / create_lease_for<br/>(escrow rate × calls)
    Active --> Expired: now ≥ expires_at
    Active --> Settled: settle_lease<br/>(verified_calls == allocated_calls)
    Expired --> Settled: settle_lease<br/>(provider, within grace)
    Expired --> Revoked: revoke_expired_lease<br/>(consumer, after 24h grace)
    Settled --> [*]
    Revoked --> [*]
```

`lease_status` derives the state from `settled`, `expires_at` and the lease's
revocation marker:

| Status | Discriminant | Condition |
| --- | --- | --- |
| `Active` | `0` | not settled, `now < expires_at` |
| `Expired` | `1` | not settled, `now ≥ expires_at` |
| `Settled` | `2` | `settled == true`, provider was paid |
| `Revoked` | `3` | `settled == true`, consumer reclaimed the deposit |

Discriminants are **stable on the wire**. They will never be renumbered, and
`Revoked` was appended as `3` rather than replacing `Settled`, so a client
written before the change still decodes every pre-existing lease. The two
closed states share the same stored `settled` flag — `Lease` layout is frozen —
and are told apart by the `LeaseRevoked(lease_id)` marker the registry writes
when a consumer reclaims.

### Two ways to close a lease, one stored flag

`settled` records that escrow has left the contract — it does not record *why*.
Both terminal transitions set it: `settle_lease` after paying the provider, and
`revoke_expired_lease` after refunding the consumer in full.
`Error::LeaseAlreadySettled` (7) keeps that combined meaning: either way the
escrow is gone, so neither path may run twice.

Why it *left* is recorded separately, in the `LeaseRevoked(lease_id)` marker
that `revoke_expired_lease` writes before the refund. It is a second key rather
than a `Lease` field because the stored struct is part of the decoding ABI: a
new field would stop live leases from decoding. `lease_status` reads the flag,
the marker and the clock, and so answers "did the provider earn?" directly:

| Lifecycle event | `settled` | `used_calls` | `lease_status` |
| --- | --- | --- | --- |
| `create_lease` | `false` | `0` | `Active` |
| `settle_lease(320)` of 500, after expiry | `true` | `320` | `Settled` |
| `settle_lease(0)` of 500, after expiry | `true` | `0` | `Settled` |
| `revoke_expired_lease` after the grace window | `true` | `0` | `Revoked` |

One row still needs the event to be read fully: `settle_lease(0)` — the
provider's legitimate way of refunding a lease it could not serve — is
`Settled` with `used_calls == 0`, so the status says "no revenue" without
naming it as a refund. `lease_settled` carries `payout = 0` and `refund` for
that lease, and it is the record an accountant should trust anyway.

The full state-by-event table, including `remaining_calls` and
`locked_balance`, is in
[`LeaseStatus` and the `settled` flag](contract-reference.md#leasestatus-and-the-settled-flag),
and how to consume the two events is in
[Following the event stream](integration.md#following-the-event-stream).

### When can a lease be settled?

`settle_lease` reverts with `LeaseNotSettleable` unless **either**:

- the lease has expired (`now ≥ expires_at`), **or**
- `verified_calls == allocated_calls`.

The second clause is the "consumer got what it paid for" fast path: if every
allocated call was consumed, the provider should not have to wait for expiry to
be paid.

### Early cancellation with provider consent

A consumer that stops using a service does not have to wait for expiry plus the
24-hour grace window. They can ask the provider to end the lease early:

1. `request_early_cancel(consumer, lease_id)` — consumer auth; requires an
   unsettled, unexpired lease and no pending request. Moves no funds.
2. The provider either `reject_early_cancel(provider, lease_id)` (request
   cleared, lease unchanged) or `approve_early_cancel(provider, lease_id,
   verified_calls)` (report served calls and release funds immediately).
3. The consumer may `withdraw_early_cancel(consumer, lease_id)` at any time.

Approval is a consented early settlement. The refund is defined exactly:

```text
payout = rate_per_call × verified_calls
unused_calls = allocated_calls − verified_calls
refund = rate_per_call × unused_calls
```

Every step uses checked arithmetic and `payout + refund` must equal
`locked_deposit`. The rate is an integer price per whole call, so there is no
division and **no rounding**: the refund is the exact value of the unused calls.

Approval settles the lease, emits `lease_cancelled { lease_id, provider,
consumer, verified_calls, payout, refund }`, and removes the request. The lease
can no longer be settled or revoked. A request cannot be filed after expiry — the
post-expiry revoke path remains the fallback when a provider is unresponsive —
but a request filed before expiry may be approved after it.

### The 24-hour grace window

After expiry there is a 24-hour window (`SETTLE_GRACE_PERIOD_SECS = 86_400`)
during which **only the provider** can settle. Once it elapses, the consumer may
call `revoke_expired_lease` and reclaim the entire deposit.

```text
expires_at ────────────────────┬─────────────────────────────┬──────────────▶ time
                               │                             │
                        expiry reached              expires_at + 24h
                               │                             │
        provider may settle for the whole window    consumer may revoke
                               │                             │
                        ───────┴──── provider grace ─────────┘
```

Why a grace window at all? Without it, a provider that was offline for a minute
at expiry would lose a legitimate payout to a consumer racing to revoke. The
window gives providers a guaranteed settlement period while keeping the
consumer's exit bounded at 24 hours.

### Exact grace-window boundaries

The contract evaluates time against ledger timestamp (`env.ledger().timestamp()`).
The exact inclusive and exclusive boundary conditions are:

| Condition / Interval | Status (`lease_status`) | `settle_lease` (Provider) | `revoke_expired_lease` (Consumer) | Allowed Transitions |
| --- | --- | --- | --- | --- |
| `now < expires_at` | `Active` (0) | Allowed **only if** `verified_calls == allocated_calls`; otherwise reverts (`LeaseNotSettleable`) | Reverts (`LeaseNotExpired`) | Early full-consumption settlement only |
| `now == expires_at` | `Expired` (1) | Allowed for any `verified_calls <= allocated_calls` | Reverts (`RevokeWindowNotElapsed`) | Provider settlement begins |
| `expires_at <= now < expires_at + 86_400` | `Expired` (1) | Allowed for any `verified_calls <= allocated_calls` | Reverts (`RevokeWindowNotElapsed`) | Exclusive provider settlement window |
| `now >= expires_at + 86_400` | `Expired` (1) | Allowed if not yet revoked; reverts (`LeaseAlreadySettled`) once revoked | Allowed; refunds 100% of locked deposit to consumer | Consumer revocation enabled (first revocable second is `expires_at + 86_400`) |

### Boundary verification tests

These boundary conditions are unit-tested and asserted in
[`contracts/registry/src/test.rs`](https://github.com/KeyLease-Security/keylease-core/blob/main/contracts/registry/src/test.rs):

- `test_revoke_before_expiry_is_rejected`: verifies that when
  `now < expires_at`, `revoke_expired_lease` returns
  `Err(Error::LeaseNotExpired)`.
- `test_full_usage_can_settle_before_expiry`: confirms early settlement when
  `verified_calls == allocated_calls` even when `now < expires_at`.
- `test_provider_can_settle_within_grace_window`: confirms provider payout at
  `expires_at + GRACE / 2` (12 hours into grace).
- `test_revoke_during_grace_window_is_rejected`: asserts `revoke_expired_lease`
  reverts with `Err(Error::RevokeWindowNotElapsed)` at `expires_at + GRACE - 1`
  (the final second of provider grace).
- `test_expired_unsettled_lease_is_fully_reclaimed`: verifies consumer revocation
  succeeds at exactly `expires_at + GRACE`, returning 100% of escrowed deposit.
- `test_lease_ttl_ask_floors_once_the_grace_window_is_past`: ensures storage rent
  bumps clamp gracefully after the grace period has elapsed.

### Who may act, and what each path writes

The arrows themselves — including the two transfers on the settlement path — are
drawn in [Architecture — The money paths in order](architecture.md#the-money-paths-in-order).
What the timing needs alongside it is the pairing of authorization and state
write, because that pairing is what makes the deadline safe to race: every
transition is recorded before money moves, so the actor that arrives second
reverts instead of paying twice.

| Transition | Authenticates | Timing required | Written before any token call | Money moved | Error if too early |
| --- | --- | --- | --- | --- | --- |
| `settle_lease` | provider, and it must own the service | expired, **or** `verified_calls == allocated_calls` | `used_calls`, `settled = true` | payout + refund | `LeaseNotSettleable` (8) |
| `revoke_expired_lease` | consumer, and it must be the lease's | `now ≥ expires_at + SETTLE_GRACE_PERIOD_SECS` | `settled = true` only | full `locked_deposit` | `LeaseNotExpired` (9), then `RevokeWindowNotElapsed` (10) |

Two consequences worth stating plainly:

- A revoke leaves `used_calls` at `0`, so a revoked lease is book-keeping-wise a
  lease that was never paid for; a settlement always satisfies
  `payout + refund == locked_deposit`. The same ambiguity, with the read that
  separates the two paths, is tabulated in
  [`LeaseStatus` and the `settled` flag](contract-reference.md#telling-the-two-paths-apart).
- The grace window ends the provider's *guarantee*, not the provider's *option*:
  `settle_lease` has no upper deadline, so after `expires_at + 24h` either actor
  can still finish the lease and the first transaction to land wins. That is why
  consumers are told to act, not to assume the refund is automatic.

## Events

Every transition publishes a typed Soroban event. Event names are the first
topic, followed by the id fields marked `#[topic]`; the payload is a map of the
remaining fields. Each payload also carries `schema_version`, so a stream that
spans a contract upgrade stays decodable (see
[Integration → Event schema version](integration.md#event-schema-version)).

| Event | Topics | Data |
| --- | --- | --- |
| `registry_initialized` | — | `schema_version`, `admin` |
| `escrow_token_set` | — | `schema_version`, `admin`, `token` |
| `service_registered` | `service_id` | `schema_version`, `provider`, `rate_per_call`, `endpoint_hash` |
| `service_status_changed` | `service_id` | `schema_version`, `provider`, `is_active` |
| `lease_created` | `lease_id` | `schema_version`, `service_id`, `consumer`, `allocated_calls`, `expires_at`, `locked_deposit` |
| `lease_cancellation_requested` | `lease_id` | `schema_version`, `consumer` |
| `lease_cancellation_rejected` | `lease_id` | `schema_version`, `provider` |
| `lease_cancellation_withdrawn` | `lease_id` | `schema_version`, `consumer` |
| `lease_cancelled` | `lease_id` | `schema_version`, `provider`, `consumer`, `verified_calls`, `payout`, `refund` |
| `lease_delegated` | `lease_id` | `schema_version`, `delegate`, `consumer` |
| `lease_settled` | `lease_id` | `schema_version`, `provider`, `verified_calls`, `payout`, `refund` |
| `lease_revoked` | `lease_id` | `schema_version`, `consumer`, `refund` |
| `contract_upgraded` | — | `schema_version`, `admin`, `new_wasm_hash` |

`lease_delegated` follows `lease_created` only for a sponsored lease, so
indexers that ignore it still see every lease.

Events are the indexing contract between on-chain state and off-chain systems: a
provider can settle from a projection built by subscribing to `lease_created`,
and accounting can reconcile payouts from `lease_settled` without polling every
lease. See [Integration](integration.md).

## Worked economics

All figures use the escrow token's smallest unit (7 decimals for the native
asset, so `1 XLM = 10_000_000`).

Suppose a provider registers a service at `rate_per_call = 1_000_000` (0.1 XLM
per call), and a consumer buys 500 calls for 1 hour.

**1. `create_lease`**

```text
required_deposit = rate_per_call × requested_calls
                 = 1_000_000 × 500
                 = 500_000_000        (50 XLM)
```

The registry transfers `500_000_000` from the consumer into escrow and sets
`expires_at = now + 3600`.

**2a. `settle_lease` after 1 hour with 320 calls consumed**

```text
payout = 1_000_000 × 320 = 320_000_000   (32 XLM to the provider)
refund = 500_000_000 − 320_000_000
       = 180_000_000                     (18 XLM back to the consumer)
```

**2b. Provider goes dark; consumer calls `revoke_expired_lease` after 24h**

```text
refund = locked_deposit = 500_000_000    (50 XLM, the full deposit)
```

**2c. Consumer burns all 500 calls in 10 minutes**

`settle_lease` is immediately allowed (`verified_calls == allocated_calls`):

```text
payout = 1_000_000 × 500 = 500_000_000   (50 XLM)
refund = 0
```

### What a lease costs

Three different things are paid for when a lease is taken, and only one of them
comes back. The table below holds the allotment fixed at the worked example —
500 calls at `rate_per_call = 1_000_000` — and changes only the duration.

| Lease shape | `duration_secs` | Deposit in escrow | TTL the contract asks for | Effective life on Testnet |
| --- | --- | --- | --- | --- |
| Short — 1 hour | `3_600` | `500_000_000` (50 XLM) | 35,280 ledgers | 120,960 (the network floor, ≈7 days) |
| Medium — 1 day | `86_400` | `500_000_000` (50 XLM) | 51,840 ledgers | 120,960 (the network floor, ≈7 days) |
| Long — 7 days | `604_800` | `500_000_000` (50 XLM) | 155,520 ledgers | 155,520 (≈9 days) |
| Longest — 30 days | `2_592_000` | `500_000_000` (50 XLM) | 552,960 ledgers | 552,960 (≈32 days) |

**The deposit column never moves.** The deposit is `rate_per_call ×
requested_calls`, and `duration_secs` does not appear in it: the same 500 calls
cost the same 50 XLM whether the window is an hour or a month, and they are
refundable in exactly the same proportions. The deposit is only bounded above, by
`MAX_LEASE_DEPOSIT = 1_000_000_000_000` base units — two thousand times the
example above — so an ordinary lease never meets the cap.

**Duration changes the rent line.** `save_lease` extends the lease entry for its
remaining term, the 24-hour settlement grace window and a one-day read buffer,
which is the `lease_ttl_extend` column. At creation the remaining term is the whole
duration, so the ask is `(duration + 86_400) / 5 + 17_280` ledgers, clamped to
`min(MAX_ENTRY_TTL_LEDGERS, network maxEntryTtl)`. The clamp never binds a legal
lease: the longest one asks for 552,960 ledgers against a 1,555,200-ledger ceiling.
The first two rows land under the Testnet minimum persistent TTL, so an hour and a
day cost the same here — buying a longer window only starts to cost more once it
exceeds the floor.

Bumping a lease *later* is a smaller ask, because the same formula is applied to the
time still remaining rather than to the original term; see
[`bump_lease`](contract-reference.md#bump_lease).

**Counting transactions.** A normal lifecycle is two of them: `create_lease` and
`settle_lease`. The refund path is also two: `create_lease` and
`revoke_expired_lease`. There is no per-call transaction — the provider settles
with a single `verified_calls` figure, which is what makes small allotments
worthwhile.

### Which of those costs come back

| Cost | Paid by | Paid when | Refundable |
| --- | --- | --- | --- |
| Token deposit | consumer | `create_lease` | Partly: `locked_deposit − payout` on settlement, `payout` being what the provider actually served |
| Token deposit, undisbursed | consumer | `create_lease` | Fully, 100%: on `revoke_expired_lease` after the grace window, and on a settlement with `verified_calls = 0` |
| Soroban transaction fee | whoever sends the call | every transaction | No. It pays for block space and goes to the network |
| Soroban storage rent | whoever makes the write that extends the entry | `create_lease`, then again at settle or revoke | No, and it is prepaid. Settling an hour into a thirty-day lease does not refund the ledgers you will never use |

So the consumer's token exposure is always reversible — the protocol's whole
point — while the network's fees are spent the moment the transaction lands.
Rent is charged again at settlement because settling rewrites the lease entry,
which is one reason the provider, not the consumer, is expected to call it.

### What this page will not tell you

Rent in XLM is not a contract constant. It is the entry's byte size times the
number of ledgers it is kept alive, times a per-ledger price the *network*
sets, and that price differs between Testnet and mainnet. The only figures in
this repository that were actually measured are the Testnet ones in
[deployments](deployments.md#deployment-economics-testnet), and they include a
minimum-TTL floor that mainnet does not share. Size a real allotment against the
fees of the network you deploy to — `stellar network settings`, or your RPC
provider — before quoting a per-call price.

## Invariants

These must hold for every reachable state. They are asserted in tests.

1. **Conservation.** For every lease, `payout + refund == locked_deposit`.
2. **Single release.** A settled lease can never be settled or revoked again
   (`LeaseAlreadySettled`), and `settled` is persisted before any token call.
3. **Bounded claim.** A provider can never receive more than
   `allocated_calls × rate_per_call`, and only for a service it owns.
4. **Guaranteed exit.** Every lease has a consumer path to a full refund.
5. **Checked math.** Deposit, payout, refund and timestamp arithmetic abort on
   overflow rather than truncating.
6. **Consented early exit.** An early cancellation releases `rate × served_calls`
   to the provider and `rate × unused_calls` to the consumer, with
   `payout + refund == locked_deposit`, and only after the provider approves.
7. **Claimable storage.** A lease's persistent entry outlives its term plus the
   grace window (`MAX_LEASE_DURATION_SECS = 30 days`), and so does the
   revocation marker recorded against it.
8. **Funded, computable prices.** `register_service` rejects `rate_per_call`
   above `MAX_RATE_PER_CALL`, and `create_lease` rejects a
   `rate_per_call × requested_calls` above `MAX_LEASE_DEPOSIT`. The two bounds
   are the same number, so every listed price can fund at least a one-call lease
   and no accepted lease can quote a deposit `i128` cannot hold.
9. **Refund ownership.** Whoever created a lease — consumer or delegate — the
   **consumer** is debited, is the refund recipient, and is the only address
   that can revoke.
10. **Distinct closure.** A lease closed by the provider reads `Settled`; one
    reclaimed by the consumer or ended by a cancellation the provider declined
    reads differently from revenue. Only leases that paid the provider enter
    `service_stats`.

## Storage and lifetime

| Key | Storage | Notes |
| --- | --- | --- |
| `Admin`, `Token` | instance | configuration; instance TTL is bumped on every state change |
| `ServiceCount`, `LeaseCount` | instance | monotonic id counters |
| `Service(u64)` | persistent | TTL extended on save, on `create_lease` and by `bump_service`, never by a read |
| `Lease(u64)` | persistent | TTL extended on save and by `bump_lease`, never by a read |
| `ProviderServices(Address)` | persistent | service-id index per provider; extended by `save_service`, never by a read |
| `CancelRequest(u64)` | persistent | pending early-cancellation request; cleared on withdraw/reject/approve |

`LEDGER_BUMP_THRESHOLD = 17_280` ledgers (~1 day at 5s ledgers) and
`LEDGER_BUMP_EXTEND = 518_400` ledgers (~30 days). For leases, `save_lease` and
`bump_lease` extend to `(expires_at − now + 24h) / 5s + LEDGER_BUMP_THRESHOLD`
ledgers, so the escrow stays claimable for the whole lease, the provider's grace
window and a one-day read buffer — without paying rent for unused storage time.
Once a lease is past its term and grace window that ask settles at about two
days, so keeping a dead lease alive costs rent per bump rather than per month.

Rent scales with that ask: at the ledger where an extension is due, `bump_lease`
estimates 48,684 stroops of persistent rent for a 30-day-term lease against zero
for `get_lease`, which is why reads do not extend. `MAX_ENTRY_TTL_LEDGERS`
(1,555,200 ledgers ≈ 90 days) caps any single extension, and the cap is also
taken against the network's live `maxEntryTtl`, so an entry's horizon is never
derived from stored `expires_at` alone.

### The provider index

`ProviderServices(provider)` holds that provider's `service_id`s, oldest first.
Its lifetime rules are the service rules, not a new third policy:

- **Written with the record.** `save_service` appends to the index and extends it
  in the same step it stores the `Service`, with the same clamped horizon, so an
  entry and the index row that points at it cannot drift apart in rent terms.
- **Refreshed by any write to the catalogue, not by reading it.** `save_service`
  also runs on `set_service_active`, whose id is already in the index, so pausing
  or resuming one service keeps the whole catalogue alive. Nothing on the read path
  extends anything, this entry included — see the module doc in
  `contracts/registry/src/storage.rs` for the rent measurement behind that rule.
- **Never bumped for the listed services.** Paging reads each `Service` record to
  re-check ownership but does not extend it: a page is a discovery read over
  entries somebody else owns, so their rent stays with their owner. A caller
  cannot keep another provider's records alive, nor evict them.
- **Append-only, pruned on read.** The index is derived state; `Service.provider`
  is the authority. If a service is later transferred, its id stays in the old
  owner's row and the read path drops it, while `save_service` puts it in the new
  owner's row. An index can therefore only ever make a page shorter — never
  disclose a service that belongs to someone else. The stale id costs one ledger
  read to skip, and a provider that re-registers does not duplicate it.

If the index entry expires while its services live on, the provider's pages go
empty until the next write to any of its services recreates the row. That is a
discovery-availability failure, not a loss of funds or of ownership, which is why
the index is safe to treat as cache. `bump_service` does not refresh it: that call
takes a `service_id` and extends one record without loading it, and finding the
index key means reading the record — the cost it exists to avoid. A provider that
needs a durable index across months of inactivity is the case an explicit index
bump would serve, and it is not implemented here.

## Read next

- [Contract reference](contract-reference.md) — every entry point, parameter and error.
- [Security model](security.md) — the trust assumptions behind these invariants.
