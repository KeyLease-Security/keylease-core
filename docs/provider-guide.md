# Provider guide

You run an API or RPC endpoint. This guide shows how to sell metered access to it
with KeyLease instead of handing out a static key.

## What you are agreeing to

- You publish a **price per call** and a 32-byte `endpoint_hash` commitment.
- Consumers escrow `price × calls` before they can make a request.
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
  -- register_service \
     --provider $PROVIDER \
     --rate_per_call 1000000 \
     --endpoint_hash $(printf '%s' "$PROVIDER:my-api" | sha256sum | awk '{print $1}')
```

`rate_per_call` is in the escrow token's smallest unit (**7 decimals** for the
native asset, so `1000000` = `0.1 XLM` per call). The command prints the new
`service_id` — record it.

> Until `@keylease/cli` ships a `register` command, `stellar contract invoke` is
> the supported path. See the [contract reference](contract-reference.md).

## 2. Pause or resume a service

```bash
stellar contract invoke \
  --id $REGISTRY --source-account provider --network testnet \
  -- set_service_active --provider $PROVIDER --service_id 1 --is_active false
```

Deactivation blocks **new** leases only. Existing leases stay valid and must
still be settleable — you remain obligated to settle or let them expire.

## 3. Verify leases and meter calls

The on-chain contract cannot see your HTTP traffic, so verification and metering
happen in your request path. Two options:

### Use `@keylease/proxy` (recommended to start)

The proxy reads the lease from Soroban, checks expiry and the local counter, and
forwards only verified requests. Point it at your upstream:

```bash
export KEYLEASE_CONTRACT_ID=$REGISTRY
export KEYLEASE_UPSTREAM_URL=https://api.example.com
export KEYLEASE_SESSION_SECRET=<32-byte secret>
pnpm --filter @keylease/proxy start
```

Verified requests arrive with:

- `x-keylease-lease-id` — the lease backing the request
- `x-keylease-calls-remaining` — `allocated_calls − used`

### Verify in-process

If you already run a gateway, read the lease yourself:

1. Parse the bearer session token to get `lease_id`.
2. `get_lease(lease_id)` over Soroban RPC.
3. Reject if `settled`, if `now ≥ expires_at`, or if your call counter exceeds
   `allocated_calls`.
4. Count the call, return the response.

The on-chain `allocated_calls` and `expires_at` bound what you will ever be paid
for, so your counter is an optimisation rather than a source of truth.

## 4. Settle

Settle when the lease expires, or immediately once the consumer has used every
allocated call.

```bash
stellar contract invoke \
  --id $REGISTRY --source-account provider --network testnet \
  -- settle_lease --provider $PROVIDER --lease_id 1 --verified_calls 320
```

The registry pays you `verified_calls × rate_per_call` and refunds the rest to
the consumer. You may settle with `verified_calls = 0` to return the entire
deposit for a lease you could not serve.

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

- **Revenue recognised:** `verified_calls × rate_per_call` per settlement.
- **Unused consumer balance:** refunded by you at settlement, or by the consumer
  via `revoke_expired_lease` after the 24h grace window.
- **Disputes:** there is no dispute mechanism. The consumer's protection is the
  refund path; yours is the grace window. Report honestly.

## Operational notes

- **Watch expiry.** A lease you forget to settle becomes revocable 24 hours after
  it expires. Subscribe to lease-creation events once they ship (tracked as a
  `wave-medium` issue), or poll `lease_count` + `get_lease`.
- **Changing your rate.** Today you would register a new service. An update entry
  point is a `wave-medium` issue.
- **Rotating your key.** A service is bound to the registering address; key
  rotation is a `wave-medium` issue. Until then, register from an account you
  control long-term.
- **Testnet resets** wipe your service. Re-register after each reset.

## Read next

- [Consumer guide](consumer-guide.md) — what your customers do.
- [Hosting topology](hosting-topology.md) — where the proxy runs.
