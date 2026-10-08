# Developer guide

How to build, test, extend and integrate KeyLease.

## Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | `1.84+` | `rustup install stable` |
| Wasm target | `wasm32v1-none` | `rustup target add wasm32v1-none` |
| components | `rustfmt`, `clippy` | `rustup component add rustfmt clippy` |
| Stellar CLI | latest | only needed to deploy or invoke on a network |
| Node.js | `>= 20.11` | only for `keylease-gateway` |

## Setup

```bash
git clone https://github.com/KeyLease-Security/keylease-core
cd keylease-core
rustup target add wasm32v1-none

cargo build --all
cargo test --all
```

## The commands CI runs

These four are the definition of "green". Run all four before opening a PR.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo build --target wasm32v1-none --release --all
```

CI job names are `Format`, `Clippy`, `Test` and `Build (wasm32v1-none)` — these are
the required status checks in branch protection.

## Project layout

| Path | Purpose |
| --- | --- |
| `contracts/registry/src/lib.rs` | Entry points and protocol lifecycle. |
| `contracts/registry/src/types.rs` | `Service`, `Lease`, `LeaseStatus`. |
| `contracts/registry/src/storage.rs` | `DataKey` layout, TTL policy, accessors. |
| `contracts/registry/src/errors.rs` | Stable `Error` codes (never reuse a number). |
| `contracts/registry/src/test.rs` | Integration tests for the registry. |
| `contracts/mock_token/` | Test-only SEP-41 token. |
| `scripts/deploy-testnet.sh` | Sequential testnet deployment. |
| `scripts/create-issues.sh` | Batch Wave backlog creation. |
| `scripts/setup-repo.sh` | Topics, labels, branch protection. |

## Environment variables

See [`.env.example`](../.env.example) for the full list.

| Variable | Used by | Meaning |
| --- | --- | --- |
| `KEYLEASE_NETWORK` | CLI, proxy, scripts | `testnet` / `mainnet` / `futurenet` / `local` |
| `KEYLEASE_RPC_URL` | CLI, proxy | Soroban RPC endpoint |
| `KEYLEASE_NETWORK_PASSPHRASE` | CLI, proxy | passphrase used to sign transactions |
| `KEYLEASE_CONTRACT_ID` | CLI, proxy | deployed `KeyLeaseRegistry` id |
| `KEYLEASE_SERVICE_ID` | CLI | service the CLI acquires from |
| `KEYLEASE_TOKEN_ADDRESS` | tooling | SEP-41 / SAC escrow token |
| `KEYLEASE_ADMIN` / `KEYLEASE_PROVIDER` / `KEYLEASE_CONSUMER` | tooling | public addresses |
| `KEYLEASE_RATE_PER_CALL` | `register_service` | stroops per call |
| `KEYLEASE_ENDPOINT_HASH` | `register_service` | 64 hex chars (32 bytes) |
| `KEYLEASE_SESSION_SECRET` | proxy | signs `kls1.*` session tokens |
| `KEYLEASE_UPSTREAM_URL` | proxy | upstream API to forward to |
| `KEYLEASE_PROXY_PORT` | proxy | listen port |

Never commit secret seeds or the session secret. `.env` is gitignored.

## Adding a new entry point

1. Add the variant(s) to `errors.rs` (**append**, never renumber).
2. Add any new `DataKey` variants to `storage.rs` with a documented TTL policy.
3. Implement the entry point in `lib.rs`, documenting auth + preconditions.
4. Add tests covering the happy path **and** each failure/refund path.
5. Update the interface table in `README.md` and
   [`docs/contract-reference.md`](contract-reference.md).

## Code standards

- `#![no_std]` — no `std` in contract code.
- **All token math is checked**; map failures to `Error::Overflow`.
- **Authenticate actors** with `require_auth()` on the address you represent.
- **Guard money paths** with the `settled` check first.
- **No unbounded loops** over user-supplied collections.
- **Errors over panics.**
- **Never reuse an error code.**

## Testing conventions

Use `env.mock_all_auths()` for happy paths and `try_*` client methods to assert
specific errors:

```rust
assert_eq!(
    client.try_settle_lease(&provider, &lease_id, &11),
    Err(Ok(Error::InvalidCalls))
);
```

Every security-sensitive change needs an adversarial test — an attempt to steal,
strand, or double-spend.

## Interoperating: reading contract state yourself

If you are writing your own client (as `keylease-gateway` does), you must encode
storage keys exactly as Soroban does.

`#[contracttype]` enums encode as an `SCV_VEC` whose first element is an
`SCV_SYMBOL` with the variant name, followed by the variant's values. So:

| Logical key | Wire key |
| --- | --- |
| `DataKey::Admin` | `Symbol("Admin")` |
| `DataKey::Lease(7)` | `Vec[Symbol("Lease"), u64(7)]` |
| `DataKey::Service(3)` | `Vec[Symbol("Service"), u64(3)]` |

With `@stellar/stellar-sdk`:

```ts
import { xdr, nativeToScVal } from '@stellar/stellar-sdk';

// DataKey::Lease(leaseId)
xdr.ScVal.scvVec([
  xdr.ScVal.scvSymbol('Lease'),
  nativeToScVal(leaseId, { type: 'u64' }),
]);
```

`getContractData` returns a `scvMap` of the `Lease` struct fields. Decode it as
`{ lease_id, service_id, consumer, allocated_calls, used_calls, expires_at,
locked_deposit, settled }`.

Full details are in the [contract reference](contract-reference.md#storage-layout).

## Deploying

See [Deployment](deployment.md). `scripts/deploy-testnet.sh` runs the exact
sequence and prints the values the gateway needs.

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md). Drips Wave tasks are filed from the
templates in `.github/ISSUE_TEMPLATE/`, or in bulk with
`scripts/create-issues.sh`.

## Read next

- [Contract reference](contract-reference.md)
- [Security model](security.md)
- [Hosting topology](hosting-topology.md)
