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
| Local standalone | `Standalone Network ; February 2017` | your local RPC | `stellar network container start` / `stellar scaffold` |

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

## Funding accounts

Friendbot funds accounts and contracts with **10,000 fake XLM** on Testnet:

```bash
stellar keys generate consumer --network testnet --fund
# or, for an address you already have:
curl "https://friendbot.stellar.org?addr=$(stellar keys address consumer)"
```

The CLI's `--fund` flag and the HTTP endpoint above are Testnet-only; Mainnet
requires real XLM for fees and for the escrowed asset.

## Testnet resets

Testnet resets wipe all ledger entries, including the deployed contract,
services and leases, **2–4 times per year at 17:00 UTC**.

> After a reset, the contract id changes. Re-run
> [`scripts/deploy-testnet.sh`](https://github.com/KeyLease-Security/keylease-core/blob/main/scripts/deploy-testnet.sh)
> and update [`docs/deployments.md`](deployments.md).

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
