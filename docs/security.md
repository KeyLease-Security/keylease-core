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
7. **The admin cannot touch user funds.** It can only call `set_token` and
   `upgrade`.
8. **Every mutation is authenticated.** No state-changing entry point trusts its
   caller without `require_auth()`.
9. **Every mutation is observable.** Each transition emits a typed Soroban event
   with no secret material in it.

## Invariants

Asserted in `contracts/registry/src/test.rs`. The
[contract reference](contract-reference.md) lists the exact error code for each
violation.

- `payout + refund == locked_deposit` for every lease.
- A settled lease never transitions again.
- `settled` is true on-chain before any value leaves the contract.
- No lease can leave more value than was escrowed.
- Every lease has a consumer path to a full refund, for its entire TTL.

## Threats considered

| Threat | Mitigation |
| --- | --- |
| Provider claims more calls than allocated | `InvalidCalls` |
| Provider settles a lease twice | `settled` flag persisted before transfers + `LeaseAlreadySettled` |
| Attacker settles someone else's lease | `service.provider == provider` check |
| Attacker revokes someone else's lease | `lease.consumer == consumer` check |
| Re-entrant token pays a lease twice | checks-effects-interactions; adversarial test |
| Provider strands funds by never settling | 24h grace, then full consumer refund |
| Consumer races the provider at expiry | 24h grace window reserved for the provider |
| Rate overflow creates a cheap lease | `checked_mul` → `Overflow` |
| Duration overflow wraps expiry | `checked_add` → `Overflow` |
| Long lease is evicted from storage | `DurationTooLong` + duration-aware TTL extension |
| Fake "KeyLease" contract id | Only ids in [Deployments](deployments.md) are ours |
| Silent logic change on upgrade | Admin-only, emits `contract_upgraded`, `contractmeta!` exposes the version |

## Known limitations

These are real and tracked as Drips Wave issues rather than hidden.

- **Upgrade trust.** The admin can replace the contract Wasm. This is a
  deliberate operational capability, but it means a compromised admin key can
  change protocol rules. A timelock or multisig requirement is future work.
- **No provider key rotation.** A service is bound to the address that registered
  it. Transferring a service is a `wave-medium` issue.
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
