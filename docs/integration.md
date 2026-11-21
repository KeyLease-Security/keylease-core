# Integration

KeyLease has no privileged off-chain service and no bespoke credential format.
A provider integrates with the Stellar ledger directly: it verifies the lease on
chain, binds the caller to the lease's consumer using Stellar account
authentication, and settles through the contract.

This page describes the supported pattern. Any implementation that follows it is
compatible with the protocol.

## What the credential is

The **lease is the credential**. `get_lease(lease_id)` returns the authoritative
terms: `allocated_calls`, `used_calls`, `expires_at`, `settled`, and the
`consumer` who owns the refund path. A provider admits a request only when that
on-chain record says it is allowed.

There is nothing else to issue: no API key, no session JWT, no signing secret.
The consumer's identity is its Stellar address (`G…` or `C…`).

## The verification pipeline

A provider should implement these steps, in order, and fail closed at every one:

1. **Authenticate the caller with Stellar.** Use a challenge–response flow with
   the consumer's Stellar key; [SEP-10 Stellar Web Authentication][sep10] is the
   standard way to prove account ownership to an off-chain service. The result is
   a trusted `Address` for the request — not a password.
2. **Resolve the lease on chain.** Call `get_lease(lease_id)` over Soroban RPC
   and cache the result briefly (a few seconds is usually enough for a busy
   provider). An unreachable RPC is a **503**, never an allow.
3. **Check the lease.** The lease must exist, must not be `settled`, and
   `ledger.timestamp() < expires_at`. Reject otherwise (`403`).
4. **Check ownership.** The authenticated address must equal `lease.consumer`.
   Otherwise reject (`403`). This is what makes a stolen lease id worthless.
5. **Enforce the quota.** Use `max(lease.used_calls, local_counter)` against
   `lease.allocated_calls`; reject with `429` when exhausted, and increment the
   counter only for admitted requests.
6. **Serve the request**, adding the remaining-calls header if you want the
   client to see its budget.
7. **Settle** with `settle_lease` when the lease expires or when the allotment is
   fully consumed.

Step 5's local counter is an optimisation, not the source of truth. On-chain
`allocated_calls × rate_per_call` bounds what the provider can ever be paid, so a
tampered counter can only make the provider charge **less**. It can never
over-charge the consumer.

[sep10]: https://developers.stellar.org/docs/learn/encyclopedia/security/authentication

## Reading a lease

With the Stellar CLI:

```bash
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet -- get_lease --lease_id 1
```

With `@stellar/stellar-sdk`, simulate a `get_lease` invocation and decode the
result:

```ts
import {
  Contract, TransactionBuilder, Networks, Account,
  nativeToScVal, scValToNative, rpc,
} from '@stellar/stellar-sdk';

const server = new rpc.Server(process.env.KEYLEASE_RPC_URL!);
const contract = new Contract(process.env.KEYLEASE_CONTRACT_ID!);

export async function readLease(leaseId: bigint) {
  const source = new Account(
    'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF',
    '0',
  );
  const tx = new TransactionBuilder(source, {
    fee: '100',
    networkPassphrase: Networks.TESTNET,
  })
    .addOperation(contract.call('get_lease', nativeToScVal(leaseId, { type: 'u64' })))
    .setTimeout(30)
    .build();

  const sim = await server.simulateTransaction(tx);
  if (!rpc.Api.isSimulationSuccess(sim) || !sim.result) {
    throw new Error('get_lease failed');
  }
  return scValToNative(sim.result.retval) as {
    lease_id: bigint;
    service_id: bigint;
    consumer: string;
    allocated_calls: number;
    used_calls: number;
    created_at: bigint;
    expires_at: bigint;
    locked_deposit: bigint;
    settled: boolean;
  };
}
```

The read views require no authorization, so simulation is enough — a provider
never needs a Stellar secret key to verify a lease. That is deliberate:
compromising an enforcement tier does not compromise escrowed funds.

## Following the event stream

For accounting, dashboards and settlement automation, subscribe to the contract's
events instead of polling every lease:

```bash
stellar events --id $REGISTRY --network testnet --start-ledger <ledger>
```

An event's **first topic is always the event name** as a symbol; the fields
marked `#[topic]` in `contracts/registry/src/events.rs` follow it, and every
remaining field is carried in the payload map. Topics are what a subscriber
filters on, so which side of that line a field falls on decides whether you can
act on it without decoding.

