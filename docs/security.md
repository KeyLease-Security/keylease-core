# Security model

KeyLease escrows funds on Stellar. This page states exactly what the Soroban
contract guarantees, what it assumes, and what is not yet hardened. For
reporting a vulnerability, see
[SECURITY.md](https://github.com/KeyLease-Security/keylease-core/blob/main/SECURITY.md).

## Trust assumptions

| Assumption | Consequence if violated |
| --- | --- |
| The admin configures a compatible SEP-41 token with truthful, stable balances and exact transfers. | Non-exact transfers are rejected, but a malicious token can lie about balances, rebase outside a transfer, or otherwise violate SEP-41 semantics. |
| The admin does not upgrade to a malicious Wasm. | An upgrade can change the contract logic. The admin key should be cold or multisig; upgrades emit `contract_upgraded`. |
| The provider reports `verified_calls` honestly. | It can under-report (it gets paid less) but never over-report past `allocated_calls`. |
| Soroban ledger time is monotonic and honest. | Validity windows would be unreliable. |
| Consumers keep their own key safe. | Anyone with the consumer key can create leases that spend its balance. |
| The admin configures a suitable protocol fee recipient. | Fees are paid to the configured address on provider-earned settlement amounts. |

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
5. **Deposits and transfers are exact.** The contract checks both token
   balances before and after each transfer. A transfer that does not debit and
   credit exactly the requested amount fails with `InvalidTokenBehavior` and
   the whole transaction rolls back.
6. **The protocol fee is capped and bounded to earned funds.** The admin may
   configure a fee from 0 through `MAX_PROTOCOL_FEE_BPS` (1,000 basis points,
   or 10%) and its recipient. The fee is calculated only on the provider's
   earned amount; unused funds remain refundable in full.
7. **Overflow aborts.** Deposit, payout and expiry arithmetic use
   `checked_mul`/`checked_add`/`checked_sub` and revert with `Overflow`.
8. **Lease durations are storage-safe.** Durations above
   `MAX_LEASE_DURATION_SECS` (30 days) revert with `DurationTooLong`, and the
   persistent entry is extended for the term plus the grace window, so escrow
   cannot be evicted while it is claimable.
9. **The admin cannot touch user funds.** It can configure the token, protocol
   fee, and upgrade the Wasm, but has no entry point to move escrow.
10. **Every mutation is authenticated.** No state-changing entry point trusts its
   caller without `require_auth()`.
11. **Every mutation is observable.** Each transition emits a typed Soroban event
   with no secret material in it.

## Invariants

Asserted in `contracts/registry/src/test.rs`. The
[contract reference](contract-reference.md) lists the exact error code for each
violation.

- `provider_payment + protocol_fee + refund == locked_deposit` for every
  settled lease.
- A revoked lease returns its full `locked_deposit`; revocation never charges a
  protocol fee.
- A settled lease never transitions again.
- `settled` is true on-chain before any value leaves the contract.
- No lease can leave more value than was escrowed.
- Every lease has a consumer path to a full refund, for its entire TTL.

## Design decisions: token transfers and protocol fees

These are the selected designs for issues #129 and #131.

### #129 — fee-on-transfer token behavior

Two strategies were considered:

- **Balance-delta accounting** would accept fee-on-transfer tokens and record
  only the amount received. It changes the lease's funded value from the
  advertised `rate_per_call × requested_calls`, complicates call pricing and
  refund expectations, and still requires checks on outgoing transfers to
  avoid spending more than escrow holds.
- **Reject non-exact transfers** preserves the current price and lease model.
  The registry measures both sender and recipient balances before and after
  each transfer and requires an exact debit and credit. Unexpected deltas
  return `InvalidTokenBehavior`; Soroban transaction atomicity rolls back the
  token call and all registry state, so no underfunded lease is created and no
  settlement or refund is partially recorded.

The registry uses the second strategy. Checks run on every inbound and
outbound transfer, rather than only when `set_token` is called: configuration
cannot safely probe a token without moving user funds, and a token's behavior
could change after configuration. Fee-on-transfer tokens are therefore not
supported; administrators must choose a token with exact transfer semantics.
The guarantee depends on the token's `balance` implementation being truthful
and stable during the check. It cannot defend against arbitrary malicious
tokens that lie, rebase outside transfers, or otherwise break SEP-41 behavior.

### #131 — capped protocol fee

The fee is an unsigned integer in basis points (10,000 basis points = 100%),
with a contract-enforced hard cap of 1,000 basis points (10%). Only the
authenticated registry admin can set the fee and recipient together. A zero
fee is valid and may have no recipient; a positive fee requires a recipient
that is not the registry itself. New configuration keys are appended and
default to zero fee / no recipient when absent, preserving existing instance
storage and in-flight leases on upgrade.

At settlement, the fee is floored to whole token units from the gross amount
earned for `verified_calls` (`rate_per_call × verified_calls`). The provider
receives gross earned amount minus the fee, and the consumer receives
`locked_deposit - gross earned amount`. Thus rounding always favors the
provider/consumer side rather than increasing the fee, and the unused consumer
refund is never reduced by the fee. The amount is computed with checked,
overflow-safe quotient/remainder arithmetic. The fee rate and recipient in
effect when settlement executes apply to that lease, including leases created
before the latest configuration change; each change is observable through
`protocol_fee_set` and the rate cannot exceed the hard cap.

Revoked leases pay **no** protocol fee: the provider earned nothing through
settlement, and the consumer's escape hatch returns the entire deposit. For a
settlement, `provider_payment + protocol_fee + consumer_refund` must equal the
recorded deposit. For revocation, `consumer_refund` equals that full deposit.
Settlement events report provider payment, fee, and refund separately.

## Threats considered

| Threat | Mitigation |
| --- | --- |
| Provider claims more calls than allocated | `InvalidCalls` |
| Provider settles a lease twice | `settled` flag persisted before transfers + `LeaseAlreadySettled` |
| Fee-on-transfer token underfunds escrow or reduces payouts | Exact sender/recipient balance deltas are checked; mismatch returns `InvalidTokenBehavior` and rolls back |
| Fee configuration exceeds the protocol limit | Admin setter enforces the 1,000 bps cap in contract logic |
| Fee consumes the consumer's unused deposit | Fee applies only to gross consumed amount; refund is computed independently |
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

These remaining limitations are stated rather than hidden.

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
- **Token compatibility remains an admin responsibility.** Balance-delta
  checks reject transfer fees, but cannot prove a custom token's balance view
  truthful or prevent it from rebasing or changing behavior outside a checked
  transfer. Prefer the native SAC or a vetted, exact-transfer SEP-41 token.
- **Not audited.** Do not escrow mainnet value you would be unhappy to lose.

## Deployment hygiene

- Only trust contract ids in [`docs/deployments.md`](deployments.md).
- The admin key should be a cold or multisig Stellar account. It can change the
  escrow token, protocol fee and recipient, and upgrade the contract.
- Before calling `set_token`, verify the token has truthful, stable balances
  and exact transfer behavior. `set_token` does not probe token semantics;
  fee-on-transfer and rebasing tokens are unsupported.
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
