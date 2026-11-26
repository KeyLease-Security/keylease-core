# Consumer guide

## Command-line helper

The helper under `examples/consumer/` validates the configured service before
every operation and keeps signing in the Stellar CLI keystore:

```bash
cp .env.example .env
# Set KEYLEASE_CONTRACT_ID, KEYLEASE_SERVICE_ID, KEYLEASE_NETWORK,
# KEYLEASE_CONSUMER_IDENTITY, KEYLEASE_REQUESTED_CALLS and KEYLEASE_DURATION_SECS.

./examples/consumer/keylease-consumer.sh create --env .env
./examples/consumer/keylease-consumer.sh status --lease-id 1 --env .env
./examples/consumer/keylease-consumer.sh revoke --lease-id 1 --env .env
```

`create` first calls `get_service`, then `create_lease`, and prints `lease_id`
and `lease_expiry`. `status` calls `get_lease`. `revoke` applies the contract's
expiry plus 24-hour grace window by calling `revoke_expired_lease`; it does not
bypass that window. The script never accepts or writes a secret seed.

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

### Rate units: base units, not XLM

`rate_per_call` is an integer in the escrow token's **smallest unit**. It is not
a whole-token amount and not necessarily XLM. The registry never converts it, so
the human-readable price depends entirely on the token's decimals.

For the native SAC, XLM has 7 decimals: `1 XLM = 10,000,000 stroops`. The demo
service's `rate_per_call = 1_000_000` therefore means:

```text
1,000,000 stroops / 10,000,000 stroops per XLM = 0.1 XLM per call
500 calls -> 1,000,000 × 500 = 500,000,000 stroops = 50 XLM
```

For a custom SEP-41 token, use its `decimals()`: the same `1,000,000` is 1.0
token at 6 decimals and 0.000000000001 token at 18 decimals. Check the token's
decimals before you create the lease — assuming 7 when the token has 6 makes
this lease ten times more expensive than you expect. See
[Rate units and stroops](contract-reference.md#rate-units-and-stroops) for the
conversion table.

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

# optional: price it first. quote_lease is a view — no auth, no transfer — and
# it runs the same validation as create_lease, so these are the numbers the next
# transaction locks.
stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  -- quote_lease \
     --service_id 1 \
     --requested_calls 500 \
     --duration_secs 3600

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
expires_at, locked_deposit, rate_per_call }`; keep the transaction hash as
proof of purchase.

**Choose your numbers deliberately.** If your lease expires while you still have
unused calls, those calls are not usable — `remaining_calls` drops to `0` at
`expires_at`. The maximum duration is 30 days (`MAX_LEASE_DURATION_SECS`).
Under-buy with a generous duration, then renew, rather than over-buying with a
short one.

`rate_per_call × requested_calls` also has a ceiling: `MAX_LEASE_DEPOSIT`
(1,000,000,000,000 base units, so 100,000 of a seven-decimal token). A lease
above it reverts with `DepositTooLarge` before the token is called, so an
over-typed call count cannot escrow the wrong amount even when your balance
would cover it.

> **Order of arguments matters.** `consumer` is the first argument. This is the
> exact ABI; older documentation that shows `service` first is wrong.

**Someone else can arrange it for you.** `create_lease_for` mints the identical
lease with a sponsor as the courier: the sponsor signs and pays the Soroban
transaction fee, you sign and your balance is escrowed, and you keep the refund
and revoke rights.

```bash
stellar contract invoke \
  --id $REGISTRY --source-account sponsor --network testnet --send=yes \
  --sign $CONSUMER_SECRET_KEY \
  -- create_lease_for \
     --delegate $SPONSOR --consumer $CONSUMER \
     --service_id 1 --requested_calls 500 --duration_secs 3600
```

Your signature is mandatory: a sponsor cannot lock your deposit on its own.

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

`lease_status` is `0` (Active), `1` (Expired), `2` (Settled) or `3` (Revoked —
you reclaimed the deposit, so the provider earned nothing and `used_calls`
stays `0`). A lease becomes `Settled` as soon as the provider proposes a
settlement; read `get_settlement` to see whether the payout is still in the
challenge window or has been released. Only a finalized settlement is counted
in `service_stats`.

### Hand the lease to another account

You can transfer an unsettled lease to any Stellar address. The recipient gets
the remaining access **and** the refund right; your account loses both.

```bash
stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  --send=yes -- transfer_lease \
     --consumer $CONSUMER --lease_id 1 --new_consumer <NEW_ADDRESS>
```

This is a direct assignment — the recipient does not need to accept — and it
emits `lease_transferred { lease_id, from, to }`. A settled or revoked lease
cannot be transferred, and neither can a lease whose settlement is pending under
a challenge window. An expired, unsettled lease can still be transferred: you
are selling the refund claim, not access.

### End the lease early

```bash
stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  --send=yes -- request_early_cancel --consumer $CONSUMER --lease_id 1
```

The provider either approves — reporting the calls it actually served — or
rejects the request. On approval you are refunded `rate × unused_calls`
immediately and the lease ends; on rejection the lease keeps running exactly as
before. You can withdraw a pending request with `withdraw_early_cancel`. Early
cancellation is only available before `expires_at`; after expiry, use the revoke
path below.

## 6. Get your money back

**Normal path — automatic.** When the provider settles, the unused balance is
returned to you once the challenge window closes. Nothing to claim.

A settlement is a two-phase flow: `settle_lease` only proposes the split and
locks the funds for 24 hours. If you disagree with the reported call count, you
can dispute it inside that window with a 32-byte evidence hash:

```bash
stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  --send=yes -- challenge_settlement \
     --consumer $CONSUMER --lease_id 1 \
     --evidence_hash <sha256-of-your-evidence>
```

The challenge freezes the payout until the registry admin resolves it. If the
dispute is upheld you get the entire deposit back; if it is rejected the original
proposal is released. You can withdraw a challenge with `cancel_challenge`; the
settlement returns to pending and the original deadline is unchanged.

Otherwise the payout can be released by anyone after the window closes:

```bash
stellar contract invoke \
  --id $REGISTRY --source-account consumer --network testnet \
  --send=yes -- finalize_settlement --lease_id 1
```

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
| Provider proposes a settlement | Funds stay locked; you have 24h to challenge |
| You challenge inside 24h | Payout frozen until the admin resolves it |
| Dispute upheld | You get the entire deposit back |
| Dispute rejected or no challenge | `locked_deposit − used × rate` returns to you at finalization |
| Provider proposes `verified_calls = 0` | You get the entire deposit back |
| Provider never settles, `now ≥ expiry + 24h` | You `revoke` and get the entire deposit |
| Provider never settles, inside the 24h window | Nothing yet — wait |
| You transferred the lease | The new consumer receives all later refunds |
| You request early cancel and the provider approves | `rate × unused_calls`, immediately |
| You request early cancel and the provider rejects | Lease continues; nothing moves |

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
