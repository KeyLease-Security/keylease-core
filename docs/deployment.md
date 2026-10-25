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

```
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
