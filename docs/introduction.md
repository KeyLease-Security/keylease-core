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

KeyLease turns metered access into a **lease**, and puts the lease on Stellar.

A consumer locks a SEP-41 micropayment in Soroban escrow and receives an
on-chain authorization for a fixed allotment of calls. The provider verifies the
lease against Stellar instead of trusting a password, settles once with a
verified call count, and is paid from escrow. Unused balance refunds
automatically.

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
   `expires_at`, read from ledger time. There is nothing to rotate and nothing
   to leak permanently.
2. **Nobody can strand your money.** If the provider never settles, the consumer
   reclaims the **entire** deposit 24 hours after expiry. That refund path is
   what makes escrow safe to use against a provider you have never met.

## Why this is a Stellar protocol

KeyLease is not an off-chain SaaS that happens to store a hash on a chain. The
ledger is the system of record and each part of the design maps to a Stellar
primitive:

| Requirement | Stellar primitive |
| --- | --- |
| Hold and move the payment | SEP-41 token; the native Stellar Asset Contract (SAC) wraps XLM |
| Prove who created or settled a lease | `Address` authorization; accounts (`G…`) and contracts (`C…`) both work, including Stellar multisig |
| Know when a lease expires | Ledger timestamp, not an operator's clock |
| Prove what happened | Typed Soroban contract events (`lease_created`, `lease_settled`, …) |
| Keep escrow claimable | Persistent-storage TTL, extended per lease for its term + grace window |
| Identify the deployed code | `contractmeta!` (name, version, description, repository) and `version()` |
| Fix a deployed bug | `upgrade(admin, new_wasm_hash)` through the Soroban deployer |

The consumer's proof of access is the lease itself. A provider reads it from
Soroban RPC (or follows the event stream) and binds the caller to the lease's
consumer address using Stellar signatures — no second credential format is
invented. See [Integration](integration.md).

## What KeyLease is not

- **Not a payment processor.** It moves a SEP-41 token between a consumer, a
  provider, and escrow. It does not touch fiat.
- **Not an HTTP service.** The contract cannot see your traffic. Quota checking
  and request forwarding are the provider's job; the on-chain lease bounds what
  any implementation may charge.
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

### Networks and testnet facts

RPC endpoints, network passphrases, native SAC addresses, explorer links and the
testnet reset schedule live in [Networks](networks.md). In short:

- Testnet RPC is `https://soroban-testnet.stellar.org`; passphrase
  `Test SDF Network ; September 2015`.
- Friendbot funds accounts and contracts with **10,000 fake XLM**.
- Testnet resets wipe all ledger entries, including the deployed contract, 2–4
  times a year at 17:00 UTC.

## Read next

- [Architecture](architecture.md) — the contract modules, storage and events.
- [How the protocol works](protocol.md) — the state machine, with worked numbers.
