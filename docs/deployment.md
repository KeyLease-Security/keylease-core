# Deployment

Deploying KeyLease is a short, strictly ordered sequence. Steps 2–5 each depend
on the previous one, so the whole thing is scripted:

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
stellar contract invoke --id $REGISTRY --network testnet -- get_admin
stellar contract invoke --id $REGISTRY --network testnet -- get_token
stellar contract invoke --id $REGISTRY --network testnet -- get_service --service_id 1
stellar contract invoke --id $REGISTRY --network testnet -- service_count
```

Finally, record the addresses in [`docs/deployments.md`](deployments.md) and tag
the release so the addresses are tied to a revision.

## Post-deploy

1. **Fund a consumer** so `create_lease` can escrow a deposit.
2. **Point the gateway at the registry** with `KEYLEASE_CONTRACT_ID` and
   `KEYLEASE_SERVICE_ID`.
3. **Host the proxy** — see [Hosting topology](hosting-topology.md).

## Networks

| Network | Passphrase | RPC |
| --- | --- | --- |
| Testnet | `Test SDF Network ; September 2015` | `https://soroban-testnet.stellar.org` |
| Futurenet | `Test SDF Future Network ; October 2022` | `https://rpc-futurenet.stellar.org` |
| Mainnet | `Public Global Stellar Network ; September 2015` | third-party only, e.g. `https://mainnet.sorobanrpc.com` |
| Local standalone | `Standalone Network ; February 2017` | your local RPC |

⚠️ **Testnet resets 2–4 times per year at 17:00 UTC**, wiping all ledger entries
and invalidating your contract id. The next scheduled 2026 reset is
**December 16, 2026**. Redeploy afterwards.

## Mainnet

Not yet. Mainnet deployment is deferred until the contract is audited — see
[Security](security.md) and the known limitations listed there.

## Read next

- [Hosting topology](hosting-topology.md) — where each process runs.
- [Deployments](deployments.md) — the address registry.
