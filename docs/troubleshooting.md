# Troubleshooting

Every failure this contract can return, grouped by who is holding the error.
Each entry names the fix and the command that proves the state you are about to
act on. Error codes link to the
[contract reference table](contract-reference.md#error-codes), which is the
authoritative list.

Nothing here moves funds. `settle_lease` pays a provider and
`revoke_expired_lease` refunds a consumer, and both can only be called once per
lease — so verify first, then call.

## Three commands first

Most "the contract is broken" reports are a wrong contract id, a wrong network
or an unconfigured token. These three answers cost one simulation each:

```bash
export REGISTRY=<contract id from docs/deployments.md>
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet -- version
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet -- get_admin
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet -- get_token
```

| What you see | What it means | Go to |
| --- | --- | --- |
| `version` returns the release you deployed | the id is live and initialised | keep reading |
| any call returns `Error(WasmNotInstalled)` or the ledger says the entry does not exist | wrong id, or the network was reset | [The network moved under you](#the-network-moved-under-you) |
| `get_admin` returns `NotInitialized` | this contract was never initialised | [`NotInitialized` and `AlreadyInitialized`](#notinitialized-and-alreadyinitialized) |
| `get_token` returns `TokenNotSet` | leases can be read but never funded | [`TokenNotSet`](#tokennotset) |

Read calls still need a funded `--source-account`, because the CLI builds a
simulation transaction: see the note on read calls in
[Networks](networks.md).

---

## For consumers

### `LeaseNotExpired`

**Cause:** `revoke_expired_lease` ran before `expires_at`. The lease is still
inside its term, so the provider may still be paid.

**Fix:** wait for the term to end, or use the calls you paid for and let the
provider settle.

**Verify the term:**

```bash
stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- get_lease --lease_id 1
stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- lease_status --lease_id 1
```

Reference: [error code 9](contract-reference.md#error-codes).

### `RevokeWindowNotElapsed`

**Cause:** the lease is expired, but fewer than
`SETTLE_GRACE_PERIOD_SECS` (24 hours) have passed since `expires_at`. That window
belongs to the provider: they can still settle for the calls they served.

**Fix:** wait until `expires_at + 24h`. The refund is the whole
`locked_deposit` — revocation is all-or-nothing and pays the provider nothing,
which is why the window exists.

**Verify how much is still yours:**

```bash
stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- locked_balance --lease_id 1
```

Reference: [error code 10](contract-reference.md#error-codes).

### `Unauthorized` on `revoke_expired_lease`

**Cause:** the lease belongs to a different address than the one that authorised.
A lease is not transferable, and the refund path follows
`lease.consumer`, not the caller.

**Fix:** re-run with the account that created the lease. If you lost that key,
there is no recovery path on chain — that is a
[trust assumption](security.md#trust-assumptions), not a bug.

**Verify who owns it:** compare `consumer` in the `get_lease` output with
`stellar keys address <your-seed-name>`.

Reference: [error code 3](contract-reference.md#error-codes).

### `InvalidCalls`, `InvalidDuration`, `DurationTooLong` on `create_lease`

| Code | Cause | Fix |
| --- | --- | --- |
| [`InvalidCalls`](contract-reference.md#error-codes) | `requested_calls` is `0` | ask for at least one call |
| [`InvalidDuration`](contract-reference.md#error-codes) | `duration_secs` is `0` | the lease must have a term |
| [`DurationTooLong`](contract-reference.md#error-codes) | term exceeds `MAX_LEASE_DURATION_SECS` (30 days) | renew instead of lengthening: the storage TTL is only guaranteed for the bounded term |

`create_lease` escrows `rate_per_call × requested_calls` before anything else, so
a rejected lease moves no funds. To check the price before you buy:

```bash
stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- get_service --service_id 1
```

### `ServiceNotFound` or `ServiceInactive` on `create_lease`

**Cause:** the id is from another deployment, or the provider paused the service.
Testnet resets restart `service_id` from zero, so an id you saved last week can
point at a different service — or at nothing.

**Fix:** re-read the ids for the network you are on:

```bash
stellar contract invoke --id $REGISTRY --source-account consumer \
  --network testnet -- service_count
```

`ServiceInactive` is the provider's `set_service_status(false)`. Nothing is
stuck: your funds never left your account.

### `Overflow` on `create_lease`

**Cause:** `rate_per_call × requested_calls` does not fit in `i128`. All deposit
math is checked and a failed multiplication returns this instead of a cheaper
lease.

**Fix:** request fewer calls. If a provider's rate is large enough that a normal
order overflows, that is a pricing bug on their side — quote it as
`rate_per_call` × calls before you escrow.

### `DepositTooLarge` on `create_lease`

**Cause:** the deposit fits in `i128` but exceeds `MAX_LEASE_DEPOSIT`
(1,000,000,000,000 base units, so 100,000 XLM at seven decimals). `Overflow`
means the multiplication does not fit; this means it fits and is still more than
one lease is allowed to escrow.

**Fix:** split the order into several leases of fewer calls each. The bound is
deliberate: it caps what a single lease can put at risk, so raising it is a
contract change, not a client setting.

### Your balance did not move but the lease exists

**Cause:** the transaction was not sent. `stellar contract invoke` simulates by
default and prints the result without submitting.

**Fix:** add `--send=yes`, then confirm the lease landed:

```bash
stellar events --id $REGISTRY --network testnet --start-ledger <ledger>
```

A `lease_created` event is the record that the escrow happened; see
[Following the event stream](integration.md#following-the-event-stream).

---

## For providers

### `LeaseNotSettleable` on `settle_lease`

**Cause:** neither settlement condition holds yet. A lease settles when it has
**expired**, or when the consumer used **every** call it paid for
(`verified_calls == allocated_calls`).

**Fix:** settle at expiry with the real number of served calls — an under-report
refunds the consumer and is not recoverable later, because a lease settles once.

**Verify what you may claim:**

```bash
stellar contract invoke --id $REGISTRY --source-account provider \
  --network testnet -- get_lease --lease_id 1
stellar contract invoke --id $REGISTRY --source-account provider \
  --network testnet -- remaining_calls --lease_id 1
```

`remaining_calls` returns `0` for an expired lease, so use `used_calls` for the
quota, not for the payout.

Reference: [error code 8](contract-reference.md#error-codes).

### `InvalidCalls` on `settle_lease`

**Cause:** `verified_calls` exceeds `allocated_calls`. Settlement cannot pay out
more than the consumer escrowed.

**Fix:** pass at most `allocated_calls`. If your counter is higher than the
allotment, your local count and the lease disagree — the on-chain record wins.

### `Unauthorized` on `settle_lease`

**Cause:** the authorising account is not `service.provider` for that lease.

**Fix:** settle with the key that registered the service. Two things make this
surprisingly common:

- a service was registered by one key and the settlement daemon runs with another
- keys cannot be rotated yet; a service is bound to its registering address
  forever ([known limitation](security.md#known-limitations))

**Verify the binding:**

```bash
stellar contract invoke --id $REGISTRY --source-account provider \
  --network testnet -- get_service --service_id 1
```

### Your funds never arrived after settling

**Cause:** the settlement paid `verified_calls × rate_per_call`, and the rest went
back to the consumer. A settle with `verified_calls = 0` pays you nothing.

**Fix:** check the amounts against the event, not your logs:

```bash
stellar events --id $REGISTRY --network testnet --start-ledger <ledger>
```

A `lease_settled` event carries `payout` and `refund`. If the event is absent, the
transaction was simulated and never sent (`--send=yes`).

### `429` / `403` from your own service, not the chain

The contract only escrows; the HTTP layer is yours. The
[verification pipeline](integration.md#the-verification-pipeline) fails closed at
each step, so the causes are: lease expired or settled (`403`), the caller is not
`lease.consumer` (`403`), or the allotment is exhausted (`429`). A lease id that
is not in `docs/deployments.md`'s network is `403` with a
[`LeaseNotFound`](contract-reference.md#error-codes) on chain.

---

## For admins and operators

### `NotInitialized` and `AlreadyInitialized`

| Code | Cause | Fix |
| --- | --- | --- |
| [`NotInitialized`](contract-reference.md#error-codes) | `init` never ran on this id, or you are pointed at an old one | check `version` above, then run `init` or correct `REGISTRY` |
| [`AlreadyInitialized`](contract-reference.md#error-codes) | `init` ran twice, usually a re-run of the deploy script against an id that already exists | stop; deploy a fresh contract instead. The admin cannot be changed by re-initialising |

### `TokenNotSet`

**Cause:** the escrow asset was never configured. `create_lease` refuses to store
a lease it cannot fund.

**Fix:** set the token, then verify it resolved to the address you meant:

```bash
stellar contract id asset --network testnet --asset native
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet --send=yes -- set_token --admin $ADMIN --token <TOKEN_ID>
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet -- get_token
```

Changing the token while leases are in flight strands their payouts against the
old asset. Settle or revoke first: see
[`set_token`](contract-reference.md#set_token).

### `Unauthorized` on `set_token` or `upgrade`

**Cause:** the caller is not the stored `Admin`.

**Fix:** `stellar contract invoke --id $REGISTRY --source-account <whoever>
--network testnet -- get_admin` prints the only address that can authorise. If
that key is lost, the token and the Wasm are frozen — escrow can still be
refunded by consumers through `revoke_expired_lease`.

### `upgrade` succeeded but behaviour did not change

**Cause:** the hash you uploaded is not the build you think it is, or the
`version()` string was not bumped before building.

**Fix:** compare what is on chain with what you built:

```bash
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet -- version
```

`version` is embedded from `Cargo.toml` by `contractmeta!`, so it proves which
release is live. Upgrades must keep the storage layout and must never renumber
error codes ([upgrading](deployment.md#upgrading)).

### The network moved under you

Testnet and Futurenet are reset by SDF
[2–4 times a year, at 17:00 UTC](networks.md#testnet-resets). A reset deletes
every ledger entry: the contract, its services, its leases and the account that
funded them. The contract **id changes**, so the symptom is not one error but all
of them at once — `LeaseNotFound` for ids that worked yesterday,
[`ServiceNotFound`](contract-reference.md#error-codes) for ids in
[Deployments](deployments.md), and clients that look like they are reading a
broken protocol.

```bash
# Is this network a fresh one? Its ledger is near zero after a reset.
curl -s -X POST "$KEYLEASE_RPC_URL" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getLatestLedger","params":{}}'

# Is this the network you think it is?
curl -s -X POST "$KEYLEASE_RPC_URL" -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getNetwork","params":{}}'
```

The passphrase in the `getNetwork` reply must match the row for your network in
[Networks](networks.md#networks). After a reset: redeploy with
`scripts/deploy-testnet.sh`, re-issue `set_token` and `register_service`, fund
accounts again, then update `KEYLEASE_CONTRACT_ID` and
[Deployments](deployments.md).

---

## For indexers and RPC clients

### Reads work, then start failing with `429`

**Cause:** the public Testnet and Futurenet RPC endpoints are rate-limited and
are not meant for a busy provider
([production notes](networks.md#rpc-production-notes)).

**Fix:** move to an authenticated provider and point `KEYLEASE_RPC_URL` at it.
Back off on `429` rather than retrying immediately, and cache `get_lease` for a
few seconds instead of re-reading per request.

### Your service rejects requests while RPC is down

That is correct. The contract is not reachable during an RPC outage, so a
provider cannot verify a lease and must not serve it
([verification pipeline](integration.md#the-verification-pipeline), step 2). No
funds move: settlement and revocation still work as soon as any RPC responds, and
a consumer's refund path never depends on a provider's infrastructure.

### Events are missing after a restart

**Cause:** the start ledger predates the contract's creation, or is ahead of the
events you need.

**Fix:** re-read from the deploy ledger and check the count:

```bash
stellar events --id $REGISTRY --network testnet --start-ledger <ledger>
```

Event names, topics and payloads are part of the ABI and only ever gain fields;
decode by position-tolerant lookup, and treat a settled lease as final
([Integration](integration.md#following-the-event-stream)).

### Storage reads return nothing

**Cause:** raw `getContractData` needs the exact wire key. `#[contracttype]`
enums encode as a vector whose first element is the variant symbol, so
`DataKey::Lease(7)` is `Vec[Symbol("Lease"), u64(7)]`.

**Fix:** prefer the read views (`get_lease`, `get_service`, `lease_status`) over
raw storage. If you must read the key directly, the mapping table is in
[Reading contract state and events](developer-guide.md#reading-contract-state-and-events).

---

## Error code index

The full table, with the precondition each code guards, is in the
[contract reference](contract-reference.md#error-codes). Codes are part of the
ABI: they are appended and never reused, so a number you see today is the same
number after an upgrade.

## Read next

- [Contract reference](contract-reference.md) — entry points, events, error codes.
- [Integration](integration.md) — the verification path a provider should ship.
- [Networks](networks.md) — endpoints, passphrases, resets.
- [Security model](security.md) — what the contract does not promise.
