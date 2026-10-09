# KeyLease

> **Time-bound, ephemeral micro-leases for API & RPC providers on Stellar/Soroban.**

KeyLease replaces long-lived static API keys with **leases**: short-lived,
on-chain authorizations backed by an escrowed micropayment. Providers publish a
price instead of a secret; consumers pay only for the calls they use.

- **Repository:** [keylease-core](https://github.com/KeyLease-Security/keylease-core) — the Soroban contracts
- **Gateway:** [keylease-gateway](https://github.com/KeyLease-Security/keylease-gateway) — CLI + edge proxy
- **License:** MIT

## Start here

| I want to… | Read |
| --- | --- |
| understand what KeyLease is and why | [Introduction](introduction.md) |
| see how the pieces fit together | [Architecture](architecture.md) |
| understand the state machine and the economics | [How the protocol works](protocol.md) |
| look up an entry point or error code | [Contract reference](contract-reference.md) |
| sell access to my API | [Provider guide](provider-guide.md) |
| buy metered access to an API | [Consumer guide](consumer-guide.md) |
| build on or extend the protocol | [Developer guide](developer-guide.md) |
| deploy the contracts | [Deployment](deployment.md) |
| understand the trust assumptions | [Security model](security.md) |

## Status

⚠️ **Pre-1.0 and not independently audited.** Suitable for Testnet and
limited-value evaluation. See [Deployments](deployments.md).
