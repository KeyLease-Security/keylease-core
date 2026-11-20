# Security model

KeyLease escrows funds on Stellar. This page states exactly what the Soroban
contract guarantees, what it assumes, and what is not yet hardened. For
reporting a vulnerability, see
[SECURITY.md](https://github.com/KeyLease-Security/keylease-core/blob/main/SECURITY.md).

## Trust assumptions

| Assumption | Consequence if violated |
| --- | --- |
| The admin configures a well-behaved SEP-41 token. | A malicious token contract could revert transfers and stall settlement. It cannot be paid twice: `settled` is persisted before the transfer, and a re-entrant token is rejected (`LeaseAlreadySettled`). |
| The admin does not upgrade to a malicious Wasm. | An upgrade can change the contract logic. The admin key should be cold or multisig; upgrades emit `contract_upgraded`. |
| The provider reports `verified_calls` honestly. | It can under-report (it gets paid less) but never over-report past `allocated_calls`. |
| Soroban ledger time is monotonic and honest. | Validity windows would be unreliable. |
| Consumers keep their own key safe. | Anyone with the consumer key can create leases that spend its balance. |

The provider is explicitly **untrusted** in the direction that matters: it cannot
take more than it is owed, and it cannot prevent the consumer's refund.

## Guarantees

1. **The provider is paid at most `allocated_calls × rate_per_call`.** The payout
   is computed from the stored `rate_per_call` and the lease allotment; an
   over-claim reverts with `InvalidCalls`.
2. **Only the owning provider can settle.** `settle_lease` checks
   `service.provider == provider` after authenticating `provider`
   (`Unauthorized`).
3. **Only the consumer can revoke, and only after the grace window.** Combined
   checks for ownership, expiry and elapsed grace (`Unauthorized`,
   `LeaseNotExpired`, `RevokeWindowNotElapsed`).
4. **A lease releases funds once.** Every money path checks `settled` first,
   persists `settled = true`, and only then calls the token
   (`LeaseAlreadySettled`). Checks-effects-interactions is enforced by
   `test_reentrant_token_cannot_settle_a_lease_twice`.
5. **Overflow aborts.** Deposit, payout and expiry arithmetic use
   `checked_mul`/`checked_add`/`checked_sub` and revert with `Overflow`.
6. **Lease durations are storage-safe.** Durations above
   `MAX_LEASE_DURATION_SECS` (30 days) revert with `DurationTooLong`, and the
   persistent entry is extended for the term plus the grace window, so escrow
   cannot be evicted while it is claimable.
7. **A single lease cannot escrow an unbounded amount.** A deposit that fits
   `i128` but exceeds `MAX_LEASE_DEPOSIT` reverts with `DepositTooLarge`, before
   the lease id is allocated and before the token is called. Checked arithmetic
   alone cannot catch this: a product can be perfectly representable and still
   be three orders of magnitude more than the consumer meant to lock.
8. **The admin cannot touch user funds.** It can only call `set_token` and
   `upgrade`.
9. **Every mutation is authenticated.** No state-changing entry point trusts its
   caller without `require_auth()`.
10. **Every mutation is observable.** Each transition emits a typed Soroban event
   with no secret material in it.

## Re-entrancy hardening: checks-effects-interactions

> Design note for issue #7. **Pending maintainer approval.**

The escrow token is a cross-contract call, so every money path is written as
**checks → effects → interactions**, and the ordering is the security property,
not a style preference:

1. **Checks.** Authenticate the actor, then validate ownership, lease state,
timestamps and arithmetic. Nothing has been written and no external contract has
been called, so a rejected call moves no value.
2. **Effects.** Persist the complete post-state in one write: for a settlement
`used_calls` and `settled`, for a revocation `settled`, and for a creation the
lease row itself (with `used_calls = 0` and the locked deposit already recorded)
before the id counter and TTLs are bumped. A re-entrant call re-reads this
committed state.
3. **Interactions.** Only then call `transfer` on the SEP-41 token.

| Entry point | Checks | Effects persisted first | Interaction |
| --- | --- | --- | --- |
| `create_lease` | auth, calls ≠ 0, duration bounds, service active, checked deposit, `MAX_LEASE_DEPOSIT`, token configured, checked expiry | `Lease { used_calls: 0, settled: false, locked_deposit }` | `transfer(consumer → registry)` |
| `settle_lease` | auth, not settled, `verified_calls ≤ allocated_calls`, stored provider, expired or fully consumed, checked payout/refund | `used_calls = verified_calls`, `settled = true` | `transfer(registry → provider)` then `transfer(registry → consumer)` |
| `revoke_expired_lease` | auth, consumer owns lease, not settled, expired, grace elapsed | `settled = true` | `transfer(registry → consumer)` |

**What a hostile token can and cannot do.**

- It cannot reach the registry while the registry is still executing: Soroban
  rejects a call into a contract that is already on the call stack. The two
  adversarial tokens
  (`test_reentrant_token_cannot_settle_a_lease_twice`,
  `test_reentrant_token_cannot_revoke_a_lease_twice`) attempt exactly that from
  inside `transfer`, and the attempt is rejected. That is a host guarantee this
  contract does not control, which is why it is not the only layer.
- The layer the ordering *does* guarantee: the `settled` flag is already
  persisted when the token runs. If a callback does arrive — on a host that
  permits re-entry — `settle_lease` and `revoke_expired_lease` read `settled`
  first and return `LeaseAlreadySettled` before any token call, so the second
  release is rejected and the outer call still completes once. That is pinned
  directly by `test_persisted_settled_flag_blocks_a_second_release`, which shows
  a settled lease rejecting both money paths and a revoked lease rejecting a
  later settlement.
- It can re-enter `create_lease` and mint an additional, separately funded
  lease. That is not a double spend: each lease has its own deposit and its own
  single release, and the re-entrant creation must fund itself through the same
  token.
- It can revert its own transfer, which reverts the whole registry call and every
  state write with it, including the lease id it had already allocated —
  `test_failed_escrow_transfer_rolls_back_the_creation` pins that. That is a
  liveness stall, not a theft: the lease stays unsettled and the consumer's
  revoke path remains open. It is why the admin is trusted to configure a
  well-behaved token.
- Value is released by two transfers in `settle_lease` (payout, then refund).
  The state effect is written before the first one, and the second transfer runs
  after the first has already moved value out of an escrow this contract reports
  as released, so no transfer can run before the lease is settled.

No new storage key, no lock and no unbounded loop is involved: the ordering adds
a single storage write per money path that was already written to, so
per-ledger work stays constant.

## Invariants

Asserted in `contracts/registry/src/test.rs`. The
[contract reference](contract-reference.md) lists the exact error code for each
violation.

- `payout + refund == locked_deposit` for every lease
  (`test_settlement_conserves_the_locked_deposit`,
  `test_revocation_conserves_the_locked_deposit`).
- A settled lease never transitions again
  (`test_lease_cannot_be_settled_twice`,
  `test_reentrant_token_cannot_settle_a_lease_twice`,
  `test_reentrant_token_cannot_revoke_a_lease_twice`).
- `settled` is true on-chain before any value leaves the contract: the
  re-entrant callbacks recorded during the payout and refund transfers are
  rejected (`test_reentrant_token_cannot_settle_a_lease_twice`,
  `test_reentrant_token_cannot_revoke_a_lease_twice`), and the persisted flag
  rejects both money paths on a settled lease
  (`test_persisted_settled_flag_blocks_a_second_release`).
- No lease can leave more value than was escrowed.
- Every lease has a consumer path to a full refund, for its entire TTL.
- The create / settle / revoke state machine and the conservation law are
  exhaustively model-checked by `contracts/registry/src/model.rs`, and the model
  is held to the deployed contract by a conformance test (see
  [Formal escrow model](#formal-escrow-model)).

## Provider key rotation (`transfer_service`)

`transfer_service(provider, service_id, new_provider)` re-points
`Service.provider`. It is **not** a money path and it is deliberately narrow:

- **Auth.** `provider.require_auth()` only, and the authenticated address must
  equal the stored owner (`Unauthorized` otherwise). The new address is not
  asked to co-sign because ownership is handed over by the party that already
  owns it.
- **No no-op.** `new_provider == service.provider` returns `ProviderUnchanged`,
  so an event never claims a transfer that did not happen.
- **One write, no external call.** Ownership is a single `save_service` write,
  so there is no intermediate ledger state. Settlement reads the stored provider
  when it executes, which is why there is no window where neither party can
  settle: up to the write the old provider settles, from the write onward the new
  one does.
- **In-flight leases are unaffected.** A lease references `service_id`, never an
  address. Leases created before the transfer are settled by the new provider
  with the same `locked_deposit`, `rate_per_call` and `expires_at`; the old
  provider is rejected with `Unauthorized` and can no longer pause, settle or
  transfer the service. The consumer's revoke path is untouched, so a transfer
  can never be used to strand a deposit.
- **Not a third-party seizure path.** No admin, consumer or arbitrary caller can
  call it for a service they do not own.

## Formal escrow model

> Design note for issue #128. **Pending maintainer approval.**

The escrow machine is small enough to check exhaustively, so it is checked in
CI by an ordinary `cargo test` run. No new tooling is needed:

- `contracts/registry/src/model.rs` is a bounded state machine — the
  specification below, in Rust.
- `test_model_explores_the_escrow_state_machine_exhaustively` walks **every**
  sequence of up to four actions from every create shape, under each of the three
  time regimes, and panics on the first violating transition.
- `test_model_matches_the_deployed_contract` drives the **real** registry through
  the same alphabet and compares the outcome and the resulting balances after
  every step, which is what keeps the model honest.

### Transitions

Let `L = rate_per_call × allocated_calls`, `verified` the provider's report and
`g` the 24-hour grace window. `now` is ledger time.

| # | Transition | Guard | Effect |
| --- | --- | --- | --- |
| T1 | `create(calls, rate)` | `calls > 0`, `rate > 0`, `rate × calls` fits `i128` and is `≤ MAX_LEASE_DEPOSIT` | `Active`, `locked = rate × calls`, `used = 0` |
| T2 | `settle(verified)` | `¬settled`, `verified ≤ allocated_calls`, caller is the stored provider, and `(now ≥ expires_at ∨ verified = allocated_calls)`, `payout`/`refund` fit | `Settled`, `used = verified`, `payout = rate × verified`, `refund = locked − payout` |
| T3 | `revoke()` | caller is the consumer, `¬settled`, `now ≥ expires_at`, `now ≥ expires_at + g` | `Settled`, `refund = locked` |
| T4 | any action on `Settled` | always false | rejected (`LeaseAlreadySettled`), no state change |

Error order is part of the spec, because clients branch on it: `settle` checks
`settled`, then the call count, then ownership, then the window; `revoke` checks
ownership, then `settled`, then expiry, then the grace window. The conformance
test compares the exact `Error`, so a reordered guard fails CI.

### Invariants

1. **Conservation.** In every reachable state `payout + refund ≤ locked`; on
   every successful release `payout + refund = locked`. Value is never created
   and never stranded: a lease is either fully escrowed or fully released.
2. **No double release.** A lease releases value at most once. Every action from
   `Settled` is rejected and leaves the state untouched, and the cumulative
   released amount is monotone and never exceeds `locked`.
3. **Rejections are no-ops.** A guarded transition that fails changes nothing,
   so a rejected call cannot be used to move value or rewrite a lease.

### Assumptions the model makes

1. The configured SEP-41 token moves exactly the requested amount or reverts.
   Token misbehaviour is covered by the re-entrancy tests, not by the model.
2. Every input except the lease shape is valid: the service exists, is active,
   has a configured token, `duration_secs` is within `1..=MAX_LEASE_DURATION_SECS`,
   and ledger time is monotonic. Each of those guards has its own unit test.
3. The clock is read once per call, as the entry points do.
4. `rate_per_call` is immutable for a service; a new price is a new service.
5. Leases are independent accounting units: one lease's escrow never backs
   another's payout.
6. `transfer_service` changes which address passes the provider guard, never the
   escrow arithmetic, so the model parameterises that guard as "is the caller the
   owner" and the rotation behaviour is pinned by its own tests.

### How the implementation stays in sync

The model does not read the contract's source; it is held to it by
`test_model_matches_the_deployed_contract`, which runs the real registry against
the same alphabet and compares, after every transition: the returned `Error`,
`settled`, `used_calls`, the provider's balance, the consumer's balance and the
balance left in escrow. Any change to a guard, its order, the payout formula, or
the field that is persisted first breaks one of those six comparisons.

An optional human-readable TLA+-style sketch is deliberately not committed: a
spec file CI does not check drifts, and the executable model above is the one
CI runs.

## Threats considered

| Threat | Mitigation |
| --- | --- |
| Provider claims more calls than allocated | `InvalidCalls` |
| Provider settles a lease twice | `settled` flag persisted before transfers + `LeaseAlreadySettled` |
| Attacker settles someone else's lease | `service.provider == provider` check |
| Attacker transfers a service they do not own | `transfer_service` owner check → `Unauthorized` |
| A provider transfer strands in-flight escrow | leases bind to `service_id`; new owner settles, consumer still revokes |
| Attacker revokes someone else's lease | `lease.consumer == consumer` check |
| Re-entrant token pays a lease twice | checks-effects-interactions; adversarial test |
| Provider strands funds by never settling | 24h grace, then full consumer refund |
| Consumer races the provider at expiry | 24h grace window reserved for the provider |
| Rate overflow creates a cheap lease | `checked_mul` → `Overflow` |
| Typo locks a consumer into an enormous deposit | `MAX_LEASE_DEPOSIT` → `DepositTooLarge` |
| Escrow token has more decimals than assumed | `MAX_LEASE_DEPOSIT` is in base units, so the ceiling tightens as the scale grows |
| Duration overflow wraps expiry | `checked_add` → `Overflow` |
| Long lease is evicted from storage | `DurationTooLong` + duration-aware TTL extension |
| Fake "KeyLease" contract id | Only ids in [Deployments](deployments.md) are ours |
| Silent logic change on upgrade | Admin-only, emits `contract_upgraded`, `contractmeta!` exposes the version |

## Known limitations

These are real and tracked as Drips Wave issues rather than hidden.

- **Upgrade trust.** The admin can replace the contract Wasm. This is a
  deliberate operational capability, but it means a compromised admin key can
  change protocol rules. A timelock or multisig requirement is future work.
- **Provider key rotation requires the old key.** `transfer_service` moves a
  service — and the right to settle its in-flight leases — to a new address, but
  only the address that owns it can authorize the hand-over, and the new address
  is not asked to accept it. A lost provider key therefore cannot be recovered
  by the protocol; the admin cannot reassign a service, by design, because that
  would be a path by which a third party seizes a service.
- **No rate updates.** Changing `rate_per_call` requires registering a new
  service; in-flight leases keep their original escrow.
- **Settlement is a single report.** The contract trusts the provider's
  `verified_calls`; it cannot independently observe HTTP usage. Consumers are
  protected by the fixed allotment and the refund path, not by proof of
  non-delivery.
- **Not audited.** Do not escrow mainnet value you would be unhappy to lose.

## Deployment hygiene

- Only trust contract ids in [`docs/deployments.md`](deployments.md).
- The admin key should be a cold or multisig Stellar account. It can change the
  escrow token and upgrade the contract.
- Verify the deployed metadata and configuration after deploy:

```bash
stellar contract invoke --id $REGISTRY --source-account admin --network testnet -- version
stellar contract invoke --id $REGISTRY --source-account admin --network testnet -- get_admin
stellar contract invoke --id $REGISTRY --source-account admin --network testnet -- get_token
stellar contract invoke --id $REGISTRY --source-account admin --network testnet -- get_service --service_id 1
```

> Testnet resets (2–4 times a year, at 17:00 UTC) wipe all ledger entries and
> change the contract id. Redeploy after a reset. See [Networks](networks.md).

## Disclosure

Report vulnerabilities privately per
[SECURITY.md](https://github.com/KeyLease-Security/keylease-core/blob/main/SECURITY.md).
We will credit reporters in the release notes unless asked not to.