### Topics, payloads and filters

The `--topic` filter takes base64-encoded `ScVal` segments, one per topic
position, and matches only events with **exactly** that many topics unless the
last segment is the `**` wildcard. A `*` segment matches any single topic.

| Event | Topics (in order) | Payload-only fields | Filter |
| --- | --- | --- | --- |
| `lease_created` | name, `lease_id` | `service_id`, `consumer`, `allocated_calls`, `expires_at`, `locked_deposit` | `--topic "AAAADwAAAA1sZWFzZV9jcmVhdGVkAAAA,*"` |
| `lease_settled` | name, `lease_id` | `provider`, `verified_calls`, `payout`, `refund` | `--topic "AAAADwAAAA1sZWFzZV9zZXR0bGVkAAAA,*"` |
| `lease_revoked` | name, `lease_id` | `consumer`, `refund` | `--topic "AAAADwAAAA1sZWFzZV9yZXZva2VkAAAA,*"` |
| `service_registered` | name, `service_id` | `provider`, `rate_per_call`, `endpoint_hash` | `--topic "AAAADwAAABJzZXJ2aWNlX3JlZ2lzdGVyZWQAAA==,*"` |
| `service_status_changed` | name, `service_id` | `provider`, `is_active` | `--topic "AAAADwAAABZzZXJ2aWNlX3N0YXR1c19jaGFuZ2VkAAA=,*"` |
| `contract_upgraded` | name only | `admin`, `new_wasm_hash` | `--topic "AAAADwAAABFjb250cmFjdF91cGdyYWRlZAAAAA=="` |
| `registry_initialized` | name only | `admin` | `--topic "AAAADwAAABRyZWdpc3RyeV9pbml0aWFsaXplZA=="` |
| `escrow_token_set` | name only | `admin`, `token` | `--topic "AAAADwAAABBlc2Nyb3dfdG9rZW5fc2V0"` |

Note that only five of the eight events carry an id topic at all:
`contract_upgraded`, `registry_initialized` and `escrow_token_set` have **no**
id topics, so they can only be narrowed by contract id, and a lease's
`service_id` is payload-only — a per-service queue must decode the payload
rather than filter. Regenerate any of these strings yourself when you need a
different id, e.g. for lease 1:

```bash
printf '{"symbol":"lease_created"}' | stellar xdr encode --type ScVal
# AAAADwAAAA1sZWFzZV9jcmVhdGVkAAAA
printf '{"u64":"1"}' | stellar xdr encode --type ScVal
# AAAABQAAAAAAAAAB
stellar events --id $REGISTRY --network testnet --start-ledger <ledger> \
  --topic "AAAADwAAAA1sZWFzZV9jcmVhdGVkAAAA,AAAABQAAAAAAAAAB"
```

`--output json` and `--output pretty` decode names and fields for you, which is
usually enough for a one-off check; the encoded filters matter when a subscriber
needs the server to narrow the stream.

### Recipes

- **`lease_created`** — open (or upsert) a lease record keyed by `lease_id`,
  storing `service_id`, `consumer`, `allocated_calls`, `expires_at` and
  `locked_deposit` from the payload, and push it onto the settlement queue
  ordered by `expires_at`. This is the only event that tells you escrow exists,
  so anything you do not record here must be re-read with `get_lease`.
- **`lease_settled`** — close the lease in your ledger and reconcile: assert
  `payout + refund == locked_deposit` from the create event, and credit the
  provider `payout`. A `payout` of `0` means the provider refunded a lease it
  could not serve, which is worth alerting on.
