# Deployments

Only trust contract ids listed on this page. Any other "KeyLease" contract id is
not ours. Addresses are filled in from the output of
[`scripts/deploy-testnet.sh`](https://github.com/KeyLease-Security/keylease-core/blob/main/scripts/deploy-testnet.sh) —
never guess one.

## Testnet

| Field | Value |
| --- | --- |
| Status | ✅ live |
| Network | `testnet` |
| Network passphrase | `Test SDF Network ; September 2015` |
| RPC | `https://soroban-testnet.stellar.org` |
| `KeyLeaseRegistry` | `CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62` |
| Contract version (`version()`) | `0.2.1` |
| Wasm hash | `5362d4cb866a0405f17d1adfe8179e63afaca8a7dc741df3c3b87ee0ab75146a` |
| Escrow token (native SAC) | `CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC` |
| Admin / provider | `GBDHDKBXURVBLAJNGBSAH4AHXRI2R3GFCEU5PCP6LI34SCETSBBLMUUT` |
| Demo consumer | `GALF2TZRNCJWY7I2AHI2ORNZMDYFPRDNZXDV2DIALRLGDM6JZT35HPRY` |
| Demo `service_id` | `1` — rate `1000000` stroops/call, endpoint hash `2c119e2f7de1bec4d43b2d6aaf45b1d9f3b1d5405317bc7b395f1d62a1504f23` |
| Contract created | ledger `5382788` — tx [`f598d9a2…f0172`](https://stellar.expert/explorer/testnet/tx/f598d9a2744c4dc87b8a3cfbceca3374008942bbbcdbc950965ca585882f0172) |
| `init` | ledger `5382790` — tx [`fd33b90e…9565c`](https://stellar.expert/explorer/testnet/tx/fd33b90e81ac07953da60dab18483af291b781834b4f69e734be8096e879565c) |
| `set_token` | ledger `5382791` — tx [`cea5662e…3d3c97`](https://stellar.expert/explorer/testnet/tx/cea5662ed365a272035652e6c23e9ad9cb06dcef64f33a9097eceb04233d3c97) |
| `register_service` | ledger `5382792` — tx [`9cfb9050…f9ff5`](https://stellar.expert/explorer/testnet/tx/9cfb9050f86b189855aa377c51430ff1fae436961e6b7b32c3225a83070f9ff5) |
| Deployed from | v0.2.1 release commit (the revision that records this table) |
| Explorer | <https://stellar.expert/explorer/testnet/contract/CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62> |

### On-chain metadata

`stellar contract info meta --id $REGISTRY --network testnet` returns the
`contractmeta!` embedded in the deployed Wasm:

```text
name:        KeyLeaseRegistry
version:     0.2.1
repository:  https://github.com/KeyLease-Security/keylease-core
description: Time-bound micro-leases for API/RPC access, escrowed with SEP-41 tokens on Stellar.
rssdkver:    26.1.1
```

### Lifecycle verified on-chain

Two real leases were created and settled against the live contract. The escrow
moved through the native Stellar Asset Contract.

| Step | Output |
| --- | --- |
| `create_lease` (lease 1, 100 calls, 1h) | ledger `5382801` — tx [`bea25af0…4b90`](https://stellar.expert/explorer/testnet/tx/bea25af03a45dc830440cd4ee77135a4f1a3c4eb74dc0d403df9a2a7e80a4b90); `locked_deposit = 100000000` |
| `settle_lease` (lease 1, 100 verified) | ledger `5382802` — tx [`f34bff56…bde22`](https://stellar.expert/explorer/testnet/tx/f34bff56afa59e16439f3346c4842465edc2ce3a55280f8060b262658a9bde22); `payout = 100000000`, `refund = 0` |
| `create_lease` (lease 2, 10 calls, 60s) | ledger `5382814` — tx [`ab4f69e2…0ca21`](https://stellar.expert/explorer/testnet/tx/ab4f69e26189fae5f71ece6f0c9b1e8aa1dd29656e109514b920f33df4b0ca21); `locked_deposit = 10000000` |
| `settle_lease` (lease 2, 4 verified after expiry) | ledger `5382835` — tx [`7a6a939d…5a01`](https://stellar.expert/explorer/testnet/tx/7a6a939deb07e6993da544d1e24e755194c783d9e8fef29c2beb7d8cd4975a01); `payout = 4000000`, `refund = 6000000` |

Both settlements emitted `lease_settled`; every create emitted `lease_created`.
Replay the whole history with:

```bash
stellar events --id CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62 \
  --network testnet --start-ledger 5382788
```

### Deployment economics (Testnet)

The Wasm upload reserves on-chain code rent (~3,557 XLM for this build at the
current Testnet settings). Lease creation pays storage rent as part of the
transaction resource fee: the host enforces a minimum persistent TTL (~7 days on
Testnet), so the first lease (which also funded the contract's own SAC balance
entry) cost ~20 XLM, a following 10-call lease cost ~5.6 XLM, and settlement is
a few thousand stroops. Mainnet rent parameters differ; size allotments against
real fees before quoting per-call prices.

## Mainnet

| Field | Value |
| --- | --- |
| Status | ⛔ not deployed |
| Network | `mainnet` |
| Network passphrase | `Public Global Stellar Network ; September 2015` |
| RPC | third-party only, e.g. `https://mainnet.sorobanrpc.com` |

Mainnet deployment is intentionally deferred until the contract is audited. See
[Security](security.md) and
[SECURITY.md](https://github.com/KeyLease-Security/keylease-core/blob/main/SECURITY.md).

## Verifying a listed address

The registry can be checked without trusting this page: the Wasm carries
`contractmeta!`, and the configured token and admin are public views.

```bash
stellar contract invoke --id <ID> --source-account <funded-account> --network testnet -- version
stellar contract invoke --id <ID> --source-account <funded-account> --network testnet -- get_admin
stellar contract invoke --id <ID> --source-account <funded-account> --network testnet -- get_token

# lifecycle history, straight from the ledger
stellar events --id <ID> --network testnet --start-ledger <ledger>
```

`version()` must match the release this page claims, `get_admin()` must be the
documented admin, and `escrow_token_set` / `service_registered` events must
appear at the recorded deployment ledger.

The deployed Wasm is reproducible from this tree — its hash matches
`stellar contract info hash`:

```bash
cargo build --target wasm32v1-none --release --all
sha256sum target/wasm32v1-none/release/keylease_registry.wasm
# 5362d4cb866a0405f17d1adfe8179e63afaca8a7dc741df3c3b87ee0ab75146a
```

## Recording a deployment

1. Run `./scripts/deploy-testnet.sh`.
2. Copy the printed values into the Testnet table above, including the contract
   version.
3. Record the deployment ledger so `stellar events` can be replayed.
4. Commit the change and tag the release so the addresses are tied to a
   specific revision.
