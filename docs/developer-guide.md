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

These five are the definition of "green". Run all five before opening a PR.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32v1-none --release --all -- -D warnings
cargo test --all
cargo build --target wasm32v1-none --release --all
```

CI also runs a cargo-deny dependency policy check (`deny.toml`), a shellcheck
pass over every tracked shell script, and a ClamAV malware scan over the
checkout and the built Wasm artifacts. CI job names are `Format`, `Shellcheck`,
`Clippy`, `Test`, `Build (wasm32v1-none)`, `Dependency Policy` and
`Malware Scan` — these are the required status checks in branch protection.

The shell pass is not one of the four cargo commands, so run it separately:

```bash
git ls-files -z '*.sh' | xargs -0 shellcheck
```

It uses shellcheck's default severity, which includes style notes, and fails
outright when no tracked script matches.

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
| `scripts/check-no-std.sh` | Checks the release Wasm is built without `std` and crates forbid `unsafe`. |
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

## Releasing a version

A release is four things: a version bump, a notes page, a check run and an
annotated tag. Nothing in `.github/workflows/` publishes a crate, builds a
GitHub Release or cuts a tag — `ci.yml` only gates commits and `docs.yml` only
builds and publishes the site — so the steps below are the whole process.

### 1. Bump the version

`Cargo.toml` carries the only version this repository reports. Both crates
inherit it (`version.workspace = true`), so one edit covers the workspace — but
two more files have to follow it.

| File | What changes |
| --- | --- |
| `Cargo.toml` | `[workspace.package] version = "X.Y.Z"` |
| `contracts/registry/Cargo.toml` | the `version = "X.Y.Z"` pin on the `keylease-mock-token` dev-dependency |
| `Cargo.lock` | the `keylease-registry` and `keylease-mock-token` entries — regenerate with `cargo check --all` and commit the diff |

No other file carries a version string, because the version reaches the rest of
the system through the build:

- `contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"))` embeds the
  new value in the Wasm, so `stellar contract info meta` reports it.
