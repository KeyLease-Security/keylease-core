# Consumer guide

You want metered access to an API or RPC endpoint without a permanent static key.
This guide walks through buying, using and reclaiming a lease with Stellar.

## The deal you are getting

- You **prepay** `rate_per_call × requested_calls` into Soroban escrow.
- You get an on-chain authorization that expires after `duration_secs`.
- You pay **only for the calls you use**: unused balance is refunded at
  settlement.
- If the provider never settles, you reclaim the **entire** deposit 24 hours
  after expiry — no support ticket, no trust required.

Your Stellar account is your identity. There is no API key to copy into a `.env`
file and no separate credential service.

## 1. Pick a service

```bash
stellar contract invoke --id $REGISTRY --source-account <funded-account> \
  --network testnet -- get_service --service_id 1
```

Returns the `provider`, `rate_per_call`, `is_active` and `endpoint_hash`. Confirm
the `endpoint_hash` matches the endpoint descriptor the provider published
off-chain before you escrow anything.

## 2. Fund your account

Any SEP-41 token the registry is configured with can be escrowed. On Testnet the
native asset's SAC is the usual choice, and Friendbot funds accounts with
**10,000 fake XLM**.

```bash
# Testnet only: create + fund in one step
stellar keys generate consumer --network testnet --fund

# or fund an address you already have
curl "https://friendbot.stellar.org?addr=$(stellar keys address consumer)"
```

## 3. Create the lease

```bash
export REGISTRY=<your registry contract id>
export CONSUMER=$(stellar keys address consumer)

stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  --send=yes -- create_lease \
     --consumer $CONSUMER \
     --service_id 1 \
     --requested_calls 500 \
     --duration_secs 3600
```

This escrows `rate_per_call × 500` and prints the new `lease_id`. The transaction
emits `lease_created { lease_id, service_id, consumer, allocated_calls,
expires_at, locked_deposit }`; keep the transaction hash as proof of purchase.

**Choose your numbers deliberately.** If your lease expires while you still have
unused calls, those calls are not usable — `remaining_calls` drops to `0` at
`expires_at`. The maximum duration is 30 days (`MAX_LEASE_DURATION_SECS`).
Under-buy with a generous duration, then renew, rather than over-buying with a
short one.

> **Order of arguments matters.** `consumer` is the first argument. This is the
> exact ABI; older documentation that shows `service` first is wrong.

## 4. Use the access

Tell the provider which lease backs your requests. Because the lease records your
Stellar address, the provider authenticates you with a Stellar signature
(SEP-10 challenge–response, or an equivalent signed challenge) and checks that you
are `lease.consumer`. No bearer token is issued by KeyLease, and none is needed.

Concretely, for a provider that follows [Integration](integration.md):

1. complete the provider's Stellar challenge with your account,
2. send requests with your `lease_id` (path, query or header, as the provider
   documents) or let the provider resolve it from your authenticated address,
3. read `x-keylease-calls-remaining` if the provider exposes it.

If you can sign for the account that owns the lease, you can use it — there is no
separate credential to store or rotate. If you rotate that account, create a new
lease from the new one.

## 5. Watch the lease

```bash
stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- get_lease --lease_id 1

stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- remaining_calls --lease_id 1

stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- lease_status --lease_id 1
```

`lease_status` is `0` (Active), `1` (Expired) or `2` (Settled).

## 6. Get your money back

**Normal path — automatic.** When the provider settles, the unused balance is
transferred back to you in the same transaction. Nothing to claim.

**Provider went dark.** After `expires_at + 24 hours`, reclaim everything:

```bash
stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  --send=yes -- revoke_expired_lease --consumer $CONSUMER --lease_id 1
```

This refunds the full `locked_deposit` and emits `lease_revoked { lease_id,
consumer, refund }`. It works for a lease that was never used and for one that
was partially used but never settled.

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

Creating a lease and revoking one are two Soroban transactions (network fees plus
the escrow transfer). There is no per-call on-chain cost — the provider settles
with a single call count. Check the current fee with `stellar network settings`
or your RPC provider.

`create_lease` also pays **storage rent** for keeping the lease entry alive
through its term, the 24-hour grace window and a one-day buffer. Rent depends on
duration, not call count, so a short lease with a larger allotment is cheaper per
call than a long lease with a tiny one. The network enforces a minimum persistent
TTL (≈7 days on Testnet), so a very short lease still pays that floor.

## Read next

- [Provider guide](provider-guide.md) — the other side of the lease.
- [Integration](integration.md) — how the provider verifies you.
- [How the protocol works](protocol.md) — worked numbers for a full lifecycle.
