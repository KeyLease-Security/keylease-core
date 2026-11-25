# Glossary

Every term the guides use, defined once and linked to the page that explains it
in depth. Terms are grouped by what they describe; the links point at the
authoritative page, and the contract reference remains the source of truth for
signatures, fields and error codes.

## Actors and system

| Term | What it means | Explained in depth |
| --- | --- | --- |
| **Admin** | The account that initialized the registry. It can set the escrow token and schedule Wasm upgrades, and has no path to escrowed funds. | [Protocol — Actors](protocol.md#actors) |
| **Provider** | The seller of metered access. It publishes services, prices them, and settles the leases it owns. | [Protocol — Actors](protocol.md#actors) |
| **Consumer** | The buyer of access. It creates leases (its deposit is escrowed), uses the calls, and can revoke an expired lease nobody settled. | [Protocol — Actors](protocol.md#actors) |
| **Delegate** | An optional sponsor that submits `create_lease_for` and pays the transaction fee. It never pays the deposit and holds no claim on the lease. | [Protocol — Actors](protocol.md#actors) |
| **Service** | A provider's listing: a price per call plus a commitment to an off-chain endpoint. It is not an endpoint URL and not a credential. | [Protocol — Service](protocol.md#service) |
| **Lease** | A time-bound, escrowed authorization for a fixed number of calls. Its whole life is recorded on-chain. | [Protocol — Lease](protocol.md#lease) |
| **Escrow** | The registry-held token balance backing a lease. It is released only by settlement or revocation. | [Introduction — The idea](introduction.md#the-idea) |
| **SEP-41** | The Stellar token standard the registry uses to move and hold value. | [Introduction — Why this is a Stellar protocol](introduction.md#why-this-is-a-stellar-protocol) |
| **SAC** | Stellar Asset Contract: the host contract that wraps XLM or a classic asset as a SEP-41 token. | [Networks — Escrow asset](networks.md#escrow-asset-the-native-sac) |
| **Soroban** | Stellar's smart-contract runtime, where the registry and tokens execute. | [Introduction — The idea](introduction.md#the-idea) |
| **Wasm upgrade** | A contract code replacement through `propose_upgrade` → 24-hour delay → `execute_upgrade`, preserving storage and live leases. | [Contract reference — `propose_upgrade`](contract-reference.md#propose_upgrade) |
| **Attestation** | A provider-signed usage report that can settle a lease when a service has an attestor configured. | [Contract reference — Types](contract-reference.md#types) |

## Amounts and pricing

| Term | What it means | Explained in depth |
| --- | --- | --- |
| **Base unit** | The smallest indivisible amount of the escrow token. Every amount in the contract is an integer count of base units, never a whole-token figure. | [Contract reference — Rate units and stroops](contract-reference.md#rate-units-and-stroops) |
| **Stroop** | One base unit of the native asset: `1 stroop = 0.0000001 XLM` (7 decimals). The word applies only to XLM; a custom token's base unit is not a stroop. | [Contract reference — Rate units and stroops](contract-reference.md#rate-units-and-stroops) |
| **`rate_per_call`** | The integer price of one metered call, in the escrow token's base units. | [Contract reference — `register_service`](contract-reference.md#register_service) |
| **`locked_deposit`** | `rate_per_call × allocated_calls`, transferred into escrow when the lease is created. | [Contract reference — `create_lease`](contract-reference.md#create_lease) |
| **`payout`** | The part of the deposit paid to the provider for served calls at settlement. | [Contract reference — `settle_lease`](contract-reference.md#settle_lease) |
| **`refund`** | The part of the deposit returned to the consumer: the unused-call value at settlement, or the whole deposit on revocation. | [Contract reference — `settle_lease`](contract-reference.md#settle_lease) |
| **`MAX_RATE_PER_CALL`** | The upper bound on a published per-call price, in base units. | [Contract reference — Constants](contract-reference.md#constants) |
| **`MAX_LEASE_DEPOSIT`** | The upper bound on a single lease's deposit, in base units. | [Contract reference — Constants](contract-reference.md#constants) |
| **`quote_lease`** | A read-only preview of the `locked_deposit` and `expires_at` that `create_lease` would produce. It moves no funds. | [Contract reference — Read-only views](contract-reference.md#read-only-views) |

## Lease terms and state

| Term | What it means | Explained in depth |
| --- | --- | --- |
| **`service_id`** | The monotonic id assigned when a service is registered; it starts at `1`. | [Contract reference — `register_service`](contract-reference.md#register_service) |
| **`lease_id`** | The monotonic id assigned when a lease is created; it starts at `1` and is never reused. | [Contract reference — `create_lease`](contract-reference.md#create_lease) |
| **`endpoint_hash`** | A 32-byte commitment to the provider's off-chain endpoint descriptor, so no secret is published. | [Protocol — Service](protocol.md#service) |
| **`allocated_calls`** | The number of calls the consumer bought and the deposit covers. | [Contract reference — `create_lease`](contract-reference.md#create_lease) |
| **`used_calls`** | The calls the provider reported at settlement. It stays `0` on a revoked lease. | [Contract reference — `settle_lease`](contract-reference.md#settle_lease) |
| **`verified_calls`** | The call count the provider passes to `settle_lease` or `approve_early_cancel`; it must not exceed `allocated_calls`. | [Contract reference — `settle_lease`](contract-reference.md#settle_lease) |
| **`expires_at`** | The ledger timestamp at which the lease's term ends and its remaining calls stop being usable. | [Protocol — Lease](protocol.md#lease) |
| **`remaining_calls`** | Calls left before expiry; it reads `0` once the lease expires or settles. | [Contract reference — Read-only views](contract-reference.md#read-only-views) |
| **`locked_balance`** | The deposit still held in escrow for the lease; it reads `0` once settled or revoked. | [Contract reference — Read-only views](contract-reference.md#read-only-views) |
| **`settled` flag** | The stored flag that says escrow has left the contract. It is shared by settlement and revocation, so `lease_status` separates the two paths. | [Contract reference — `LeaseStatus` and the `settled` flag](contract-reference.md#leasestatus-and-the-settled-flag) |
| **`lease_status`** | The derived view of the lease: `Active`, `Expired`, `Settled` or `Revoked`. | [Contract reference — `LeaseStatus` and the `settled` flag](contract-reference.md#leasestatus-and-the-settled-flag) |
| **`service_stats`** | Cumulative settled leases, settled calls and payouts for one service. Revocations never count. | [Contract reference — Read-only views](contract-reference.md#read-only-views) |
| **`schema_version`** | The revision of an event payload, carried as the first key of every event so a stream stays decodable across upgrades. | [Integration — Event schema version](integration.md#event-schema-version) |

## Lifecycle and timing

| Term | What it means | Explained in depth |
| --- | --- | --- |
| **Settlement** | Closing a lease by paying the provider for `verified_calls` and refunding the remainder to the consumer. Allowed after expiry, or early when every allocated call was used. | [Protocol — When can a lease be settled?](protocol.md#when-can-a-lease-be-settled) |
| **Revocation** | The consumer's escape hatch: after the grace window, reclaim the entire deposit from a lease the provider never settled. The provider earns nothing. | [Protocol — The 24-hour grace window](protocol.md#the-24-hour-grace-window) |
| **Grace window** | The 24 hours after `expires_at` (`SETTLE_GRACE_PERIOD_SECS = 86_400`) reserved for provider settlement. Revocation becomes possible when it ends. | [Protocol — The 24-hour grace window](protocol.md#the-24-hour-grace-window) |
| **Early cancellation** | A consumer-requested, provider-approved settlement before expiry. It releases `rate × unused_calls` immediately and settles the lease. | [Protocol — Early cancellation with provider consent](protocol.md#early-cancellation-with-provider-consent) |
| **Ledger time** | The clock the protocol uses (`env.ledger().timestamp()`), not wall-clock time; expiry and grace boundaries are evaluated against it. | [Integration — Ledger time, not clock time](integration.md#ledger-time-not-clock-time) |
| **TTL** | The ledger lifetime of a storage entry. Lease entries are extended to cover their term, the grace window and a read buffer. | [Protocol — Storage and lifetime](protocol.md#storage-and-lifetime) |
| **Storage rent** | The fee paid to keep an entry alive on the ledger. It is prepaid, depends on duration, and is never refunded. | [Protocol — What a lease costs](protocol.md#what-a-lease-costs) |

## Lease states

| State | Meaning |
| --- | --- |
| **`Active`** (`0`) | Not settled, and `now < expires_at`. Remaining calls are usable. |
| **`Expired`** (`1`) | Not settled, and `now ≥ expires_at`. Remaining calls are no longer usable; the provider may still settle. |
| **`Settled`** (`2`) | Closed by settlement: the provider was paid for `used_calls`, and the rest was refunded. |
| **`Revoked`** (`3`) | Closed by revocation: the consumer reclaimed the whole deposit and the provider earned nothing. |

See [`LeaseStatus` and the `settled` flag](contract-reference.md#leasestatus-and-the-settled-flag)
for the transition table and the fields each state exposes.
