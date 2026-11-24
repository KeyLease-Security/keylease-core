# Security model

KeyLease escrows funds on Stellar. This page states exactly what the Soroban
contract guarantees, what it assumes, and what is not yet hardened. For
reporting a vulnerability, see
[SECURITY.md](https://github.com/KeyLease-Security/keylease-core/blob/main/SECURITY.md).

## Trust assumptions

| Assumption | Consequence if violated |
| --- | --- |
| The admin configures a well-behaved SEP-41 token. | A malicious token contract could revert transfers and stall settlement. It cannot be paid twice: `settled` is persisted before the transfer, and a re-entrant token is rejected (`LeaseAlreadySettled`). |
| The admin does not upgrade to a malicious Wasm. | An upgrade can change the contract logic. The admin key should be cold or multisig. Upgrades now require a 24h on-ledger delay and emit `upgrade_proposed` with the hash and execution ledger, so consumers can exit; execution emits `contract_upgraded`. |
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
4. **Early cancellation needs the provider's consent.** Only the consumer can
   request or withdraw, only the owning provider can approve or reject, and only
   approval moves funds (`Unauthorized`, `CancellationNotRequested`). A request
   never blocks use, settlement or revocation and expires with the lease.
5. **A lease releases funds once.** Every money path checks `settled` first,
   persists `settled = true`, and only then calls the token
   (`LeaseAlreadySettled`). Checks-effects-interactions is enforced by
   `test_reentrant_token_cannot_settle_a_lease_twice`; the service's running
   totals are written in the same window, before the transfers, so a re-entrant
   token cannot inflate them either.
6. **Overflow aborts.** Deposit, payout, refund and expiry arithmetic use
   `checked_mul`/`checked_add`/`checked_sub` and revert with `Overflow`.
7. **Lease durations are storage-safe.** Durations above
   `MAX_LEASE_DURATION_SECS` (30 days) revert with `DurationTooLong`, and the
   persistent entry is extended for the term plus the grace window, so escrow
   cannot be evicted while it is claimable.
8. **A single lease cannot escrow an unbounded amount.** A deposit that fits
   `i128` but exceeds `MAX_LEASE_DEPOSIT` reverts with `DepositTooLarge`, before
   the lease id is allocated and before the token is called. Checked arithmetic
   alone cannot catch this: a product can be perfectly representable and still
   be three orders of magnitude more than the consumer meant to lock.
9. **Listed prices are leasable.** `register_service` rejects `rate_per_call`
   above `MAX_RATE_PER_CALL`, which equals `MAX_LEASE_DEPOSIT`. A service can
   therefore never be listed at a price that cannot fund even the smallest
   (one-call) lease, and the worst legal `rate × calls` product stays far inside
   `i128`, so a consumer's quote is always the amount that moves.
10. **The admin cannot touch user funds.** It can only call `set_token` and
    `upgrade`.
11. **Every mutation is authenticated.** No state-changing entry point trusts
    its caller without `require_auth()`.
12. **A sponsor cannot spend your balance.** `create_lease_for` calls
    `require_auth()` on the delegate *and* the consumer, and the deposit is
    transferred from the consumer; the refund and revoke paths stay bound to the
    consumer alone. Missing either signature reverts before any state changes.
13. **Revocation is not settlement.** A reclaimed lease reports
    `LeaseStatus::Revoked`, keeps `used_calls == 0`, pays the provider nothing,
    and is excluded from `service_stats`, so revenue reporting cannot be
    confused with a refund. An approved early cancellation is the opposite case:
    the provider was paid for served calls, so it is counted like any other
    settlement.
14. **Every mutation is observable.** Each transition emits a typed Soroban
    event with no secret material in it.

## Invariants

Asserted in `contracts/registry/src/test.rs`. The
[contract reference](contract-reference.md) lists the exact error code for each
violation.

- `payout + refund == locked_deposit` for every settlement and every approved
  early cancellation.
- A settled lease never transitions again.
- `settled` is true on-chain before any value leaves the contract.
- No lease can leave more value than was escrowed.
- Every lease has a consumer path to a full refund, for its entire TTL.
- `service_stats` advances only on provider settlement, and only by exactly
  `verified_calls` and the payout the transfers performed.
- A revoked lease and a settled lease are distinguishable through
  `lease_status` alone.

## Threats considered

| Threat | Mitigation |
| --- | --- |
| Provider claims more calls than allocated | `InvalidCalls` |
| Provider settles a lease twice | `settled` flag persisted before transfers + `LeaseAlreadySettled` |
| Attacker settles someone else's lease | `service.provider == provider` check |
| Attacker revokes someone else's lease | `lease.consumer == consumer` check |
| Consumer cancels early without paying for served calls | Provider approval reports `verified_calls`; no unilateral path reduces the payout |
| Provider approves a cancellation that was never requested | `CancellationNotRequested` |
| Cancellation bypasses the grace window at expiry | Requests are rejected at/after `expires_at` (`LeaseExpired`) |
| Re-entrant token pays a lease twice | checks-effects-interactions; adversarial test |
| Provider strands funds by never settling | 24h grace, then full consumer refund |
| Consumer races the provider at expiry | 24h grace window reserved for the provider |
| Rate overflow creates a cheap lease | `checked_mul` → `Overflow`, and `RateTooHigh` blocks the price before it can ever be leased |
| Typo locks a consumer into an enormous deposit | `MAX_LEASE_DEPOSIT` → `DepositTooLarge` |
| Escrow token has more decimals than assumed | `MAX_LEASE_DEPOSIT` is in base units, so the ceiling tightens as the scale grows |
| Provider lists a price no consumer can rent | `MAX_RATE_PER_CALL = MAX_LEASE_DEPOSIT`, so the cheapest lease of any listed service is fundable |
| Duration overflow wraps expiry | `checked_add` → `Overflow` |
| Delegate locks a consumer's deposit unilaterally | `create_lease_for` requires the consumer's `require_auth()` as well; no state changes without both |
| Delegate claims a refund it sponsored | refunds and revocation are keyed to `lease.consumer`, and the delegate is never stored on the lease |
| Provider inflates reported revenue | revoked leases are excluded from `service_stats`; totals use `checked_add` and revert on overflow |
| Long lease is evicted from storage | `DurationTooLong` + duration-aware TTL extension |
| Fake "KeyLease" contract id | Only ids in [Deployments](deployments.md) are ours |
| Silent logic change on upgrade | 24h delay + admin-only execution, emits `upgrade_proposed`/`contract_upgraded`, `contractmeta!` exposes the version |

## Running with a multisig admin

KeyLease stores the admin as a standard Soroban `Address`. In production, operators
should avoid using a single hot keypair for the admin role. Instead, the admin
can be a native Stellar multi-signature account (`G...`) or a multisig smart contract.

### Host-enforced authentication

When `admin.require_auth()` is called inside `set_token` or `upgrade`:

- If `admin` is a Stellar account, Soroban's runtime delegates authentication to
  Stellar's native account signing engine.
- If the account is configured with multi-signature weights and operation thresholds
  (e.g., medium threshold for contract invocations), the Stellar host requires
  signatures that satisfy the threshold before the invocation transaction can execute.
- The contract does not need special multisig code; multi-signature security is
  enforced natively at the protocol host layer.

### What the contract can and cannot enforce

| Scope | Contract capability | Details |
| --- | --- | --- |
| **Can enforce** | Execution authority (`require_auth`) | Ensures that the admin account legitimately approved the call based on Stellar's consensus rules |
| **Cannot enforce** | Signer composition | The contract cannot inspect individual signers, public keys, or individual weights |
| **Cannot enforce** | Minimum threshold | The contract cannot verify whether the admin account is 1-of-1, 2-of-3, or 3-of-5 |
| **Cannot enforce** | Signer set immutability | Signers and thresholds can be modified on the Stellar account via standard `SetOptions` operations without contract interaction |

## Audit status

**No external audit of this contract has been performed.** As of v0.2.1 no third
party has reviewed, formally verified or signed off on `contracts/registry`. No
firm is engaged and no report exists. Everything on this page is the project's own
claim about its own code: read [Guarantees](#guarantees) as a specification of
what the authors intended, not as an independent finding.

### Coverage that exists

| What | How much | Where |
| --- | --- | --- |
| Contract tests | 63 | `contracts/registry/src/test.rs`, run by `cargo test --all` |
| Documented guarantees | 10 | [Guarantees](#guarantees) |
| Invariants asserted in tests | 5 | [Invariants](#invariants) |
| Threats modelled | 14 | [Threats considered](#threats-considered) |
| CI gates | 9 | `Format`, `Clippy`, `Test`, `Build (wasm32v1-none)`, `Secret Scan` (gitleaks), `Spell Check`, `Markdown Lint`, `Dependency Policy` (cargo-deny) and `Malware Scan` (ClamAV) in [`.github/workflows/ci.yml`](https://github.com/KeyLease-Security/keylease-core/blob/main/.github/workflows/ci.yml) |

The counts are as of the revision this page is written against; each one is
mechanically checkable from the file it names.

Two of the 63 tests matter more than the rest, because they attack the contract
instead of exercising it:

- `test_reentrant_token_cannot_settle_a_lease_twice` registers a hostile token
  whose `transfer` calls back into `settle_lease` while the first settlement is
  still paying out. The attack is attempted once, and the test asserts the
  re-entrant call errored and the lease ended up settled exactly once. This is
  the only executable proof of the checks-effects-interactions ordering, and it
  is the property that makes a malicious token non-exploitable rather than merely
  inconvenient.
- the full lifecycle test escrowed with the **real Stellar Asset Contract**
  (`env.register_stellar_asset_contract_v2`) rather than `contracts/mock_token`,
  so the money paths are checked against a token this project does not control.

### What that coverage does not establish

A green suite is easy to over-read, so the limits are stated explicitly.

- **The threat model may be incomplete.** The 14 rows are the threats the authors
  thought of. The main value of an audit is the rows it adds, and by definition
  none of those are here yet.
- **Arbitrary tokens are out of scope.** `set_token` accepts any SEP-41 address.
  The suite proves the registry survives a re-entrant token; it cannot prove
  anything about a token that deviates from the interface in ways the registry
  does not expect.
- **Only simulated-ledger behaviour is tested.** The tests run against the
  `soroban-sdk` `Env`, not a deployed network, so instruction-footprint limits,
  eviction under real storage pressure, and consensus-timing edges are not
  covered here. The `soroban-sdk` test host does not enforce `maxEntryTtl` on
  `extend_ttl` at all, so the TTL clamp is asserted from the policy value rather
  than demonstrated as a host rejection. Read the TTL rules in the
  [contract reference](contract-reference.md#storage-classes-and-ttl-policies) as
  analysis, not as measurement.
- **State growth is bounded per call, not in aggregate.** `bump_service` and
  `bump_lease` are permissionless and each one charges its rent to the caller,
  which is what makes them safe to expose — but nothing here bounds how many
  entries may be kept alive or by whom. Persistent state that outlives its
  usefulness is an operational cost, and the suite does not price it.
- **Client code and provider middleware are not this contract.** Anything
  integrating per [Integration](integration.md) is outside the audited surface,
  and outside the surface these guarantees describe.
- **The documentation itself is unaudited.** A page that describes behaviour the
  code does not have is precisely the defect an audit catches and a test suite
  cannot.

### What a mainnet deployment would require

[Deployments](deployments.md#mainnet) records Mainnet as ⛔ not deployed, and
[Deployment → Mainnet](deployment.md#mainnet) gives the reason: deployment is
deferred until the contract is audited. Audited is a process, not a badge, so the
gate list a mainnet launch would have to clear first is:

1. **An independent audit** by a contract-security firm, with the report
   published and linked from the release notes and
   [SECURITY.md](https://github.com/KeyLease-Security/keylease-core/blob/main/SECURITY.md).
   A private report that stays private does not count as clearing this.
2. **Findings fixed and re-proved.** Every High and Medium with a code fix and a
   test in `contracts/registry/src/test.rs` that fails without the fix. Per
   [the developer guide](developer-guide.md), each security-sensitive change gets
   an adversarial case, not just a happy path.
3. **Upgrade trust bounded.** Today a single admin key can swap the Wasm and
   rewrite the escrow rules. Before mainnet value is in range, that key should be
   multisig or timelocked, and the timelock itself reviewed (see
   [Known limitations](#known-limitations)).
4. **A live operation path.** Someone watching the event stream, a named owner
   for the deployment, and a written response procedure. There is no
   protocol-wide pause: `set_service_active(provider, service_id, false)` blocks
   *new* leases on one service and leaves in-flight leases valid, and `upgrade` is
   admin-only. That is a throttle, not a kill switch.
5. **A staged value ramp.** Deploy, cap total escrow to something you would
   accept losing, run real lease cycles against the cap, and only then raise it.
6. **A verifiable record.** The mainnet contract id, Wasm hash and version in
   [`docs/deployments.md`](deployments.md), each confirmed on-chain through
   `version()`, `get_admin()` and `get_token()` rather than taken from the page.

Until 1 through 4 are done, this contract is for Testnet and for limited-value
evaluation. On Testnet every failure mode above costs an afternoon of redeploying
fake XLM; on Mainnet it costs someone's deposit.

## Known limitations

These are real and tracked as Drips Wave issues rather than hidden.

- **Upgrade trust.** The admin can replace the contract Wasm. While using a multisig
  admin prevents a single compromised key from executing an unauthorized upgrade,
  adding an on-chain timelock to upgrades remains future work.
- **No provider key rotation.** A service is bound to the address that registered
  it. Transferring a service is a `wave-medium` issue.
- **No rate updates.** Changing `rate_per_call` requires registering a new
  service; in-flight leases keep their original escrow.
- **Early cancellation depends on the provider.** A consumer cannot exit early
  unilaterally; if the provider never approves or rejects, the lease runs to
  expiry and the normal revoke path applies. There is no penalty scheme yet.
- **Settlement is a single report.** The contract trusts the provider's
  `verified_calls`; it cannot independently observe HTTP usage. Consumers are
  protected by the fixed allotment and the refund path, not by proof of
  non-delivery.
- **Not audited, and nothing on this page substitutes for it.** See
  [Audit status](#audit-status).

## Secret scanning

Pull requests are scanned for committed secrets with
[gitleaks](https://github.com/gitleaks/gitleaks) in CI (`Secret Scan` job). The
scan uses a pinned gitleaks version, fails on findings, and reads allowlist
entries only from `.gitleaks.toml`. Add allowlist entries only for documented
false positives and include a comment explaining why the match is not a secret.

Run locally before opening a PR:

```bash
gitleaks detect --source . --redact --verbose --config .gitleaks.toml
```

## Deployment hygiene

- Only trust contract ids in [`docs/deployments.md`](deployments.md).
- The admin key should be a cold or multisig Stellar account. It can change the
  escrow token and propose/cancel upgrades, but execution waits 24h on-ledger.
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
