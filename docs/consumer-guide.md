# Consumer guide

You want metered access to an API or RPC endpoint without a permanent static key.
This guide walks through buying, using and reclaiming a lease.

## The deal you are getting

- You **prepay** `rate_per_call × requested_calls` into escrow.
- You get an authorization that expires after `duration_secs`.
- You pay **only for the calls you use**: unused balance is refunded at
  settlement.
- If the provider never settles, you reclaim the **entire** deposit 24 hours
  after expiry — no support ticket, no trust required.

## 1. Pick a service

```bash
stellar contract invoke --id $REGISTRY --network testnet \
  -- get_service --service_id 1
```

Returns the `provider`, `rate_per_call`, `is_active` and `endpoint_hash`. Confirm
the `endpoint_hash` matches the endpoint descriptor the provider published
off-chain before you escrow anything.

## 2. Fund your account

Any SEP-41 token the registry is configured with can be escrowed. On Testnet the
native asset's SAC is the usual choice, and Friendbot funds accounts with
**10,000 fake XLM**.

```bash
# Testnet only
curl "https://friendbot.stellar.org?addr=$(stellar keys address consumer)"
```

## 3. Create the lease

```bash
export REGISTRY=<your registry contract id>
export CONSUMER=$(stellar keys address consumer)

stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  -- create_lease \
     --consumer $CONSUMER \
     --service_id 1 \
     --requested_calls 500 \
     --duration_secs 3600
```

This escrows `rate_per_call × 500` and prints the new `lease_id`.

**Choose your numbers deliberately.** If your lease expires while you still have
unused calls, those calls are not usable — `remaining_calls` drops to `0` at
`expires_at`. Under-buy with a generous duration, then renew, rather than
over-buying with a short one.

> **Order of arguments matters.** `consumer` is the first argument. This is the
> exact ABI; older documentation that shows `service` first is wrong.

## 4. Use the access

With `@keylease/cli`:

```bash
export KEYLEASE_CONTRACT_ID=$REGISTRY
export KEYLEASE_SERVICE_ID=1

npx @keylease/cli acquire --service 1 --calls 500 --duration 3600
npx @keylease/cli env        # emit env vars for your app
npx @keylease/cli status --lease <lease_id>
```

`acquire` stores the session in `.keylease/sessions.json` and mints a
`kls1.*` bearer token. Send it to the provider's proxy:

```
Authorization: Bearer kls1.<payload>.<signature>
```

The response carries `x-keylease-calls-remaining` so you can watch your budget.

### Reading the session token

A session token is `kls1.<base64url payload>.<base64url ed25519 signature>` with
payload `{ v, lease_id, consumer, iat, exp }`. It proves *to the provider* which
lease you hold. It is not a substitute for the on-chain lease and it does not
move funds.

## 5. Watch the lease

```bash
stellar contract invoke --id $REGISTRY --network testnet -- \
  get_lease --lease_id 1

stellar contract invoke --id $REGISTRY --network testnet -- \
  remaining_calls --lease_id 1

stellar contract invoke --id $REGISTRY --network testnet -- \
  lease_status --lease_id 1
```

`lease_status` is `0` (Active), `1` (Expired) or `2` (Settled).

## 6. Get your money back

**Normal path — automatic.** When the provider settles, the unused balance is
transferred back to you in the same transaction. Nothing to claim.

**Provider went dark.** After `expires_at + 24 hours`, reclaim everything:

```bash
stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  -- revoke_expired_lease --consumer $CONSUMER --lease_id 1
```

This refunds the full `locked_deposit`. It works for a lease that was never used
and for one that was partially used but never settled.

### Why you must wait 24 hours

That window is reserved for the provider to settle legitimately. It is the
provider's guarantee, and your refund is the counterweight. It is a fixed
constant in the contract (`SETTLE_GRACE_PERIOD_SECS = 86_400`).

## Refund decision table

| Lease state | What happens |
| --- | --- |
| Provider settles before/at expiry | You get `locked_deposit − used × rate` back |
| Provider settles after expiry within 24h | Same as above |
| Provider settles with `verified_calls = 0` | You get the entire deposit back |
| Provider never settles, `now ≥ expiry + 24h` | You `revoke` and get the entire deposit |
| Provider never settles, inside the 24h window | Nothing yet — wait |

## Costs

Creating a lease and revoking one are two Soroban transactions (fees plus the
escrow transfer). There is no per-call on-chain cost — the provider settles with
a single call count.

## Read next

- [Provider guide](provider-guide.md) — the other side of the lease.
- [How the protocol works](protocol.md) — worked numbers for a full lifecycle.