- `version()` reads that same value, so the on-chain contract identifies itself
  after the upgrade — the check in [Deployment](deployment.md#verifying-a-deployment).

Use the convention the existing releases already follow. `0.x.0` is a release
that changes what the world sees — a new entry point, a new event, a new error
code, new storage. `0.x.1` is a release that changes none of those (v0.2.1 only
changed the lease TTL policy). Error codes are append-only, so a new code alone
is never a major-version event.

### 2. Write the notes page

Add `docs/releases/vX.Y.Z.md`, then register it in **both** tables of contents —
`mkdocs.yml` under `nav:` → `Project:` and `docs/SUMMARY.md` under
`## Project`. The newest page keeps the `Release notes` label and the previous
ones are relabelled to their version:

```yaml
  - Project:
      - Deployments: deployments.md
      - Release notes: releases/v0.2.2.md   # new
      - v0.2.1: releases/v0.2.1.md          # relabelled
      - v0.2.0: releases/v0.2.0.md
      - v0.1.0: releases/v0.1.0.md
```

Copy the shape from an existing page rather than inventing one:

| Example | What it shows |
| --- | --- |
| [v0.2.1](releases/v0.2.1.md) | a behavior-only patch: the problem it solved, a short "What changed", compatibility stated as "no ABI, event, storage-key or error-code changes" |
| [v0.2.0](releases/v0.2.0.md) | an additive release: new events and `upgrade`, a full "Interface at this tag" table, and an "Upgrading from v0.1.0" section |
| [v0.1.0](releases/v0.1.0.md) | the first release, and the banner pattern a superseded page uses to point at the current interface |

A notes page needs these sections, in this order:

1. **Heading and lead** — `# keylease-core vX.Y.Z` then one paragraph saying what
   the release is and, when there is a reason, why it exists.
2. **Highlights** or **What changed** — one bullet per user-visible change, each
   naming the entry point, constant or error code it touched.
3. **Contracts** — the package → Wasm table.
4. **Interface at this tag** — the entry points and their auth requirements, so
   the page stays readable after the reference moves on.
5. **Compatibility** — Rust, `soroban-sdk`, Wasm target, the error-code range, and
   an explicit statement about storage keys and events.
6. **Upgrading from vX.Y.Z** — required whenever a deployed network is running an
   older build; say whether `upgrade` is safe in place.
7. **Known limitations**, **Verification** (the four commands), **License**.

Never put a secret, seed or raw credential in a notes page — including a
deployer's secret key. Addresses and ledger numbers are fine and useful.

### 3. Run the checks a release is gated on

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32v1-none --release --all -- -D warnings
cargo test --all
cargo build --target wasm32v1-none --release --all
./scripts/check-no-std.sh --no-build
cargo deny check
./scripts/scan-malware.sh
pip install -r requirements-docs.txt && mkdocs build --strict
```

The first four are the CI jobs every commit must pass, and
`check-no-std.sh` runs inside the `Build (wasm32v1-none)` job; `cargo deny check` and
`./scripts/scan-malware.sh` are the Dependency Policy and Malware Scan jobs run
locally. `mkdocs build --strict` is the one that bites here, because the failure
it catches is a page missing from `nav:` or an anchor that does not resolve. It
is a pull-request gate as well as a `main` gate, but only for a PR that touches
`docs/**`, `mkdocs.yml` or `requirements-docs.txt` — those paths run the
`Docs → Build preview` job. A release branch that changes none of them, such as
one that only bumps the version, gets no preview run at all, so execute the
command above yourself before tagging.

### 4. Tag the release commit on `main`

Tag the merge-base commit on `main` that carries both the version bump and the
notes page — not a branch tip that has not merged. `v0.1.0` is an annotated tag,
so stay annotated:

```bash
git fetch origin
git tag -a vX.Y.Z -m "keylease-core vX.Y.Z" origin/main
git push origin vX.Y.Z
```

The tag is what makes the recorded hashes meaningful: it is the revision anyone
can rebuild from to reproduce the deployed Wasm.

### 5. Record the artifacts

Deploying is covered in [Deployment](deployment.md); what a release must
*record* is in [Deployments](deployments.md), whose
[Recording a deployment](deployments.md#recording-a-deployment) steps this table
feeds.

| Artifact | How to obtain it | Where it is recorded |
| --- | --- | --- |
| Contract id | `stellar contract deploy` prints it | Testnet table, `KeyLeaseRegistry` |
| Built Wasm digest | `sha256sum target/wasm32v1-none/release/keylease_registry.wasm` | "Verifying a listed address" |
| On-chain Wasm hash | `stellar contract upload` prints it; `stellar contract info hash --id $REGISTRY` reads it back | Testnet table, "Wasm hash" |
| Version, as the chain sees it | `… -- version` and `stellar contract info meta --id $REGISTRY` | Testnet table + "On-chain metadata" (`rssdkver` included) |
| Deploy-ledger trail | ledger and tx hash for `init`, `set_token`, `register_service`, or the `upgrade` | Testnet table rows |
| Pre/post counters | `service_count`, `lease_count`, `get_service`, `get_lease` | release notes "Verified on-chain" bullet |
| Signer addresses | admin, provider, demo consumer, escrow token | Testnet table |
| Source revision | the tag cut in step 4 | Testnet table, "Deployed from" |

The built digest and the on-chain hash are the same value only if the build is
reproducible: check out the tag, run
`cargo build --target wasm32v1-none --release --all` and compare against
`stellar contract info hash`. If they differ, the release is not verifiable from
source — say so in the notes page rather than recording a hash you cannot
reproduce.

## Upgrading a deployment

```bash
stellar contract upload \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account admin --network testnet

stellar contract invoke --id $REGISTRY --source-account admin --network testnet \
  --send=yes -- propose_upgrade --admin $ADMIN --new_wasm_hash <hash-from-upload>

# inspect the recorded hash and execution ledger timestamp
stellar contract invoke --id $REGISTRY --network testnet -- get_pending_upgrade

# after the 24h delay has elapsed (the contract rejects earlier calls)
stellar contract invoke --id $REGISTRY --source-account admin --network testnet \
  --send=yes -- execute_upgrade --admin $ADMIN

# or abandon the proposal: -- cancel_upgrade --admin $ADMIN
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
| `DataKey::CancelRequest(7)` | `Vec[Symbol("CancelRequest"), u64(7)]` |

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

## Local sandbox walkthrough

The unit suite proves the contract logic inside a simulated ledger. This runs the
same lifecycle against a real local network using the Wasm you just built, so you
can watch one lease move value end to end.

There is no `scripts/deploy-local.sh` — the sequence is below, and
[`scripts/deploy-testnet.sh`](https://github.com/KeyLease-Security/keylease-core/blob/main/scripts/deploy-testnet.sh)
is the Testnet reference for the identical steps. Everything here is
[`stellar 28.1.0`](https://developers.stellar.org/docs/tools/cli/stellar-cli)
syntax; if your CLI is a different version, re-read the flag with
`stellar <command> --help` rather than assuming.

### Prerequisites

| Requirement | Check |
| --- | --- |
| A running container engine (Docker, Podman or Colima). | `docker info` answers without error. |
| The registry Wasm built. | `cargo build --target wasm32v1-none --release --all` |
| Port 8000 free on localhost. | `stellar container start local` maps `8000:8000` by default; pass `-p HOST_PORT:CONTAINER_PORT` to move it. |

### 1. Start a local network

```bash
stellar container start local
```

This runs the `stellar/quickstart` image with the node, Soroban RPC, Horizon
**and a friendbot** in one container. The command has no detached mode — note that
`-d` is `--docker-host`, not Docker's detach flag — so run it in its own terminal
and leave it up for the rest of the walkthrough.

`local` is a built-in CLI network — `stellar network ls` already lists it, so you
do not need `stellar network add`. It resolves to:

| | |
| --- | --- |
| Network passphrase | `Standalone Network ; February 2017` |
| Soroban RPC | `http://localhost:8000/rpc` |
| Horizon | `http://localhost:8000` |
| Friendbot | `http://localhost:8000/friendbot?addr=<ACCOUNT>` |

You can confirm the RPC is up before going further:

```bash
stellar network health --network local
stellar ledger latest --network local
```

### 2. Create and fund three identities

Three separate accounts, not one: `settle_lease` checks
`service.provider == provider` and `revoke_expired_lease` checks the lease owner.
A single account for every role hides exactly the authorisation bugs a sandbox is
for.

```bash
for who in kl-admin kl-provider kl-consumer; do
  stellar keys generate "$who" --network local --fund
done
```

`--fund` works on `local` too — the CLI posts to the container's own friendbot,
the same code path as Testnet. Read a balance back to be sure the account exists:

```bash
stellar token balance --id native --account "$(stellar keys address kl-consumer)" --network local
```

### 3. Deploy the registry

```bash
cargo build --target wasm32v1-none --release --all

REGISTRY=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account kl-admin --network local | tail -n1)
```

### 4. Initialise, set the escrow token, publish a service

Each step reads what the previous one wrote, which is why the order is fixed.

```bash
stellar contract invoke --id "$REGISTRY" --source-account kl-admin \
  --network local --send=yes -- init --admin "$(stellar keys address kl-admin)"

TOKEN=$(stellar contract id asset --network local --asset native)
stellar contract invoke --id "$REGISTRY" --source-account kl-admin \
  --network local --send=yes -- set_token \
     --admin "$(stellar keys address kl-admin)" --token "$TOKEN"

ENDPOINT_HASH=$(printf '%s' "$(stellar keys address kl-provider):local-demo" \
  | shasum -a 256 | awk '{print $1}')

SERVICE_ID=$(stellar contract invoke --id "$REGISTRY" --source-account kl-provider \
  --network local --send=yes -- register_service \
     --provider "$(stellar keys address kl-provider)" \
     --rate_per_call 1000000 --endpoint_hash "$ENDPOINT_HASH" | tail -n1)
```

`rate_per_call` is in stroops, so `1000000` is 0.1 XLM per call.

The native SAC address is derived from the network passphrase, not assigned. On
the standalone passphrase it is
`CDMLFMKMMD7MWZP3FKUBZPVHTUEDLSX4BYGYKH4GCESXYHS3IHQ4EIG4` — re-derive it with the
command above rather than copying that value, because your own `stellar network add`
passphrase would give a different address.

### 5. Create one lease

100 calls at 0.1 XLM escrows 10 XLM:

```bash
LEASE_ID=$(stellar contract invoke --id "$REGISTRY" --source-account kl-consumer \
  --network local --send=yes -- create_lease \
     --consumer "$(stellar keys address kl-consumer)" \
     --service_id "$SERVICE_ID" \
     --requested_calls 100 \
     --duration_secs 120 | tail -n1)

stellar contract invoke --id "$REGISTRY" --source-account kl-consumer \
  --network local -- get_lease --lease_id "$LEASE_ID"
stellar contract invoke --id "$REGISTRY" --source-account kl-consumer \
  --network local -- remaining_calls --lease_id "$LEASE_ID"
```

Do not `approve` an allowance first — there is none to grant. `create_lease`
calls `transfer(consumer, contract, amount)` itself under the consumer's own auth,
so the consumer needs a balance, not a permission.

`duration_secs` is a wall-clock number, and the ledger still advances in real
time. 120 seconds is enough to settle an expired lease without leaving the
terminal; a day-long lease is not a sandbox demo.

### 6. Settle it

`settle_lease` runs when the lease has **expired**, or when the provider reports
full consumption — whichever comes first. Reporting everything immediately is the
fastest path through the happy case:

```bash
stellar contract invoke --id "$REGISTRY" --source-account kl-provider \
  --network local --send=yes -- settle_lease \
     --provider "$(stellar keys address kl-provider)" \
     --lease_id "$LEASE_ID" --verified_calls 100
```

Payout and refund must sum to the deposit, so 100 verified of 100 allocated pays
the provider 10 XLM and refunds 0. Create a second lease and settle it
*partially* before it expires to see the guard refuse:

```bash
OTHER=$(stellar contract invoke --id "$REGISTRY" --source-account kl-consumer \
  --network local --send=yes -- create_lease \
     --consumer "$(stellar keys address kl-consumer)" \
     --service_id "$SERVICE_ID" --requested_calls 100 --duration_secs 120 | tail -n1)

stellar contract invoke --id "$REGISTRY" --source-account kl-provider \
  --network local --send=yes -- settle_lease \
     --provider "$(stellar keys address kl-provider)" \
     --lease_id "$OTHER" --verified_calls 30
# Error(LeaseNotSettleable) — the lease is live and under-consumed
```

Wait past `duration_secs` and the same call succeeds, paying 3 XLM and refunding
7 XLM to the consumer. Verify it with the read views and the token itself:

```bash
stellar contract invoke --id "$REGISTRY" --source-account kl-admin --network local \
  -- locked_balance --lease_id "$OTHER"      # 0 once settled
stellar token balance --id native --account "$(stellar keys address kl-provider)" \
  --network local
```

### 7. Read the events

Note the ledger sequence before you deploy, then read the whole lifecycle out of
the chain afterwards:

```bash
stellar ledger latest --network local          # write down the sequence
# ... run steps 3 to 6 ...
stellar events --id "$REGISTRY" --network local --start-ledger <that sequence> -c 50
```

Expect `RegistryInitialized`, `EscrowTokenSet`, `ServiceRegistered`,
`LeaseCreated`, `LeaseSettled`. Use `-c` generously: the default is 10 events,
and a full lifecycle plus the token transfers is more than that. Event payloads
and their fields are in the [contract reference](contract-reference.md).

### 8. Keep an entry alive

A local network is the only place this costs nothing to try. Reads never extend a
storage entry, so an entry that is looked at but not written ages toward
eviction until somebody bumps it explicitly.

```bash
# Both are permissionless — any account can pay for any entry's rent.
stellar contract invoke --id "$REGISTRY" --source-account kl-provider --network local \
  -- bump_service --service_id "$SERVICE_ID"
```

A successful bump is silent: no event, no field changes, and the only effect is a
later `liveUntilLedgerSeq` plus the rent charged into the caller's fee. Lease
entries need a lease that can still be acted on, because `bump_lease` refuses a
settled one — so create a third lease and leave it unsettled:

```bash
LIVE=$(stellar contract invoke --id "$REGISTRY" --source-account kl-consumer \
  --network local --send=yes -- create_lease \
     --consumer "$(stellar keys address kl-consumer)" \
     --service_id "$SERVICE_ID" --requested_calls 10 --duration_secs 3600 | tail -n1)

stellar contract invoke --id "$REGISTRY" --source-account kl-consumer --network local \
  -- bump_lease --lease_id "$LIVE"
```

Then try the two refusals, because they are the reason these calls are safe to
leave open to anyone. `$LEASE_ID` was settled in step 6, so its funds have already
moved and extending it would only grow state:

```bash
stellar contract invoke --id "$REGISTRY" --source-account kl-provider --network local \
  -- bump_lease --lease_id "$LEASE_ID"         # Error(LeaseAlreadySettled)

# A miss is an error, so nobody pays rent for an entry that is not there.
stellar contract invoke --id "$REGISTRY" --source-account kl-provider --network local \
  -- bump_service --service_id 404             # Error(ServiceNotFound)
```

`bump_lease` takes only a `lease_id`: the horizon comes from the lease's own
`expires_at` and the contract's ceiling, never from the caller. The policy, the
per-lease arithmetic and what eviction costs a consumer are in
[Storage classes and TTL policies](contract-reference.md#storage-classes-and-ttl-policies).

### What differs from Testnet

Only the network is different; the contract, its ABI and the step order are not.

| | Testnet | Local sandbox |
| --- | --- | --- |
| Network flag | `--network testnet` | `--network local` |
| Who operates the network | SDF | you, in a container you start |
| RPC | `https://soroban-testnet.stellar.org` | `http://localhost:8000/rpc` |
| Friendbot | `https://friendbot.stellar.org?addr=…` | `http://localhost:8000/friendbot?addr=…` |
| Funding an identity | `stellar keys generate x --network testnet --fund` | `stellar keys generate x --network local --fund` — same flag |
| Native SAC address | derived from `Test SDF Network ; September 2015` | derived from `Standalone Network ; February 2017`; different address |
| Deploy script | `./scripts/deploy-testnet.sh` | no script; the sequence above |
| Contract id lifetime | until the next Testnet reset | until you stop the container |
| Wiping the ledger | 2–4× a year, on SDF's schedule | whenever you like: `stellar container stop local` |
| Value at risk | fake XLM | none |

The commands that are *identical* are worth stating, because contributors expect
to rewrite them: `stellar contract deploy`, `upload`, `invoke`,
`contract id asset` and the `--send=yes` form all take the same arguments with a
different `--network`. Nothing in `contracts/` is network-aware.

### Teardown

```bash
stellar container stop local

# optional: the identities are only seeds on your machine
for who in kl-admin kl-provider kl-consumer; do
  stellar keys rm --force "$who"
done
```

`stellar keys rm` takes one name at a time and asks for confirmation unless you
pass `--force`.

`stellar container start` uses `--rm`, so stopping the container discards the
ledger. Starting it again gives you an empty network with the same passphrase —
which means the redeploy you must now do is the same one described in
[After a Testnet reset](networks.md#after-a-testnet-reset), including
`register_service` returning `1` again because `ServiceCount` lives in instance
storage.

### Using `contracts/mock_token` instead of the native SAC

The mock token exists so a sandbox does not depend on lumens at all. It has an
**unlimited mint** and must never be configured on a network that holds value —
see [contract reference](contract-reference.md).

```bash
MOCK=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/keylease_mock_token.wasm \
  --source-account kl-admin --network local | tail -n1)

stellar contract invoke --id "$MOCK" --source-account kl-admin --network local \
  --send=yes -- init --admin "$(stellar keys address kl-admin)"

# Mint 1000 tokens (7 decimals) to the consumer, then point the registry at it.
stellar contract invoke --id "$MOCK" --source-account kl-admin --network local \
  --send=yes -- mint --to "$(stellar keys address kl-consumer)" --amount 10000000000

stellar contract invoke --id "$REGISTRY" --source-account kl-admin --network local \
  --send=yes -- set_token --admin "$(stellar keys address kl-admin)" --token "$MOCK"
```

From here the lease steps are unchanged: every money path goes through the token
address that `set_token` stored, so swapping the asset changes nothing else. That
indirection is also how you exercise a misbehaving token by hand — point the
registry at a contract that reverts or re-enters on `transfer`. The automated
version of exactly that attack is
`test_reentrant_token_cannot_settle_a_lease_twice` in
`contracts/registry/src/test.rs`.

## Contributing

See [CONTRIBUTING.md](https://github.com/KeyLease-Security/keylease-core/blob/main/CONTRIBUTING.md).
Drips Wave tasks are filed from the templates in `.github/ISSUE_TEMPLATE/`, or in
bulk with `scripts/create-issues.sh`.

## Read next

- [Contract reference](contract-reference.md)
- [Security model](security.md)
- [Integration](integration.md)
