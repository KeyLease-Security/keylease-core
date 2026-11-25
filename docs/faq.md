# Protocol FAQ

Frequently asked questions regarding KeyLease escrow mechanics, lifecycles, key
management, and network operations.

---

## How and when do consumers receive refunds?

Refunds occur under two distinct operational circumstances:

1. **Partial or Zero Usage upon Settlement:**
   When a provider settles a lease via `settle_lease`, it reports `verified_calls`.
   The provider receives `verified_calls × rate_per_call`, and any unspent
   remainder of `locked_deposit` is refunded to the consumer's token balance
   when the settlement is finalized after the 24-hour challenge window. An
   upheld challenge refunds 100% of the deposit instead. If `verified_calls ==
   0` and the proposal stands, the consumer is refunded 100% of their deposit.
   *See [Protocol: Lifecycle](protocol.md#lifecycle) and
   [Contract reference: settle_lease](contract-reference.md#settle_lease).*

2. **Consumer Revocation after Inactivity:**
   If a lease expires and the provider fails to settle within the 24-hour
   settlement grace window, the consumer can invoke `revoke_expired_lease`. This
   revokes the lease and refunds 100% of the locked deposit directly to the
   consumer.
   *See [Protocol: Exact grace-window boundaries](protocol.md#exact-grace-window-boundaries)
   and [Security: Guarantees](security.md#guarantees).*

---

## Why does the 24-hour grace window exist, and who can settle during it?

The 24-hour grace window (`SETTLE_GRACE_PERIOD_SECS = 86_400`) protects providers
from losing earned compensation due to transient network latency, RPC restarts,
or temporary downtime at the moment of lease expiry.

- **During the window (`expires_at <= now < expires_at + 86_400`):** Only the
  provider can settle via `settle_lease`. The consumer cannot revoke during this
  period (`RevokeWindowNotElapsed`).
- **After the window (`now >= expires_at + 86_400`):** The consumer gains the
  right to call `revoke_expired_lease` to reclaim 100% of their escrow if the
  provider has still not settled.

*See [Protocol: The 24-hour grace window](protocol.md#the-24-hour-grace-window)
and [Boundary tests](protocol.md#boundary-verification-tests).*

---

## What happens if a consumer or provider loses their secret key?

- **If the Consumer loses their secret key:**
  Escrowed funds remain locked in the contract until the lease expires. If the
  provider is operational, the provider can still call `settle_lease` to receive
  its earned payout; any remaining refund will be returned to the consumer's
  on-chain address (which the consumer cannot access without the lost key). If the
  provider never settles, the funds remain locked indefinitely, because only the
  consumer's key can sign `revoke_expired_lease`.
- **If the Provider loses their secret key:**
  The provider cannot authorize `settle_lease`, forfeiting its ability to claim
  payouts for delivered calls. Once the lease duration and the 24-hour grace
  window elapse (`now >= expires_at + 86_400`), the consumer can invoke
  `revoke_expired_lease` using their own key to reclaim 100% of the escrowed
  funds.

*See [Security: Trust assumptions](security.md#trust-assumptions) and
[Guarantees](security.md#guarantees).*

---

## Which tokens are supported, and can the escrow token be changed?

- **Supported Tokens:** KeyLease supports any asset implementing the Soroban
  SEP-41 token interface. This includes the native Stellar Asset Contract (SAC)
  for XLM, classic wrapped assets (like USDC), and custom Soroban token contracts.
- **Changing the Token:** The admin can configure or update the token address via
  `set_token(admin, new_token_address)`. Existing, in-flight leases are settled in
  the token asset that was locked when they were created; newly created leases
  will use the updated token address.

*See [Networks: Native SAC vs. Custom SEP-41 Token](networks.md#native-sac-vs-custom-sep-41-token)
and [Contract reference: set_token](contract-reference.md#set_token).*

---

## What happens when the Stellar Testnet resets?

Stellar Testnet resets occur 2–4 times per year to clean up ledger storage. When
a reset occurs:

1. All ledger entries—including deployed contract bytecodes, contract instances,
   services, and leases—are completely wiped.
2. The contract ID and service IDs cease to exist.
3. Operators must re-run the deployment script
   (`./scripts/deploy-testnet.sh`) to deploy a fresh registry instance,
   re-initialize the admin and token, and re-register services.
4. Clients and providers must update their environment configurations with the
   new `KEYLEASE_CONTRACT_ID`.

*See [Networks: Testnet resets](networks.md#testnet-resets) and
[Deployment](deployment.md).*
