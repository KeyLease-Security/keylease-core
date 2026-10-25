# Developer guide

How to build, test, upgrade and extend the KeyLease Soroban contracts.

## Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | `1.84+` | `rustup install stable` |
| Wasm target | `wasm32v1-none` | `rustup target add wasm32v1-none` |
| components | `rustfmt`, `clippy` | `rustup component add rustfmt clippy` |
| Stellar CLI | latest | only needed to deploy, invoke, read events or upgrade on a network |
| Node.js | `>= 20.11` | only if you build your own JS client against Soroban RPC |

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
| `contracts/registry/src/lib.rs` | Entry points, `contractmeta!`, upgrade, protocol lifecycle. |
| `contracts/registry/src/events.rs` | Typed `#[contractevent]` payloads. |
| `contracts/registry/src/types.rs` | `Service`, `Lease`, `LeaseStatus`. |
| `contracts/registry/src/storage.rs` | `DataKey` layout, TTL policy, accessors. |
| `contracts/registry/src/errors.rs` | Stable `Error` codes (never reuse a number). |
| `contracts/registry/src/test.rs` | Lifecycle, SAC, events, upgrade and re-entrancy tests. |
| `contracts/mock_token/` | Test/sandbox SEP-41 token. |
| `scripts/deploy-testnet.sh` | Sequential testnet deploy and upgrade. |
| `scripts/create-issues.sh` | Batch Wave backlog creation. |
| `scripts/setup-repo.sh` | Topics, labels, branch protection. |

## Environment variables

See [`.env.example`](https://github.com/KeyLease-Security/keylease-core/blob/main/.env.example)
for the full list.

| Variable | Used by | Meaning |
| --- | --- | --- |
| `KEYLEASE_NETWORK` | scripts, clients | `testnet` / `mainnet` / `futurenet` / `local` |
| `KEYLEASE_RPC_URL` | scripts, clients | Soroban RPC endpoint |
| `KEYLEASE_NETWORK_PASSPHRASE` | clients | passphrase used to sign transactions |
| `KEYLEASE_CONTRACT_ID` | scripts, clients | deployed `KeyLeaseRegistry` id |
| `KEYLEASE_SERVICE_ID` | clients | service a consumer acquires from |
| `KEYLEASE_TOKEN_ADDRESS` | tooling | SEP-41 / SAC escrow token |
| `KEYLEASE_ADMIN` / `KEYLEASE_PROVIDER` / `KEYLEASE_CONSUMER` | tooling | Stellar addresses |
| `KEYLEASE_RATE_PER_CALL` | `register_service` | stroops per call |
| `KEYLEASE_ENDPOINT_HASH` | `register_service` | 64 hex chars (32 bytes) |

Never commit secret seeds. `.env` is gitignored.

## Adding a new entry point

1. Add the variant(s) to `errors.rs` (**append**, never renumber).
2. Add any new `DataKey` variants to `storage.rs` with a documented TTL policy.
3. Implement the entry point in `lib.rs`, documenting auth + preconditions.
4. Add a typed event in `events.rs` and publish it after the state is persisted.
5. Add tests covering the happy path **and** each failure/refund path, plus the
   emitted event.
6. Update the interface and event tables in `README.md` and
   [`docs/contract-reference.md`](contract-reference.md).

## Code standards

- `#![no_std]` — no `std` in contract code; the target is `wasm32v1-none`.
- **All token math is checked**; map failures to `Error::Overflow`.
- **Authenticate actors** with `require_auth()` on the address you represent.
- **Follow checks-effects-interactions.** Persist `settled` (or the equivalent)
  before any cross-contract call.
- **Guard money paths** with the `settled` check first.
- **Bound storage.** Every persistent key needs a TTL policy and, if user input
  controls its lifetime, an upper bound.
- **No unbounded loops** over user-supplied collections.
- **Errors over panics.**
- **Never reuse an error code.**
- **Emit events** for every state transition; never put secrets in one.

## Testing conventions

Use `env.mock_all_auths()` for happy paths and `try_*` client methods to assert
specific errors:

```rust
assert_eq!(
    client.try_settle_lease(&provider, &lease_id, &11),
    Err(Ok(Error::InvalidCalls))
);
```

Event tests compare the emitted topics and payload with the event type generated
by `#[contractevent]`:

```rust
assert_single_event(
    &env,
    &registry_id,
    &LeaseSettled { lease_id, provider, verified_calls, payout, refund },
);
```

The suite also deploys the **real Stellar Asset Contract** as the escrow token
(`env.register_stellar_asset_contract_v2`), and installs a hostile SEP-41 token
that re-enters `settle_lease` to prove the double-settlement guard holds. Keep
both patterns when adding money paths.

Every security-sensitive change needs an adversarial test — an attempt to steal,
strand, or double-spend.

## Upgrading a deployment

```bash
stellar contract upload \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account admin --network testnet

stellar contract invoke --id $REGISTRY --source-account admin --network testnet \
  --send=yes -- upgrade --admin $ADMIN --new_wasm_hash <hash-from-upload>
```

Rules for upgrades:

- Never change the `DataKey` layout or the meaning of stored values.
- Never renumber or reuse an error code.
- Keep event names and payload fields compatible; add new events for new
  transitions.
- Bump the workspace version so `version()` and `contractmeta!` change.
- Settle or revoke in-flight leases before changing token semantics.

## Reading contract state and events

Clients should prefer the read views and the event stream over raw storage:

```bash
stellar contract invoke --id $REGISTRY --source-account keylease-admin \
  --network testnet -- get_lease --lease_id 1
stellar events --id $REGISTRY --network testnet --start-ledger 123456
```

If you must read storage directly, `#[contracttype]` enums encode as an
`SCV_VEC` whose first element is an `SCV_SYMBOL` with the variant name:

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
locked_deposit, settled }`. Full details are in the
[contract reference](contract-reference.md#storage-layout), and the supported
way to verify leases is described in [Integration](integration.md).

## Deploying

See [Deployment](deployment.md). `scripts/deploy-testnet.sh` runs the exact
sequence and prints the values a client or indexer needs.

## Contributing

See [CONTRIBUTING.md](https://github.com/KeyLease-Security/keylease-core/blob/main/CONTRIBUTING.md).
Drips Wave tasks are filed from the templates in `.github/ISSUE_TEMPLATE/`, or in
bulk with `scripts/create-issues.sh`.

## Read next

- [Contract reference](contract-reference.md)
- [Security model](security.md)
- [Integration](integration.md)
