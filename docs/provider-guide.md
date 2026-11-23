# Provider guide

You run an API or RPC endpoint. This guide shows how to sell metered access to it
with KeyLease instead of handing out a static key, using only Stellar tooling and
the Soroban contract.

## What you are agreeing to

- You publish a **price per call** and a 32-byte `endpoint_hash` commitment.
- Consumers escrow `price × calls` in a SEP-41 token before they can make a
  request.
- You **settle** a lease once, reporting how many calls were consumed.
- If you never settle, the consumer reclaims the full deposit 24 hours after
  expiry. That guarantee is what makes consumers willing to prepay.

You never publish a secret on-chain. `endpoint_hash` is a commitment, not a
credential.

## 1. Register a service

```bash
export REGISTRY=<your registry contract id>
export PROVIDER=$(stellar keys address provider)

stellar contract invoke \
  --id $REGISTRY --source-account provider --network testnet \
  --send=yes -- register_service \
     --provider $PROVIDER \
     --rate_per_call 1000000 \
     --endpoint_hash $(printf '%s' "$PROVIDER:my-api" | sha256sum | awk '{print $1}')
```

`rate_per_call` is in the escrow token's smallest unit (**7 decimals** for the
native asset, so `1000000` = `0.1 XLM` per call). It must be positive and no
more than `MAX_RATE_PER_CALL`, which is `MAX_LEASE_DEPOSIT` — 1,000,000,000,000
base units, 100,000 XLM per call. A higher rate reverts with `RateTooHigh` and
registers nothing. The two bounds are the same number on purpose: the cheapest
lease is a single call, so a price above the deposit cap would advertise a
service no consumer could ever rent.

Your rate still decides how large a lease can be, because `MAX_LEASE_DEPOSIT`
caps the `rate_per_call × requested_calls` product: at your rate, the largest
lease a consumer can open is `MAX_LEASE_DEPOSIT ÷ rate_per_call` calls. A rate
of `1000000` leaves room for 1,000,000 calls; a rate of `100000000` caps them at
10,000. Price the whole allotment your typical customer wants before you pick
one. The command prints the new `service_id` — record it.

The registration emits `service_registered { service_id, provider, rate_per_call,
endpoint_hash }`, so your own indexer can pick it up without a separate query.

