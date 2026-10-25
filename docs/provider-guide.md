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
native asset, so `1000000` = `0.1 XLM` per call). The command prints the new
`service_id` — record it.

The registration emits `service_registered { service_id, provider, rate_per_call,
endpoint_hash }`, so your own indexer can pick it up without a separate query.

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

## 5. Accounting

`settle_lease` pays you from escrow. Nothing else does. The registry never holds
funds on your behalf after settlement, and there is no yield or custody to
manage.

- **Revenue recognised:** `verified_calls × rate_per_call` per settlement,
  published in the `lease_settled` event.
- **Unused consumer balance:** refunded by you at settlement, or by the consumer
  via `revoke_expired_lease` after the 24h grace window.
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
