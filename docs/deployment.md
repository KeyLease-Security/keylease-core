# Deployment

Deploying KeyLease is a short, strictly ordered sequence of `stellar contract`
commands against a Stellar network. Steps 2–5 each depend on the previous one,
so the whole thing is scripted:

```bash
SOURCE_ACCOUNT=keylease-admin ./scripts/deploy-testnet.sh
```

To create and fund the identity as well (Testnet only):

```bash
KEYLEASE_GENERATE_KEY=1 SOURCE_ACCOUNT=keylease-admin ./scripts/deploy-testnet.sh
```

The script never deploys `mock_token`. That token has an unlimited mint and is
for tests and local sandboxes only.

## What the script does

```text
1. cargo build --target wasm32v1-none --release --all
2. stellar contract deploy   <registry wasm>  -> REGISTRY contract id
3. stellar contract invoke -- init            --admin  <ADMIN>
4. stellar contract invoke -- set_token       --admin <ADMIN> --token <TOKEN>
5. stellar contract invoke -- register_service --provider <P> \
                              --rate_per_call <R> --endpoint_hash <H>
```

Then it re-reads `get_service` to confirm the write landed, and prints a block
you can paste into `.env`:

```text
KEYLEASE_CONTRACT_ID=C...
KEYLEASE_SERVICE_ID=1
KEYLEASE_TOKEN_ADDRESS=...
KEYLEASE_NETWORK=testnet
KEYLEASE_RPC_URL=https://soroban-testnet.stellar.org
```

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `SOURCE_ACCOUNT` | `keylease-admin` | stellar CLI identity used to sign |
| `NETWORK` | `testnet` | network name from your CLI config |
| `REGISTRY_ALIAS` | `keylease-registry` | contract alias saved locally |
| `ADMIN` | address of `SOURCE_ACCOUNT` | registry admin |
| `PROVIDER` | `ADMIN` | owner of the demo service |
| `TOKEN` | native SAC on `NETWORK` | escrow token |
| `RATE_PER_CALL` | `1000000` | 0.1 XLM at 7 decimals |
| `ENDPOINT_HASH` | `sha256("<PROVIDER>:keylease-demo")` | 32-byte commitment |
| `KEYLEASE_GENERATE_KEY` | unset | `1` to generate + Friendbot-fund the identity |

## Doing it manually

The script is the supported path, but the raw commands are:

```bash
# 0. identity
stellar keys generate keylease-admin --network testnet --fund

# 1. build
rustup target add wasm32v1-none
cargo build --target wasm32v1-none --release --package keylease-registry

# 2. deploy
REGISTRY=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account keylease-admin \
  --network testnet \
  --alias keylease-registry)

# 3. initialize
ADMIN=$(stellar keys address keylease-admin)
stellar contract invoke --id "$REGISTRY" --source-account keylease-admin \
  --network testnet --send=yes -- init --admin "$ADMIN"

# 4. configure the escrow token (native SAC here)
TOKEN=$(stellar contract id asset --network testnet --asset native)
stellar contract invoke --id "$REGISTRY" --source-account keylease-admin \
  --network testnet --send=yes -- set_token --admin "$ADMIN" --token "$TOKEN"

# 5. publish a service
stellar contract invoke --id "$REGISTRY" --source-account keylease-admin \
  --network testnet --send=yes -- register_service \
    --provider "$ADMIN" \
    --rate_per_call 1000000 \
    --endpoint_hash 0707070707070707070707070707070707070707070707070707070707070707
```

> `--send=yes` is required for the state-changing invocations. Without it the CLI
> only simulates. The `--` before the function name is required.

## Verifying a deployment

```bash
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- version
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- get_admin
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- get_token
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- get_service --service_id 1
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- service_count
```

`version` is embedded by `contractmeta!`, so it proves which release is live.
The contract id itself is in the registry: see
[Deployments](deployments.md), and the explorer link for that network in
[Networks](networks.md).

## Upgrading

The contract is upgradeable in place through the Soroban deployer; the contract
id, storage and balances are preserved.

```bash
# 1. upload the new Wasm and print its hash
WASM_HASH=$(stellar contract upload \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account keylease-admin --network testnet)

# 2. install it (admin auth)
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet --send=yes -- upgrade \
    --admin $ADMIN --new_wasm_hash "$WASM_HASH"

# 3. confirm the new version and watch the event
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- version
stellar events --id $REGISTRY --network testnet --start-ledger <ledger>
```

Or use the script:

```bash
REGISTRY=C... SOURCE_ACCOUNT=keylease-admin ./scripts/deploy-testnet.sh --upgrade
```

