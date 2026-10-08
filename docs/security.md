# Security model

KeyLease escrows funds. This page states exactly what the contract guarantees,
what it assumes, and what is not yet hardened. For reporting a vulnerability, see
[SECURITY.md](../SECURITY.md).

## Trust assumptions

| Assumption | Consequence if violated |
| --- | --- |
| The admin configures a well-behaved SEP-41 token. | A malicious token contract could re-enter settlement. Tracked as a `wave-high` issue; mitigation is checks-effects-interactions. |
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
4. **A lease releases funds once.** Every money path checks `settled` first and
   sets it when it releases value (`LeaseAlreadySettled`).
5. **Overflow aborts.** Deposit, payout and expiry arithmetic use
   `checked_mul`/`checked_add`/`checked_sub` and revert with `Overflow`.
6. **The admin cannot touch user funds.** It can only call `set_token`.
7. **Every mutation is authenticated.** No state-changing entry point trusts its
   caller without `require_auth()`.

## Invariants

Asserted in `contracts/registry/src/test.rs`. The [contract reference](contract-reference.md)
lists the exact error code for each violation.

- `payout + refund == locked_deposit` for every lease.
- A settled lease never transitions again.
- No lease can leave more value than was escrowed.
- Every lease has a consumer path to a full refund.

## Threats considered

| Threat | Mitigation |
| --- | --- |
| Provider claims more calls than allocated | `InvalidCalls` |
| Provider settles a lease twice | `settled` flag + `LeaseAlreadySettled` |
| Attacker settles someone else's lease | `service.provider == provider` check |
| Attacker revokes someone else's lease | `lease.consumer == consumer` check |
| Provider strands funds by never settling | 24h grace, then full consumer refund |
| Consumer races the provider at expiry | 24h grace window reserved for the provider |
| Rate overflow creates a cheap lease | `checked_mul` → `Overflow` |
| Duration overflow wraps expiry | `checked_add` → `Overflow` |
| Re-entrancy when the token calls back | **Not yet hardened** — see below |

## Known limitations

These are real and tracked as Drips Wave issues rather than hidden.

- **Re-entrancy ordering.** `settle_lease` performs the token transfers before it
  persists `settled = true`. With an honest token this is safe (Soroban
  transactions are atomic), but a malicious or upgradeable token could re-enter
  and be paid twice. Fixing this is a `wave-high` issue.
- **No on-chain events.** The lifecycle is currently observable only by polling
  storage. Adding events is a `wave-medium` issue.
- **Lease TTL bound.** A lease that nobody reads for longer than
  `LEDGER_BUMP_EXTEND` (~30 days) could be evicted before it is revoke-able.
  Establishing and enforcing the safe `duration_secs` bound is a `wave-high`
  issue.
- **No provider key rotation.** A service is bound to the address that registered
  it. Transferring a service is a `wave-medium` issue.
- **Not audited.** Do not escrow mainnet value you would be unhappy to lose.

## Deployment hygiene

- Only trust contract ids in [`docs/deployments.md`](deployments.md).
- The admin key should be a cold or multisig account. It is the only key that can
  change the escrow token.
- On Testnet, verify after deploy with the read views:

```bash
stellar contract invoke --id $REGISTRY --network testnet -- get_admin
stellar contract invoke --id $REGISTRY --network testnet -- get_token
stellar contract invoke --id $REGISTRY --network testnet -- get_service --service_id 1
```

> Testnet resets (2–4 times a year, at 17:00 UTC) wipe all ledger entries and
> change the contract id. Redeploy after a reset.

## Disclosure

Report vulnerabilities privately per [SECURITY.md](../SECURITY.md). We will
credit reporters in the release notes unless asked not to.
