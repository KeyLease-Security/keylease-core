# Design note — Provider staking and slashing (#137)

> **Status:** Proposed — **awaiting maintainer approval**. No contract code is
> changed by this note. Per [CONTRIBUTING](https://github.com/KeyLease-Security/keylease-core/blob/main/CONTRIBUTING.md),
> a maintainer must approve this design before implementation begins.
>
> **Issue:** [#137 — Add provider staking and slashing for proven over-reporting](https://github.com/KeyLease-Security/keylease-core/issues/137)
> **Target release:** v0.3.0 · **Touches:** `lib.rs`, `storage.rs`, `types.rs`, `events.rs`, `errors.rs`

## 1. Problem statement

`settle_lease` trusts the provider's `verified_calls`. The allotment caps the
damage per lease, and the consumer can reclaim an unsettled deposit after the
grace window, but a provider that reports more calls than it actually served
keeps the over-payment. The only penalty is that the consumer does not come
back. There is no on-chain cost to over-reporting within `allocated_calls`, so
the protocol's central trust assumption — "the provider reports honestly" — has
no economic backstop.

This note proposes an **opt-in provider stake** that a service must post before
it accepts leases, plus a **bounded, evidence-based slashing process** for proven
over-reporting.

### Goals

- Give consumers a recourse that is funded by the provider, not by other users.
- Make the penalty proportional to the over-report and hard-capped, so the
  mechanism cannot be used to confiscate a provider's business.
- Keep staking **opt-in**: existing services and deployments keep working
  unchanged, and unstaked services continue to be leaseable.
- Never delay, block or alter settlement or the consumer refund path.
- Add no unbounded loops, no new trust in off-chain data, and no change to the
  existing `Service`/`Lease` storage layout.

### Non-goals

- Proving real HTTP usage on-chain. The contract cannot observe traffic; the
  design constrains *who* may accuse, *how much* may be slashed and *where the
  money goes*, and relies on a public evidence hash plus an arbiter.
- Insuring consumers for non-delivery. That remains the refund path plus, now,
  a stake.
- A general arbitration court, appeals beyond one challenge round, or slashing
  for anything other than over-reporting.

## 2. Proposed architecture

### 2.1 Stake custody

Stake is held by the registry in the **same SEP-41 token** as escrow, but
accounted for separately. Escrow accounting is per lease (`locked_deposit`);
stake accounting is per provider. The contract must never infer one from the
token balance — `sum(locked deposits) + sum(stakes) == token balance` is an
invariant the tests assert; the token balance itself is never used as a source
of truth.

Stake is associated with the **provider address** (`Service.provider`), not the
service id. One pool covers every service that provider opts in, which avoids
duplicating capital across services and keeps the slash/withdraw bookkeeping
bounded. Revenue-split recipients (#133) are payees only; they are **not**
stakers and cannot be slashed.

### 2.2 Opt-in per service

A provider opts a service into staking with `opt_in_service_stake`. The opt-in
is a separate storage key (`ServiceStake(service_id)`) rather than a new field
on `Service`, so existing `Service` entries keep decoding and the storage layout
is upgrade-safe. Opting out only blocks **new** leases.

`create_lease` enforces the invariant for opted-in services:

```text
if service is staked:
    provider stake must exist and available_stake >= min_stake
    the provider must not have a withdrawal pending
```

If either check fails the lease reverts (`InsufficientStake` /
`StakeWithdrawalPending`). Existing services that never opt in are unaffected;
no new lease on an unstaked service ever requires stake.

### 2.3 Minimum and configurable requirements

The admin configures a single `StakeConfig` in instance storage:

| Field | Meaning | Default | Bound |
| --- | --- | --- | --- |
| `min_stake` | Minimum `available` stake an opted-in service needs at lease creation | — | `> 0` |
| `dispute_bond` | Bond an accuser posts to open a dispute | — | `> 0` |
| `dispute_window_secs` | How long after a lease's dispute anchor a dispute may be opened | — | 1 h – 30 days |
| `challenge_period_secs` | Provider response window before resolution | — | 1 h – 7 days |
| `unbonding_period_secs` | Delay between withdrawal request and release | — | `>= dispute_window + challenge_period`, max 30 days |
| `max_slash_bps` | Hard cap on the share of a provider's stake one dispute may slash | — | 1 – 10 000 bps |

Staking is **disabled until the admin sets a config**; `opt_in_service_stake`
and `open_dispute` revert with `StakeNotConfigured` before then. This means an
upgraded deployment behaves exactly like today until the operator opts the
protocol in. The admin can raise/lower `min_stake` later; lowering it takes
effect immediately for new leases, raising it never invalidates existing leases.

### 2.4 Evidence and slashing authority

The contract cannot validate usage data, so "proven" is defined procedurally.
A dispute may only be opened by the **lease's consumer** — the party that paid
and therefore has standing — and only against a **settled** lease of a staked
service, within the dispute window.

**Admissible evidence.** The contract stores only a 32-byte commitment
(`evidence_hash`) and emits it. The evidence bundle is published off-chain in a
documented format so any third party can reproduce the verdict:

- the lease id, service id and settlement transaction hash,
- the provider's reported `verified_calls`,
- per-call records the provider signed or that the consumer can verify
  (signed request logs, provider receipts, or the provider's own metering
  export),
- a manifest hash over those artifacts, which is the `evidence_hash`.

The provider may answer once with `contest_dispute`, supplying its own
`counter_evidence_hash`. Both hashes and the final verdict are permanent event
data.

**Who may slash.**

1. The **consumer** opens the dispute and posts `dispute_bond`.
2. The provider may contest during `challenge_period_secs`.
3. The **arbiter** — a dedicated role, defaulting to the admin and rotatable by
   the admin — resolves after the challenge period with `resolve_dispute`.
4. Only then, and only if approved, is stake moved.

**Bounds that prevent arbitrary confiscation.**

- Only a consumer with a settled lease can accuse; the arbiter cannot open a
  dispute against a provider it dislikes.
- The disputed amount is bounded by the on-chain over-report:
  `overreported_calls * rate_per_call`, where
  `overreported_calls <= lease.allocated_calls`.
- The slash is additionally capped at `stake_total * max_slash_bps / 10_000`
  and can never exceed the provider's remaining stake.
- Slashed value is paid to the **accusing consumer**, never to the admin, the
  arbiter or the registry. The arbiter therefore cannot profit from a ruling.
- A rejected dispute returns nothing to the accuser: the bond is paid to the
  provider as compensation for the false accusation, and the provider's stake is
  untouched.
- Every step emits an event with the evidence hashes and amounts; the arbiter
  role and config are admin-visible and rotatable.

A single challenge round is deliberately the whole procedure. It is a bounded
governance process, not a court: the cap, the consumer-only standing and the
"proceeds to the accuser" rule are what make it safe to keep small.

### 2.5 Withdrawal rules

Stake leaves in two steps:

1. `request_stake_withdrawal(provider, amount)` requires:
   - the provider has opted **out** of every service (`staked_services == 0`),
   - no unsettled leases are attributable to the provider
     (`open_leases == 0`),
   - no dispute against the provider is pending (`pending_disputes == 0`),
   - `amount <= available`, and `amount > 0`.
   It moves `amount` from `available` to `unbonding` and sets
   `unbonding_until = now + unbonding_period_secs`. A withdrawal request also
   makes `create_lease` revert with `StakeWithdrawalPending` for that provider's
   services, so no new exposure can be created while capital is leaving.
2. `withdraw_stake(provider)` requires `now >= unbonding_until`,
   `open_leases == 0` and `pending_disputes == 0`, then transfers the unbonding
   amount to the provider.

Because `unbonding_period_secs >= dispute_window_secs + challenge_period_secs`,
every settlement that could still be disputed has either been disputed or is
past its window before the capital is released, and unbonding stake remains
slashable until the transfer happens. A new `deposit_stake` during unbonding
cancels the request (unbonding returns to `available`, the timer clears), which
is safe because the funds never left the contract.

`pending_disputes` and `open_leases` are per-provider counters, incremented and
decremented on the paths that create and close them, so withdrawal never scans a
collection.

### 2.6 Settlement and service lifecycle

- Settlement and revocation are **unchanged**: stake never gates
  `settle_lease` or `revoke_expired_lease`, and the consumer's exit remains
  guaranteed. A provider that has been slashed can still settle leases it
  already sold.
- Slashing is paid from the provider's stake, not from lease escrow, so
  `payout + refund == locked_deposit` still holds for every lease.
- If a slash drops `available` below `min_stake`, the service is **effectively
  deactivated** without writing to `Service`: the next `create_lease` reverts
  `InsufficientStake`. This avoids iterating a provider's services to flip
  `is_active`, keeps `Service` layout-compatible, and makes topping the stake up
  an immediate recovery. `set_service_active(false)` remains available for an
  explicit pause, and a provider can always opt out.
- A provider's opt-in/opt-out never changes the terms of an existing lease;
  those leases settle under the rules in force when they were created.

## 3. Storage changes

All additions are new `DataKey` variants. `Service`, `Lease`, `LeaseStatus`
and every existing key keep their exact wire encoding.

| Key | Storage | Value | TTL policy |
| --- | --- | --- | --- |
| `StakeConfig` | instance | `StakeConfig` | instance TTL, bumped on every state change |
| `Arbiter` | instance | `Address` | instance TTL |
| `Stake(Address)` | persistent | `StakeAccount` | extended on read/write, 30-day horizon |
| `ServiceStake(u64)` | persistent | `bool` | extended with the service, 30-day horizon |
| `DisputeCount` | instance | `u64` | instance TTL |
| `Dispute(u64)` | persistent | `Dispute` | extended on read/write; retained through resolution + 30 days |

```rust
pub struct StakeAccount {
    pub available: i128,
    pub unbonding: i128,
    pub unbonding_until: u64,
    pub open_leases: u32,      // leases of this provider not yet settled/revoked
    pub staked_services: u32,  // services currently opted in
    pub pending_disputes: u32, // disputes not yet resolved
}

pub struct Dispute {
    pub dispute_id: u64,
    pub lease_id: u64,
    pub service_id: u64,
    pub accuser: Address,       // lease.consumer
    pub provider: Address,      // service.provider
    pub evidence_hash: BytesN<32>,
    pub counter_evidence_hash: BytesN<32>, // zeroed until contested
    pub overreported_calls: u32,
    pub slash_amount: i128,     // computed and frozen at open time
    pub bond: i128,
    pub opened_at: u64,
    pub status: DisputeStatus,  // Open = 0, Contested = 1, Approved = 2, Rejected = 3
}

pub struct StakeConfig {
    pub min_stake: i128,
    pub dispute_bond: i128,
    pub dispute_window_secs: u64,
    pub challenge_period_secs: u64,
    pub unbonding_period_secs: u64,
    pub max_slash_bps: u32,
}
```

`slash_amount` is frozen at `open_dispute` so topping up the stake, or a second
dispute, cannot change what the first one may take. The bounded cap is
re-evaluated against the balance at resolution and the payout is the smaller of
the frozen amount and the remaining stake.

## 4. Public interface changes

### Entry points

| Entry point | Auth | Description |
| --- | --- | --- |
| `set_stake_config(admin, config)` | admin | Set/update the staking configuration. Emits `stake_config_set`. |
| `set_arbiter(admin, arbiter)` | admin | Rotate the dispute arbiter. Emits `arbiter_set`. |
| `deposit_stake(provider, amount)` | provider | Transfer `amount` from the provider into the stake pool; cancels a pending withdrawal. Emits `stake_deposited`. |
| `request_stake_withdrawal(provider, amount)` | provider | Start unbonding. Emits `stake_withdrawal_requested`. |
| `cancel_stake_withdrawal(provider)` | provider | Return unbonding to available before release. Emits `stake_withdrawal_cancelled`. |
| `withdraw_stake(provider)` | provider | Release unbonding stake after the delay. Emits `stake_withdrawn`. |
| `opt_in_service_stake(provider, service_id)` | provider | Require stake for new leases of the service. Emits `service_stake_changed`. |
| `opt_out_service_stake(provider, service_id)` | provider | Stop requiring stake for new leases. Emits `service_stake_changed`. |
| `open_dispute(consumer, lease_id, evidence_hash, overreported_calls)` | consumer | Post the bond and open a dispute on a settled lease. Emits `dispute_opened`. |
| `contest_dispute(provider, dispute_id, counter_evidence_hash)` | provider | Answer once inside the challenge period. Emits `dispute_contested`. |
| `resolve_dispute(arbiter, dispute_id, approve: bool)` | arbiter | After the challenge period, slash or reject. Emits `dispute_resolved` and `provider_slashed`. |
| `get_stake(provider)` | — | `Result<StakeAccount, Error>` |
| `get_stake_config()` | — | `Result<StakeConfig, Error>` |
| `get_dispute(dispute_id)` | — | `Result<Dispute, Error>` |
| `is_service_staked(service_id)` | — | `bool` |
| `stake_requirement(service_id)` | — | `Result<Option<i128>, Error>`: `min_stake` when staked, otherwise `None` |

### Events

| Event | Topics | Data |
| --- | --- | --- |
| `stake_config_set` | — | `admin`, `min_stake`, `dispute_bond`, `dispute_window_secs`, `challenge_period_secs`, `unbonding_period_secs`, `max_slash_bps` |
| `arbiter_set` | — | `admin`, `arbiter` |
| `stake_deposited` | `provider` | `amount`, `available`, `unbonding` |
| `stake_withdrawal_requested` | `provider` | `amount`, `unbonding_until` |
| `stake_withdrawal_cancelled` | `provider` | `amount` |
| `stake_withdrawn` | `provider` | `amount`, `available` |
| `service_stake_changed` | `service_id` | `provider`, `staked` |
| `dispute_opened` | `dispute_id` | `lease_id`, `service_id`, `accuser`, `evidence_hash`, `overreported_calls`, `slash_amount`, `bond` |
| `dispute_contested` | `dispute_id` | `provider`, `counter_evidence_hash` |
| `dispute_resolved` | `dispute_id` | `approved`, `slash_amount`, `bond_to_provider`, `evidence_hash` |
| `provider_slashed` | `provider` | `amount`, `remaining_available`, `remaining_unbonding` |

`service_registered`, `service_status_changed`, `lease_created`,
`lease_settled` and `lease_revoked` keep their exact payloads; the stake checks
add no field to a lease.

### Error codes (appended, never renumbered)

| Code | Variant | Meaning |
| --- | --- | --- |
| 17 | `StakeNotConfigured` | Staking has not been enabled by the admin |
| 18 | `InvalidStakeAmount` | Zero/negative stake, bond or config value |
| 19 | `InsufficientStake` | Provider's available stake is below `min_stake` |
| 20 | `StakeWithdrawalPending` | Withdrawal unbonding; no new leases |
| 21 | `WithdrawalNotReady` | Unbonding period has not elapsed |
| 22 | `PendingDispute` | A pending dispute blocks the action |
| 23 | `ServiceNotStaked` | Dispute against a service that did not opt in |
| 24 | `ServiceAlreadyStaked` | Opt-in on an already-staked service |
| 25 | `OutstandingLeases` | Unsettled leases block withdrawal |
| 26 | `NoStake` | No stake account for the provider |
| 27 | `DisputeWindowClosed` | Dispute opened too late (or lease not settled) |
| 28 | `ChallengePeriodActive` | Resolution attempted before the provider's window closed |
| 29 | `DisputeClosed` | Dispute already resolved |
| 30 | `InvalidDispute` | Wrong accuser, over-report bound, or duplicate contest |
| 31 | `InvalidStakeConfig` | Config outside the documented bounds |
| 32 | `NothingToWithdraw` | No unbonding stake to release |

## 5. Authorization and governance model

| Role | Can do | Cannot do |
| --- | --- | --- |
| Provider | Deposit, opt in/out, request/cancel/complete withdrawal, contest a dispute, settle its leases | Slash, resolve disputes, withdraw while exposed |
| Consumer | Open a dispute on its **settled** lease after posting the bond | Open disputes on others' leases, slash directly |
| Arbiter | Resolve a dispute after the challenge period | Open disputes, change config, touch escrow, keep slashed value |
| Admin | Set config, rotate the arbiter, upgrade the Wasm | Slash, resolve disputes, move stake or escrow |

The admin and arbiter should be separate keys (both cold/multisig) in
production; the default of "arbiter = admin" exists so a fresh deployment works
without a second key, and the role is rotatable and announced by `arbiter_set`.
Governance is deliberately narrow: the only discretionary decision in the
protocol is the one-bit `approve` in `resolve_dispute`, and its consequences are
capped, recipient-fixed and permanently logged.

## 6. Backward compatibility

- **Storage:** only new keys. `Service` and `Lease` are untouched, so a v0.3.0
  Wasm can be installed over a v0.2.x deployment with live services and leases.
- **Existing services:** remain unstaked and continue to accept leases with no
  stake requirement. Nothing becomes mandatory.
- **Existing leases:** unchanged terms; settlement, refund and revocation do
  not consult stake.
- **ABI:** all new entry points, events and error codes are additive. No error
  number is reused and no event payload changes.
- **Default state:** staking is inert until `set_stake_config` is called, so an
  upgraded deployment behaves identically to v0.2.x.
- **Events:** the existing event stream for lease lifecycle is byte-identical;
  stake and dispute events are new names.

## 7. Security considerations

**Re-entrancy and ordering.** Every money path follows
checks-effects-interactions. `open_dispute` persists the dispute and the bond
before `transferFrom`-style movement (the consumer transfer happens after the
counter and bond are recorded). `resolve_dispute` sets the dispute to
`Approved`/`Rejected`, decrements `pending_disputes` and debits stake **before**
any token transfer, so a malicious recipient contract cannot re-enter for a
second slash or a second bond refund. `withdraw_stake` zeroes the unbonding
balance before the transfer. A re-entrant token would see terminal state and
revert the whole transaction.

**Storage integrity.** `open_leases` and `pending_disputes` are counters, not
collections, and are maintained on both sides of each transition
(create/settle/revoke, open/resolve). The tests assert they return to zero after
a full lifecycle. `Dispute` records are immutable after resolution except for
status. All persistent additions carry explicit TTL extensions; a stake entry is
read/written on every path that needs it, and a dispute entry is extended when
opened, contested and resolved.

**Economic attack vectors.**

| Attack | Mitigation |
| --- | --- |
| Provider over-reports and withdraws before a dispute | Unbonding ≥ dispute window + challenge period; unbonding remains slashable; withdrawal requires zero open leases and zero pending disputes; new leases are blocked while unbonding |
| Consumer grieves a provider with disputes | Bond forfeited to the provider on rejection; bounded cap; one contest round; evidence hash makes frivolous claims auditable |
| Arbiter colludes with a consumer | Slash proceeds go to the consumer, not the arbiter; per-dispute cap; admin-rotatable role; every ruling is event-logged with evidence hashes |
| Provider keeps too little stake relative to revenue | `min_stake` is admin-set and enforced on every new lease of a staked service; raising it never invalidates existing leases |
| Slash is used to seize a business | Only settled leases, consumer standing, over-report bound, `max_slash_bps` cap, one round, provider keeps all un-slashed stake and future revenue |
| Double slash on one lease | At most one dispute per lease (`open_dispute` rejects a lease that already has one, checked through a per-lease dispute index), and a terminal status |
| Stake/escrow accounting confusion | Separate internal ledgers; the token balance is never read as a source of truth; tests assert `escrow + stake == balance` |
| Griefing by never contesting | Resolution is permissionless in time but arbiter-driven; an uncontested dispute resolves on schedule |

**Governance and upgrade implications.** `StakeConfig`, `Arbiter` and the new
keys must keep their meaning across upgrades; the implementation must never
renumber errors or change the `Service`/`Lease` encoding. A malicious upgrade
can already change any rule, so the arbiter/admin keys deserve the same
cold-storage treatment as the existing admin key. Because `min_stake` and
`max_slash_bps` are live parameters, changes to them should be announced and
are visible in `stake_config_set`.

**Resource limits.** Every new path is O(1) in storage reads/writes and does a
constant number of token calls (at most two transfers in a resolution). No
entry point iterates a user-controlled collection; recipient and service lists
are never walked to enforce stake.

## 8. Testing strategy

New tests in `contracts/registry/src/test.rs` (plus the existing suite kept
green):

**Stake lifecycle**

- deposit credits `available`, moves tokens, emits `stake_deposited`, extends
  TTL;
- deposit with zero/negative amount reverts `InvalidStakeAmount`;
- opt-in requires a config and an account; opt-in/opt-out emit
  `service_stake_changed`;
- `create_lease` on a staked service reverts `InsufficientStake` below
  `min_stake` and succeeds at exactly `min_stake`;
- unstaked services keep leasing with no stake at all (backward compatibility).

**Withdrawal**

- request moves `available` to `unbonding`, blocks new leases
  (`StakeWithdrawalPending`);
- early `withdraw_stake` reverts `WithdrawalNotReady`;
- withdraw succeeds after the delay and only then moves tokens;
- request reverts while a lease is unsettled (`OutstandingLeases`), while a
  service is still opted in, and while a dispute is pending (`PendingDispute`);
- deposit during unbonding cancels the request and restores `available`.

**Disputes and slashing**

- only the lease consumer can open (`InvalidDispute` / `Unauthorized`);
- a dispute on an unstaked service reverts `ServiceNotStaked`;
- a dispute on an unsettled or out-of-window lease reverts
  `DisputeWindowClosed`;
- the bond is transferred in and the frozen `slash_amount` respects both the
  over-report bound and `max_slash_bps`;
- contest is provider-only and single-shot; resolution before the challenge
  period reverts `ChallengePeriodActive`;
- approval pays the accuser the slash plus the returned bond, debits stake
  (available first, then unbonding), emits `dispute_resolved` and
  `provider_slashed`;
- rejection pays the bond to the provider, leaves stake untouched, and clears
  the pending counter;
- double resolution reverts `DisputeClosed`; a second dispute on the same lease
  reverts;
- a re-entrant token cannot double-slash or double-refund a bond.

**Settlement and lifecycle after a slash**

- an existing lease still settles and refunds correctly after its provider is
  slashed;
- create_lease reverts `InsufficientStake` after the stake falls below
  `min_stake` (effective deactivation), and works again after a top-up;
- `opt_out_service_stake` blocks new leases but leaves existing leases
  settleable;
- `payout + refund == locked_deposit` and `escrow + stake == token balance`
  hold across the whole flow;
- the real-SAC lifecycle test and the existing re-entrancy test still pass.

## 9. Open questions and trade-offs

1. **Provider-level vs per-service stake.** A single pool is capital-efficient
   and keeps checks O(1); a per-service pool isolates a bad service from a
   provider's other business. This note chooses provider-level and lets the
   provider opt out individual services. *Rejected alternative:* a stake field
   on `Service` (breaks stored-value layout) and a per-service pool (fragments
   capital and needs per-service withdrawal bookkeeping).
2. **Arbiter discretion.** One challenge round with a capped, accuser-directed
   payout is cheap and bounded, but a corrupt arbiter can still approve a
   malicious dispute up to the cap. *Alternatives:* two-of-two
   (arbiter + admin) approval, a bonded arbiter, or a consumer/provider
   optimistic game with a fraud proof. Each adds interface and governance
   surface; we should decide before implementation whether disputes above a
   threshold need two approvals.
3. **Exposure-based stake.** The design requires `available >= min_stake`, not
   "stake proportional to open escrow". Per-lease stake would price risk better
   but needs mid-lease top-ups and makes settlement conditional on stake.
   *Question for maintainers:* should a later version require
   `min_stake >= k * open_leases`?
4. **Dispute window anchor.** Disputes are anchored at
   `expires_at + grace + dispute_window`, because `Lease` has no `settled_at`
   field and adding one would change its encoding. This grants a generous
   window to fully-consumed leases settled early. *Alternative:* add
   `SettlementTime(lease_id)` as a new key and anchor on the real settlement
   time.
5. **Bond size.** A flat `dispute_bond` is simple but may be too small for large
   services or too large for small ones. *Alternative:* a bps-of-disputed-value
   bond with a floor and a ceiling.
6. **Who audits evidence.** The on-chain record is a hash plus a verdict. If
   maintainers want verifiable evidence handling, the off-chain bundle format
   needs a documented schema and a canonical hashing rule before release.
7. **Unbonding UX.** Requiring `open_leases == 0` and full opt-out before a
   withdrawal request is conservative and can lock capital for up to a lease
   term plus the delay. *Alternative:* allow withdrawal requests with open
   leases by reserving the newest leases' exposure.
8. **Versioning.** This is a breaking-in-behaviour (not ABI) change once a
   config is set; the workspace version should move to `0.3.0` and the release
   notes must describe the governance assumptions.

## 10. Review request

Per `CONTRIBUTING.md`, this note needs maintainer approval before code:

- Approve or amend the **arbiter model** (single arbiter vs two-key approval).
- Confirm **provider-level** stake and opt-in-per-service.
- Confirm the **withdrawal precondition** `open_leases == 0` and the unbonding
  bound.
- Confirm the error-code allocation (17–32) and event names before they become
  ABI.

Once approved, implementation lands on the same feature branch as #133, with
the tests above and the full four-command validation from `CONTRIBUTING.md`.
