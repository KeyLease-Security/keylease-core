# Introduction

## The problem

Every API and RPC provider on Stellar hands out the same artifact: a **static
API key**. That credential is a liability on both sides of the relationship.

For **developers**, it is a secret that lives forever in a `.env` file. It leaks
through committed repositories, CI logs, screenshots and shared machines, and
because it never expires, a leak is a permanent leak. Rotating it breaks every
consumer at once.

For **providers**, a static key means an all-or-nothing relationship. You cannot
sell 1,000 calls to someone; you either grant unlimited access, or you build and
operate your own billing, quota and revocation infrastructure. Usage cannot be
metered on-chain, so nobody can prove what was consumed.

## The idea

KeyLease turns metered access into a **lease**.

A consumer locks a micropayment in Soroban escrow and receives a lease: a
time-bound, on-chain authorization for a fixed allotment of calls. The provider
verifies the lease instead of a password, settles once with a verified call
count, and is paid from escrow. Unused balance refunds automatically.

```
consumer                         registry                        provider
   |                                |                                |
   |-- create_lease(calls, secs) -->|                                |
   |   (escrow = rate * calls)      |                                |
   |<-- lease_id ------------------ |                                |
   |                                |                                |
   |                                |<-- settle_lease(used_calls) ---|
   |<-- refund of unused ---------- |---- payout of used ----------->|
   |                                |                                |
```

Two properties fall out of this design that a static key cannot offer:

1. **The credential expires on its own.** Authorization is bounded by
   `expires_at`; there is nothing to rotate and nothing to leak permanently.
2. **Nobody can strand your money.** If the provider never settles, the consumer
   reclaims the **entire** deposit 24 hours after expiry. That refund path is
   what makes escrow safe to use against a provider you have never met.

## What KeyLease is not

- **Not a payment processor.** It moves a SEP-41 token between a consumer, a
  provider, and escrow. It does not touch fiat.
- **Not an API gateway.** The on-chain contract cannot see your HTTP traffic.
  Off-chain enforcement (quota checking, request forwarding) lives in
  [keylease-gateway](https://github.com/KeyLease-Security/keylease-gateway).
- **Not a credential store.** Providers publish `endpoint_hash`, a commitment to
  their off-chain endpoint descriptor. No secret is ever written on-chain.

## Where it fits in the Stellar ecosystem

Stellar's Soroban runtime makes this practical: sub-cent fees, ~5 second ledger
close, and native SEP-41 tokens mean a lease can be created, settled and
refunded cheaply enough to be worth doing for small allotments of calls.

KeyLease targets a specific gap: **metered API/RPC credential leasing**. Adjacent
Stellar escrow projects focus on milestone-based work and P2P rental escrow.
Metered access — a fixed number of calls that expires — is the white space this
protocol occupies.

### Testnet facts you'll need

All figures below are from the live Stellar documentation.

| Fact | Value |
| --- | --- |
| Testnet RPC (SDF-hosted) | `https://soroban-testnet.stellar.org` |
| Testnet passphrase | `Test SDF Network ; September 2015` |
| Friendbot faucet | `https://friendbot.stellar.org` — funds accounts and contracts with **10,000 fake XLM** |
| Mainnet RPC | third-party only (e.g. `https://mainnet.sorobanrpc.com`); SDF does not host one |
| Testnet resets | **2–4 times per year at 17:00 UTC**; the next scheduled 2026 reset is **December 16, 2026** |

> **Testnet resets wipe all ledger entries.** Re-run
> [`scripts/deploy-testnet.sh`](../scripts/deploy-testnet.sh) after a reset —
> your contract id will change.

## Read next

- [Architecture](architecture.md) — the components and how data flows.
- [How the protocol works](protocol.md) — the state machine, with worked numbers.