Guard rails for an upgrade: keep the storage layout unchanged, append error
codes instead of renumbering, and keep event names and payload fields
backwards-compatible. Settle or revoke in-flight leases before changing token
semantics. See [Security](security.md#known-limitations).

### Upgrading safety checklist

An upgrade replaces the code but keeps the contract id, so every stored entry
written by the old Wasm is still there when the new Wasm runs. The new code does
not get a clean slate — it gets your users' escrow. Work the checklist in order.

**1. Invariants the new Wasm must preserve.** These are the reasons the guard
rails above exist; the full layout is in
[Storage layout](contract-reference.md#storage-layout).

| Invariant | Why an upgrade can silently break it |
| --- | --- |
| `DataKey` variant names and arities: `Admin`, `Token`, `ServiceCount`, `LeaseCount`, `Service(u64)`, `Lease(u64)` | A `#[contracttype]` enum key encodes as `Vec[Symbol("<variant>"), …]`, so renaming `Lease` to `Leases` orphans every existing entry. The registry then reads `LeaseNotFound` on leases that still hold escrow. |
| Stored value shapes for `Service` and `Lease` | Entries decode by field name and type. Renaming `used_calls`, retyping `locked_deposit`, or removing `settled` reinterprets or loses live records. |
| Field presence, not field order, for structs | Old entries are read by key, so adding a field is safe **only** if the accessor tolerates its absence in entries written before the upgrade. |
| `Error` codes, append-only (1–16 today) | Callers and indexers branch on the number; a reused code changes what a past transaction meant. |
| Event names, topics and payload fields, append-only | They are the indexing ABI. Dropping a `#[topic]` field shifts every subscriber's filter, including the base64 segments in [Topics, payloads and filters](integration.md#topics-payloads-and-filters). |
| `LeaseStatus` discriminants (`0`/`1`/`2`) | Returned by `lease_status` and cached by clients. |
| The `settled`-before-token-call ordering in `settle_lease` and `revoke_expired_lease` | It is the double-payout guard; reordering it behind a transfer reintroduces the re-entrancy bug the suite proves closed. |
| The lease TTL policy covers term + 24 h grace + read buffer | If a shorter policy evicts a lease entry while escrow is still locked, the consumer's revoke path disappears. |
| `MAX_LEASE_DURATION_SECS` never lowered below an in-flight lease's term | Existing leases stay settleable; lowering the cap for *new* leases is additive, lowering it retroactively is not. |

**2. Record the live state before you touch anything.**

```bash
# version, counters and the exact Wasm you are replacing
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- version
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- service_count
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- lease_count
sha256sum target/wasm32v1-none/release/keylease_registry.wasm
```

Keep the highest pre-upgrade `lease_id` and note the current ledger: it is the
`--start-ledger` for the replay in step 5.

**3. Upgrade.** Upload, install, and keep the hash the deployer returned.

```bash
WASM_HASH=$(stellar contract upload \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account keylease-admin --network testnet)
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet --send=yes -- upgrade --admin $ADMIN --new_wasm_hash "$WASM_HASH"
```

**4. Compare `version()`.** The embedded metadata must move; a build that did not
bump the workspace version installed new code that still *reports* the old one.

```bash
stellar contract invoke --id $REGISTRY --source-account keylease-admin --network testnet -- version
stellar contract info meta --id $REGISTRY --network testnet
```

`version` must equal the release you intended, and `name` must still be
`KeyLeaseRegistry`. Then re-read one lease and one service created **before** the
upgrade and diff the field values against step 2 — identical `expires_at`,
`allocated_calls`, `locked_deposit` and `settled` is what proves the stored
shapes were not reinterpreted.

**5. Replay the events across the boundary.**

```bash
stellar events --id $REGISTRY --network testnet --start-ledger <ledger-from-step-2>
```

Expect exactly one `contract_upgraded` carrying the `new_wasm_hash` from step 3,
with nothing else in that transaction; the counters must be unchanged across it
(`service_count` and `lease_count` before equals after — an upgrade mints no
leases); and every `lease_created` before the upgrade must still have a terminal
`lease_settled` or `lease_revoked` after it. If a pre-upgrade lease that was
`Active` now reverts on `settle_lease`, stop: the new Wasm is not storage-safe.

**6. Only then resume traffic**, and record the outcome — the new version, Wasm
hash and upgrade ledger — in [Deployments](deployments.md), which ties a listed
address to a revision.

**Rollback.** `upgrade` is symmetric: install the previous Wasm's hash the same
way. It is only safe while the new build has written nothing the old one cannot
read, so a new `DataKey` variant or new field is a one-way door the moment it is
used.

## Post-deploy

1. **Fund a consumer** so `create_lease` can escrow a deposit.
2. **Point your client or indexer at the registry** with
   `KEYLEASE_CONTRACT_ID`, `KEYLEASE_RPC_URL` and `KEYLEASE_NETWORK`.
3. **Subscribe to events**, or read `get_lease` on demand — see
   [Integration](integration.md).

## Networks

Testnet, Futurenet, Mainnet and local standalone differ only in RPC endpoint and
passphrase; the full table, SAC addresses and reset schedule are in
[Networks](networks.md).

## Mainnet

Not yet. Mainnet deployment is deferred until the contract is audited — see
[Security](security.md) and the known limitations listed there.

## Read next

- [Networks](networks.md) — endpoints, passphrases and resets.
- [Deployments](deployments.md) — the address registry.
- [Integration](integration.md) — verifying leases from your service.
