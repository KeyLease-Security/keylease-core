# Deployments

Only trust contract ids listed on this page. Any other "KeyLease" contract id is
not ours. Addresses are filled in from the output of
[`scripts/deploy-testnet.sh`](../scripts/deploy-testnet.sh) — never guess one.

## Testnet

| Field | Value |
| --- | --- |
| Status | ⏳ not yet deployed |
| Network | `testnet` |
| Network passphrase | `Test SDF Network ; September 2015` |
| RPC | `https://soroban-testnet.stellar.org` |
| `KeyLeaseRegistry` | — |
| Escrow token (SAC) | — |
| Demo `service_id` | — |
| Deployed at (commit) | — |

## Mainnet

| Field | Value |
| --- | --- |
| Status | ⛔ not deployed |
| Network | `mainnet` |
| Network passphrase | `Public Global Stellar Network ; September 2015` |
| RPC | third-party only, e.g. `https://mainnet.sorobanrpc.com` |

Mainnet deployment is intentionally deferred until the contract is audited. See
[Security](security.md) and [SECURITY.md](../SECURITY.md).

## Recording a deployment

1. Run `./scripts/deploy-testnet.sh`.
2. Copy the printed values into the Testnet table above.
3. Commit the change and tag the release so the addresses are tied to a
   specific revision.