- **`lease_revoked`** — mark the lease closed with **no** revenue and record it
  against the provider's reliability. `refund` here is always the full
  `locked_deposit`. This event is also the only way to tell a revocation apart
  from a zero-call settlement, because both leave `settled = true`: see
  [LeaseStatus and the `settled` flag](contract-reference.md#leasestatus-and-the-settled-flag).
- **`service_status_changed`** — update the catalogue entry for `service_id`.
  `is_active = false` blocks *new* `create_lease` calls only, so stop quoting the
  service to new consumers while still serving and settling leases you already
  hold.
- **`contract_upgraded`** — the running logic changed. Take `new_wasm_hash` from
  the payload, confirm `version()` reports the release you expected, and re-run
  the post-upgrade checks in
  [Upgrading](deployment.md#upgrading-safety-checklist) before trusting new
  leases. Reconcile any in-flight lease counters your indexer holds, because the
  payload carries no ids to scope this to.
- **`service_registered`** — add the service to the catalogue with its
  `rate_per_call` and `endpoint_hash`; the hash is the commitment your enforcement
  layer compares the real endpoint against.
- **`registry_initialized` / `escrow_token_set`** — deployment-lifecycle events.
  Treat `escrow_token_set` as a hard stop: the escrow asset changed, so the unit
  of every `payout` and `refund` you have been accumulating changed with it.

Every id you can filter on is in the table above; event names, topics and payload
fields are part of the ABI and are listed in the
[contract reference](contract-reference.md#events).

### Event schema version

Every payload includes a `schema_version` key (`u32`), and the contract also
publishes it as on-chain metadata under `eventSchemaVersion`. The number
describes the **event schema**, not the release: `version` moves on every deploy,
`schema_version` only when an event gains or loses a field. Fields are added and
never removed, renamed or retyped, and topics never change, so an old decoder
keeps working on new events.

Branch on the payload key, not on the contract's `version`:

| Payload says | What to do |
| --- | --- |
| no `schema_version` key | Revision 0: an event from before versioning was introduced. Its fields are today's fields minus `schema_version`, so decode it with the current parser and ignore the missing key. |
| `0 < schema_version <= 1` | Decode the keys you know and ignore any extra key. A payload newer than your build within the same major revision stays readable by design. |
| `schema_version > 1` | Your decoder is behind the deployment. Store the event verbatim, surface it as unhandled, and alert — do not guess at the shape of an escrow record. |

```python
version = payload.get("schema_version", 0)   # absent means pre-versioning
if version > SUPPORTED_SCHEMA_VERSION:
    quarantine(event)                        # never silently drop escrow
```

Prefer the payload key over the `eventSchemaVersion` metadata when decoding
history: metadata describes the Wasm running *now*, while an event from an old
ledger was emitted by the schema that was running *then*. Read the metadata once
at startup to learn the current version, and index from there.

## Storage lifetime

Lease and service entries are persistent entries with a bounded lifetime, and
reading one no longer extends it. If you keep a long-lived projection of leases,
you may want to keep the on-chain record alive alongside it:

```bash
stellar contract invoke --id $REGISTRY --source-account indexer --network testnet \
  -- bump_lease --lease_id 1
```

`bump_lease` and `bump_service` are permissionless; whoever calls them pays the
rent for the extension. A bump can only lengthen a lifetime, never shorten it, and
`bump_lease` rejects a settled lease, so there is nothing to grief with an entry
whose funds have already moved. Settlement itself and `create_lease` keep the
entries they touch alive on their own; you only need an explicit bump for state
that is being read and not written for the length of its horizon.

## Settling

Settlement is one signed transaction from the provider account, at or after
expiry, or as soon as every allocated call is consumed:

```bash
stellar contract invoke --id $REGISTRY --source-account provider --network testnet \
  --send=yes -- settle_lease \
    --provider $PROVIDER --lease_id 1 --verified_calls 320
```

Reporting `verified_calls = 0` refunds the entire deposit for a lease you could
not serve. Choose a settlement cadence that fits your traffic: one settlement per
lease, not per request.

## Hardening checklist

- [ ] Verify leases server-side; never trust a `lease_id` supplied by the client
      without checking `consumer` against an authenticated Stellar address.
- [ ] Fail closed on RPC errors, malformed lease data and unknown leases.
- [ ] Cache lease reads briefly (≈5–30 s) but never past the request you are
      authorising; re-read on a cache miss or when close to `expires_at`.
- [ ] Share the per-lease counter if you run more than one instance, or accept
      that each instance can grant the full allotment (the provider only loses
      revenue; the consumer is still bounded on chain).
- [ ] Keep provider signing keys out of the request path where possible: reads
      need no key, only settlement does.
- [ ] Watch `lease_created` so no lease expires unsettled and becomes revocable.
- [ ] Never log Stellar secret seeds.

## Read next

- [Networks](networks.md) — RPC endpoints and passphrases.
- [Provider guide](provider-guide.md) — the operational view.
- [Contract reference](contract-reference.md) — the ABI you are calling.
