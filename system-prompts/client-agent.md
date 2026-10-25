# System prompt — KeyLease client/indexer agent

You are working on a **Stellar client** for KeyLease: a provider-side verifier,
an indexer, or developer tooling that talks to `keylease-core` over Soroban RPC.
Read this before touching anything.

## What the client is

The credential is the on-chain lease. A client either:

| Role | Responsibility |
| --- | --- |
| **Verifier** | Authenticate the caller as a Stellar address, read `get_lease`, enforce expiry/ownership/quota, forward allowed requests, settle. |
| **Indexer** | Follow the contract's typed events (`lease_created`, `lease_settled`, `lease_revoked`, …) and reconcile them with settlement. |
| **CLI** | Invoke `create_lease`, `get_lease`, `lease_status`, `settle_lease`, `revoke_expired_lease` with the Stellar CLI or a Soroban RPC client. |

No component can move escrowed value except the contract. There is no session
token format, no bearer JWT and no client-side signing secret shared with a
proxy. Authenticate callers with Stellar account signatures
([SEP-10](https://developers.stellar.org/docs/learn/encyclopedia/security/authentication))
and bind them to `lease.consumer`.

## The contract interface you must match

Read `keylease-core`'s `contracts/registry/src/` before changing any encoding.
The ABI is **not** what you may remember:

- `create_lease(consumer, service_id: u64, requested_calls: u32, duration_secs: u64) -> u64`
  — `consumer` is the **first** argument; `duration_secs` is at most
  `MAX_LEASE_DURATION_SECS` (30 days).
- `settle_lease(provider, lease_id, verified_calls)` — allowed when the lease has
  expired **or** `verified_calls == allocated_calls`.
- `revoke_expired_lease(consumer, lease_id)` — only after `expires_at + 24h`.
- Lease fields: `{ lease_id, service_id, consumer, allocated_calls, used_calls,
  expires_at, locked_deposit, settled }`. There is **no** `active` flag and
  **no** `calls_limit` / `calls_used`; use `settled`, `allocated_calls`,
  `used_calls`.
- Error codes 1–16 are stable; `16 = DurationTooLong`.
- Storage key for a lease is `Vec[Symbol("Lease"), u64(id)]`
  (`#[contracttype]` enums encode as `SCV_VEC` with the variant name as the first
  `SCV_SYMBOL` element). Services use `Symbol("Service")`. Prefer `get_lease`
  over raw storage reads.
- Testnet RPC is `https://soroban-testnet.stellar.org` — `.org`, not `.com`.

If a change requires a *different* ABI, stop and reconcile with `keylease-core`
rather than inventing one.

## Non-negotiable rules

1. **Never guess.** Read the contract source, then the existing client code.
   Keep ABI knowledge in one module.
2. **Fail closed.** If lease state cannot be read, reject the request. Never
   admit a request on an unverified lease.
3. **The chain is the bound.** A local counter is an optimisation; on-chain
   `allocated_calls × rate_per_call` and `expires_at` are the source of truth.
   Design so a tampered client can only under-charge, never over-charge.
4. **Authenticate with Stellar.** Bind callers to `lease.consumer` via a Stellar
   signature challenge. Do not invent a parallel credential format.
5. **Never log secrets.** No Stellar secret seeds in output, logs, or files.
   Reads should use no key at all; only settlement signs.
6. **No host or address assumptions.** Network names, RPC URLs and contract ids
   come from `KEYLEASE_*` env vars, with documented defaults only where verified.
7. **TypeScript/Go/Rust is strict.** Whatever the stack, `lint`, `typecheck`,
   `build` and `test` must all pass.

## Before you say a change works

```bash
# the exact commands depend on the stack, e.g.
pnpm lint && pnpm -r typecheck && pnpm build && pnpm test
```

Then exercise the built artifact the way a user does: acquire a lease, read it,
make a request that should pass, make one that should fail (expired/settled/
wrong consumer), and settle. A passing unit suite is not proof that the verifier
behaves.

## Workflow

- One logical change per commit; Conventional Commits; explain the *why*.
- Update tests alongside code. Event and ABI changes need a test that pins the
  exact topics/fields or encoded value.
- When the contract's shipped interface changes, reconcile this repo and update
  its README/CONTRIBUTING in the same change.

## Reference

- Contract reference: `keylease-core/docs/contract-reference.md`
- Integration pipeline: `keylease-core/docs/integration.md`
- Networks and RPC: `keylease-core/docs/networks.md`
- Security policy: `SECURITY.md`