To read your catalogue back from the chain instead of your own database, page
through `services_for_provider(provider, after, limit)` — pass `next` back as
`after` until `done`, and keep `limit` at or below `MAX_PAGE_SIZE` (`100`). See
[contract reference](contract-reference.md#read-only-views).

## 2. Pause or resume a service

```bash
stellar contract invoke \
  --id $REGISTRY --source-account provider --network testnet \
  --send=yes -- set_service_active --provider $PROVIDER --service_id 1 --is_active false
```

Deactivation blocks **new** leases only. Existing leases stay valid and must
still be settleable — you remain obligated to settle or let them expire. The
change emits `service_status_changed`.

## 3. Verify leases and meter calls

The contract cannot see your HTTP traffic, so verification and metering happen in
your request path. The credential is the on-chain lease; there is no session
token to mint.

### Verify against Soroban RPC

1. Authenticate the caller as a Stellar address (SEP-10 challenge–response is the
   standard approach).
2. Read the lease with `get_lease(lease_id)` over Soroban RPC and cache it briefly.
3. Reject if `settled`, if `now ≥ expires_at`, if the caller is not
   `lease.consumer`, or if your counter has consumed `allocated_calls`.
4. Count the call, return the response.

```bash
stellar contract invoke --id $REGISTRY --source-account provider \
  --network testnet -- get_lease --lease_id 1
```

Reads need no secret key: a verifier can run without any signing authority.

### Or project the event stream

If you would rather not call RPC per request, subscribe to `lease_created` and
keep a local table of active leases. The on-chain `allocated_calls` and
`expires_at` remain the bound; your projection is an optimisation.

The full pipeline, including the Stellar SDK read example and cache guidance, is
in [Integration](integration.md).

## 4. Settle

Settle when the lease expires, or immediately once the consumer has used every
allocated call. One settlement covers the whole lease — never one per request.

```bash
stellar contract invoke \
  --id $REGISTRY --source-account provider --network testnet \
  --send=yes -- settle_lease --provider $PROVIDER --lease_id 1 --verified_calls 320
```

The registry pays you `verified_calls × rate_per_call`, refunds the rest to the
consumer, and emits `lease_settled { lease_id, provider, verified_calls, payout,
refund }`.

Settle with `verified_calls = 0` to return the entire deposit for a lease you
could not serve.

### Settling rules

| Situation | Result |
| --- | --- |
| `now ≥ expires_at`, any `verified_calls ≤ allocated_calls` | settle allowed |
| `verified_calls == allocated_calls`, any time | settle allowed |
| `now < expires_at` and `verified_calls < allocated_calls` | `LeaseNotSettleable` |
| `verified_calls > allocated_calls` | `InvalidCalls` |
| lease already settled | `LeaseAlreadySettled` |
| you don't own the service | `Unauthorized` |

## 5. Early cancellation requests

A consumer who stops using the service early can ask you to release the unused
balance without waiting for expiry + 24 hours. The request moves no funds:

```bash
stellar contract invoke --id $REGISTRY --source-account provider --network testnet \
  --send=yes -- approve_early_cancel \
     --provider $PROVIDER --lease_id 1 --verified_calls 320
```

Approving reports the calls you actually served. You receive
`verified_calls × rate_per_call`, the consumer is refunded
`rate_per_call × (allocated_calls − verified_calls)`, and the lease ends in the
same transaction. There is no rounding: the rate is a per-call integer, so the
refund is the exact value of whole unused calls.

Approval is valid even if the lease has passed `expires_at`, as long as the
consumer requested it before expiry. If you decline, reject the request instead;
the lease is untouched and settles normally later:

```bash
stellar contract invoke --id $REGISTRY --source-account provider --network testnet \
  --send=yes -- reject_early_cancel --provider $PROVIDER --lease_id 1
```

The consumer may withdraw the request at any time. A lease that is already
settled cannot be cancelled early.

## 6. Accounting

`settle_lease` pays you from escrow. Nothing else does. The registry never holds
funds on your behalf after settlement, and there is no yield or custody to
manage.

- **Revenue recognised:** `verified_calls × rate_per_call` per settlement,
  published in the `lease_settled` event.
- **Lifetime totals:** `service_stats --service_id <id>` returns
  `settled_leases`, `settled_calls` and `total_payout` for one service without
  replaying history. It counts every path that pays the provider — plain
  settlement and an approved early cancellation — and nothing else: a lease the
  consumer revoked contributed nothing, so a reclaimed lease never inflates
  reported revenue.
  Treat it as a convenience view and `lease_settled` as the book of record: the
  aggregate is stored with a TTL, so an entry nothing has settled or read for a
  long stretch can age out and read back as zero.
- **Unused consumer balance:** refunded by you at settlement or on an approved
  early cancellation, or by the consumer via `revoke_expired_lease` after the
  24h grace window.
- **Early cancellation:** `lease_cancelled` publishes `verified_calls`, `payout`
  and `refund`. It is a consented early settlement; the same conservation rule
  holds (`payout + refund == locked_deposit`).
- **Disputes:** there is no dispute mechanism. The consumer's protection is the
  refund path; yours is the grace window. Report honestly.

## Operational notes

- **Watch expiry.** A lease you forget to settle becomes revocable 24 hours after
  it expires. Subscribe to `lease_created` and settle from that queue.
- **Changing your rate.** Today you register a new service; an update entry point
  is a `wave-medium` issue. In-flight leases keep their original escrow.
- **Rotating your key.** A service is bound to the registering address; key
  rotation/transfer is a `wave-medium` issue. Register from an account you can
  keep — a Stellar multisig account works today.
- **Upgrades.** Watch `contract_upgraded` and check `version()` after a release.
  Storage and balances are preserved across upgrades.
- **Testnet resets** wipe your service. Re-register after each reset.

## Read next

- [Integration](integration.md) — the verification pipeline in detail.
- [Consumer guide](consumer-guide.md) — what your customers do.
- [Networks](networks.md) — RPC endpoints and resets.
