# Networks

KeyLease is deployed per Stellar network. Everything that differs between
environments is a network name, an RPC endpoint and a network passphrase — the
contract and its ABI are identical.

## Networks

| Network | Passphrase | RPC | Notes |
| --- | --- | --- | --- |
| Testnet | `Test SDF Network ; September 2015` | `https://soroban-testnet.stellar.org` | SDF-hosted; resets 2–4× per year |
| Futurenet | `Test SDF Future Network ; October 2022` | `https://rpc-futurenet.stellar.org` | SDF-hosted; previews protocol changes |
| Mainnet | `Public Global Stellar Network ; September 2015` | third-party only, e.g. `https://mainnet.sorobanrpc.com` | **SDF does not host a mainnet RPC** |
| Local standalone | `Standalone Network ; February 2017` | `http://localhost:8000/rpc` | `stellar container start local` — see the [local sandbox walkthrough](developer-guide.md#local-sandbox-walkthrough) |

The CLI resolves these names from its config:

```bash
stellar network ls
stellar network use testnet
```

> **Read calls need a source account.** `stellar contract invoke` builds a
> simulation transaction, so even read-only functions need a funded
> `--source-account` (any identity works; it only pays simulation fees). Contract
> events are fetched without a signer:
>
> ```bash
> stellar contract invoke --id $REGISTRY --source-account keylease-admin \
>   --network testnet -- get_lease --lease_id 1
> stellar events --id $REGISTRY --network testnet --start-ledger <ledger>
> ```

## Escrow asset: the native SAC

On Testnet the usual escrow asset is the native Stellar Asset Contract for XLM.
Its address is derived from the network passphrase, not invented:

```bash
stellar contract id asset --network testnet --asset native
```

Any SEP-41 token can be configured instead with `set_token`; the derive command
above is only for the native asset. The admin must call `set_token` before any
lease can be created (`TokenNotSet` otherwise).

## Native SAC vs. Custom SEP-41 Token

KeyLease accepts any contract implementing the SEP-41 token standard. Operators
evaluating which asset to configure should consider the architectural and
operational trade-offs:

| Characteristic | Native SAC (XLM) | Custom SEP-41 Token / Classic SAC |
| --- | --- | --- |
| **Issuance** | Native protocol asset built into the Stellar consensus layer | Minted by an issuer account (classic asset wrapper) or custom token contract |
| **Trustlines** | None required; every funded Stellar account natively holds XLM | Classic assets require explicit trustlines (`ChangeTrust`); contract-native tokens manage balances internally |
| **Decimals** | Fixed at 7 decimals (`1 stroop = 0.0000001 XLM`) | Configurable (commonly 7 for classic wrapped assets, 6 for USDC, or 18 for EVM-style tokens) |
| **Admin controls** | None. No freeze, no clawback, no issuer key, no upgradeability | Issuer may configure clawback, authorization flags (`auth_required`), pausing, or contract upgradeability |
| **Escrow guarantees** | Escrow lock and transfer are guaranteed by host runtime; zero re-entrancy or transfer-failure risk | Follows identical SEP-41 contract interface, but subject to issuer freeze, clawback, or contract bugs |

### Escrow guarantees and risk profile

From KeyLease's perspective, both token types interact through the same standard
interface (`token::Client`). Escrow invariants (`payout + refund == locked_deposit`)
hold equally across both.

However, external contract risk differs:

- **Native SAC**: The transfer cannot be blocked, paused, or redirected by any
  third party.
- **Custom Token**: If a custom token contract fails, pauses, or blacklists a
  participant, calls to `settle_lease` or `revoke_expired_lease` will revert.
  KeyLease uses checks-effects-interactions (`lease.settled = true` persisted
  *before* transfer) to prevent double-spend or re-entrancy attacks, but cannot
  force a failing external token to execute transfers.

### Deployment recommendations

- **Testnet**: Use the **Native SAC (XLM)**. It requires zero setup, is
  automatically derived from the network passphrase, and accounts can be funded
  immediately via Friendbot.
- **Mainnet**: Use **USDC via SAC** for commercial API and RPC leases where price
  volatility between fiat costs and crypto balances is undesirable. Use **Native
  SAC (XLM)** for network infrastructure services, peer-to-peer compute, or
  micro-metered telemetry where native settlement is preferred.

## Funding accounts

Friendbot funds accounts and contracts with **10,000 fake XLM** on Testnet:

```bash
stellar keys generate consumer --network testnet --fund
# or, for an address you already have:
curl "https://friendbot.stellar.org?addr=$(stellar keys address consumer)"
```

The CLI's `--fund` flag and the HTTP endpoint above are for networks with a
friendbot: Testnet, and the local container's own friendbot at
`http://localhost:8000/friendbot` (`stellar keys generate x --network local --fund`
hits it). Mainnet has neither and requires real XLM for fees and for the escrowed
asset. See [Developer guide → Local sandbox walkthrough](developer-guide.md#local-sandbox-walkthrough).

## Testnet resets

Testnet resets wipe all ledger entries, including the deployed contract,
services and leases, **2–4 times per year at 17:00 UTC**.

> After a reset, the contract id changes. Re-run
> [`scripts/deploy-testnet.sh`](https://github.com/KeyLease-Security/keylease-core/blob/main/scripts/deploy-testnet.sh)
> and update [`docs/deployments.md`](deployments.md).

### What a reset actually takes

A reset is not a data purge of the deployment — it is a purge of the whole
ledger, so plan for these to be gone:

| Gone | Survives |
| --- | --- |
| The registry contract instance, its Wasm reference and its `contractmeta!`. | Your local Stellar CLI identities. Seeds are stored on your machine, so `KEYLEASE_ADMIN` / `KEYLEASE_PROVIDER` / `KEYLEASE_CONSUMER` keep the same `G…` addresses. |
| All instance storage: `Admin`, `Token`, `ServiceCount`, `LeaseCount`. | The native SAC address. It is derived from the network passphrase, which a reset does not change — re-derive it with `stellar contract id asset --network testnet --asset native` and expect the same value. |
| All persistent storage: every `Service(id)` and `Lease(id)` entry. | The repository, its CI, and your built Wasm. |
| Every funded account, including the SAC balances and any open lease's escrow. | Nothing on-chain that identifies your old deployment. |

Open leases are the expensive part: their `Lease` entries, and therefore their
refund paths, are gone. Testnet escrow is fake XLM, so the loss is nuisance
rather than damage, but any indexer or provider middleware that caches lease
state has to be rewound too.

### After a Testnet reset

Redeploy in dependency order — the same order `scripts/deploy-testnet.sh`
enforces, because each step reads what the previous one wrote.

```bash
cd keylease-core && git fetch origin && git switch main && git pull --ff-only

# 1. Re-create and re-fund the signing identity (Friendbot creates the account
#    if it is missing; the address is the same as before the reset).
stellar keys generate keylease-admin --network testnet --fund
#    Reusing an existing seed instead of generating one:
curl "https://friendbot.stellar.org?addr=$(stellar keys address keylease-admin)"

# 2. Build, deploy, init, set_token and register_service in one pass.
SOURCE_ACCOUNT=keylease-admin ./scripts/deploy-testnet.sh
```

The script's steps 2–5 are the redeploy, re-init, token-set and re-register
steps. Run them individually only if you need to interpose something:

```bash
REGISTRY=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account keylease-admin --network testnet | tail -n1)

stellar contract invoke --id "$REGISTRY" --source-account keylease-admin \
  --network testnet --send=yes -- init --admin "$(stellar keys address keylease-admin)"

TOKEN=$(stellar contract id asset --network testnet --asset native)
stellar contract invoke --id "$REGISTRY" --source-account keylease-admin \
  --network testnet --send=yes -- set_token \
     --admin "$(stellar keys address keylease-admin)" --token "$TOKEN"

stellar contract invoke --id "$REGISTRY" --source-account keylease-admin \
  --network testnet --send=yes -- register_service \
     --provider "$(stellar keys address keylease-admin)" \
     --rate_per_call 1000000 --endpoint_hash <64_HEX_CHARS>
```

`register_service` returns `1` again. That is expected, not a collision:
`ServiceCount` lived in instance storage, and the instance is new. Lease ids
restart the same way. Consumers holding a `service_id` from before the reset must
re-read it from [Deployments](deployments.md) or from the script output.

### After a reset: update the record and the config

1. **`.env`** — copy the values the script prints. The three that change every
   reset are `KEYLEASE_CONTRACT_ID`, `KEYLEASE_SERVICE_ID` and, if you rebuilt,
   nothing else; `KEYLEASE_TOKEN_ADDRESS`, `KEYLEASE_NETWORK`,
   `KEYLEASE_RPC_URL` and the account addresses do not change. Restart anything
   that reads the file — clients pick the contract id up at construction.
2. **`docs/deployments.md`** — replace the Testnet row's `KeyLeaseRegistry` id,
   contract version, Wasm hash, the `Contract created` ledger and the four tx
   links for `init` / `set_token` / `register_service`. Leave the "Escrow token
   (native SAC)" row alone unless you switched assets. Follow
   [Recording a deployment](deployments.md#recording-a-deployment), and tag the
   revision so a reader can tie the new ids to a commit.
3. **Cursors** — an indexer following the old contract id has nothing to read
   and will not error loudly. Point it at the new id and start it at the
   deployment ledger; see [Integration](integration.md).

### Check the old contract id is dead

Do this before announcing the new id, so a stale pointer cannot be mistaken for
a working one. Both commands must fail, because no ledger entry backs the old id:

```bash
OLD=CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62   # from git history

stellar contract info fetch --id "$OLD" --network testnet
stellar contract invoke --id "$OLD" --source-account keylease-admin \
  --network testnet -- version
stellar events --id "$OLD" --network testnet --start-ledger <ledger just after the reset>
```

`info fetch` reports the entry is missing and the invoke fails during simulation
— there is no instance to load. `stellar events` answers with an empty list, not
an error: absence of history is the same fact stated quietly.

If `version()` **answers**, you are not looking at a reset network. Either the
reset has not happened yet, or the RPC you are pointing at is a different
network — check `stellar network ls` and the passphrase above before publishing
anything.

## Explorer links

Each network has a contract explorer. Substitute the id from
[Deployments](deployments.md):

| Network | Contract URL |
| --- | --- |
| Testnet | `https://stellar.expert/explorer/testnet/contract/<CONTRACT_ID>` |
| Mainnet | `https://stellar.expert/explorer/public/contract/<CONTRACT_ID>` |

## RPC production notes

- Testnet and Futurenet public RPCs are rate-limited. They are fine for demos
  and reads, not for a busy provider.
- For production, use an authenticated RPC provider and pass its URL through
  `KEYLEASE_RPC_URL`.
- A provider's verification path fails closed when RPC is unreachable: it must
  reject requests rather than admit unverified ones. No funds move either way,
  and settlement/revocation remain possible directly through the contract.

## Read next

- [Deployment](deployment.md) — the ordered sequence.
- [Integration](integration.md) — how to read leases and events from a service.
