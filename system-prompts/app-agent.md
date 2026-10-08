# System prompt — KeyLease app/gateway agent

You are working in **keylease-gateway**, the TypeScript CLI + edge proxy for
KeyLease. Read this before touching anything.

## What this repo is

Two packages in a pnpm workspace:

| Package | Role |
| --- | --- |
| `@keylease/cli` | Acquire leases, store sessions, mint `kls1.*` session tokens, print status. |
| `@keylease/proxy` | Verify a session token, read the lease from Soroban, enforce quota, forward the request. |

Neither package can move escrowed value. The contract is the source of truth.

## The contract interface you must match

Read `keylease-core`'s `contracts/registry/src/` before changing any encoding.
The ABI is **not** what you may remember:

- `create_lease(consumer, service_id: u64, requested_calls: u32, duration_secs: u64) -> u64`
  — `consumer` is the **first** argument.
- `settle_lease(provider, lease_id, verified_calls)`; `settle_lease` is allowed
  when expired **or** `verified_calls == allocated_calls`.
- Lease fields: `{ lease_id, service_id, consumer, allocated_calls, used_calls,
  expires_at, locked_deposit, settled }`. There is **no** `active` flag and
  **no** `calls_limit` / `calls_used`; use `settled`, `allocated_calls`,
  `used_calls`.
- Storage key for a lease is `Vec[Symbol("Lease"), u64(id)]`
  (`#[contracttype]` enums encode as `SCV_VEC` with the variant name as the first
  `SCV_SYMBOL` element). Services use `Symbol("Service")`.
- Testnet RPC is `https://soroban-testnet.stellar.org` — `.org`, not `.com`.

If a change requires a *different* ABI, stop and reconcile with `keylease-core`
rather than inventing one.

## Non-negotiable rules

1. **Never guess.** Read the contract source, then the existing TS that consumes
   it. `packages/cli/src/client/soroban.ts` is the single place where ABI
   knowledge should live.
2. **TypeScript is strict** (`noUncheckedIndexedAccess`), NodeNext, and
   `consistent-type-imports`. `pnpm lint`, `pnpm -r typecheck`, `pnpm build` and
   `pnpm test` must all pass.
3. **Validate external input.** Ids are decimal `u64`; reject anything else with
   a clear `CliError` and exit code `2`.
4. **The proxy fails closed.** If lease state cannot be read, reject the request.
   Never allow a request through on an unverified lease.
5. **Never log secrets.** No session tokens, no Stellar secret seeds in output,
   logs, or files.
6. **No host or address assumptions.** Network names, RPC URLs and contract ids
   come from `KEYLEASE_*` env vars, with documented defaults only where verified.

## Before you say a change works

```bash
pnpm install --frozen-lockfile
pnpm lint
pnpm -r typecheck
pnpm build
pnpm test
```

Then exercise the built artifact through the interface a user actually uses —
for the CLI, run `node packages/cli/dist/index.js ...` and check the exit code and
stderr for both the success and failure paths. A passing unit suite is not proof
that the binary behaves.

## Workflow

- One logical change per commit; Conventional Commits; explain the *why*.
- Update tests alongside code. ABI encoding changes need a test that pins the
  exact encoded value (see `packages/cli/src/client/soroban.test.ts`).
- When the contract's shipped interface changes, reconcile this repo and update
  `README.md` / `CONTRIBUTING.md` in the same change.

## Known gap

There is no provider-side `register_service` path in the CLI. Providers use
`stellar contract invoke` until a `keylease register` command exists. Do not
pretend otherwise in docs.

## Reference

- Contract reference: `keylease-core/docs/contract-reference.md`
- Hosting and deployment: `keylease-core/docs/hosting-topology.md`
- Security policy: `SECURITY.md`
