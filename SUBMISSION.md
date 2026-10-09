# Drips Stellar Wave — repo application

Application to list **keylease-core** for the Stellar Wave Program.

## Repository

| Field | Value |
| --- | --- |
| Repo | `https://github.com/KeyLease-Security/keylease-core` |
| Sibling repo | `https://github.com/KeyLease-Security/keylease-gateway` |
| License | MIT |
| Language | Rust (100% of this repo) |
| Docs site | <https://keylease-security.github.io/keylease-core/> — MkDocs Material, built from `docs/`, covering both this repo and `keylease-gateway` |

## One-paragraph description

KeyLease lets API and RPC providers on Stellar sell **time-bound, ephemeral
micro-leases** instead of long-lived static credentials. A consumer locks a
micropayment in Soroban escrow and receives an on-chain authorization for a fixed
allotment of calls; the provider verifies that lease, settles once with a verified
call count, and is paid from escrow while the unused balance is refunded. If a
provider never settles, the consumer reclaims the **entire** deposit 24 hours
after expiry. This repo is the protocol — a `no_std` Soroban workspace containing
the `KeyLeaseRegistry` contract (services, leases, escrow, settlement) and a
test-only SEP-41 mock token — with the deployment tooling, Wave issue backlog and
documentation needed to run and extend it.

## The two-repo relationship

The project is deliberately split so each half is independently reviewable,
deployable and Wave-eligible:

| Repo | Role | Holds value |
| --- | --- | --- |
| **keylease-core** (this one) | The `KeyLeaseRegistry` Soroban contract: services, leases, escrow, settlement, refunds. Pure Rust workspace. | ✅ escrow |
| **keylease-gateway** | TypeScript `@keylease/cli` (acquire leases, mint session tokens) and `@keylease/proxy` (verify sessions, enforce quota, forward requests). | ❌ |

The contract is the source of truth and the only component that can move funds.
The gateway is replaceable: a provider can run their own proxy, verify leases
in-process, or ignore the gateway entirely. The protocol does not depend on it —
if the proxy is down, providers can still settle and consumers can still revoke.

Both repos share one interface, pinned in this repo's
[contract reference](docs/contract-reference.md): `create_lease(consumer,
service_id, requested_calls, duration_secs)`, storage key
`Vec[Symbol("Lease"), u64]`, and the `Lease` struct fields
`{ lease_id, service_id, consumer, allocated_calls, used_calls, expires_at,
locked_deposit, settled }`.

## Why this is distinct

Approved Stellar repos already cover P2P escrow for rentals and tourism
(SafeTrust, StellarRent), permissionless escrow infrastructure (Trustless Work),
crowdfunding (KindFi), RWA (Akkuea), freelance escrow (OFFER-HUB) and x402
payment execution (routedock, stellarmind). KeyLease occupies a different slot:
**metered API/RPC credential leasing** — selling a fixed number of calls that
expires on its own. It is infrastructure for providers, not a marketplace or a
payment rail, and no direct equivalent was found in the approved list.

## What a contributor works on

The repo is built to be contributed to:

- **Contract:** `contracts/registry/src/{lib,types,storage,errors,test}.rs`,
  fully documented, with a design-note requirement for high-effort tasks.
- **Tooling:** `scripts/` — sequential testnet deploy, batch issue creation,
  repo setup.
- **Docs:** `docs/` — protocol mechanics, contract reference, provider and
  consumer guides, deployment and hosting topology.
- **Issue templates:** `.github/ISSUE_TEMPLATE/wave_{trivial,medium,high}_issue.md`
  (100 / 150 / 200 pts), each with checkable acceptance criteria and the exact
  CI commands.

The current backlog is created with `scripts/create-issues.sh`, which opens
three trivial, three medium and three high issues directly from these templates.

## Planned issues

Created from the templates above; each carries its Wave point label and every
acceptance-criteria list ends with the real CI commands.

**Trivial (100 pts)**

- Enforce a Wasm size budget in CI.
- Add `cargo audit` / dependency advisory scanning to CI.
- Cover the read-only views (`remaining_calls`, `locked_balance`,
  `service_count`, `lease_count`) with unit tests.

**Medium (150 pts)**

- Emit structured events for the service and lease lifecycle.
- Let providers update a service's rate and endpoint commitment.
- Support provider key rotation by transferring a service.

**High (200 pts, `security`)**

- Harden the money paths against re-entrant SEP-41 tokens
  (checks-effects-interactions).
- Fuzz the escrow invariant: value can never be created or destroyed.
- Prove lease and service storage survive archival (TTL) under load.

The high issues require an approved design note (Problem / Invariants / Approach /
Threat model / Test plan) before implementation, per `CONTRIBUTING.md`.

## Repository hygiene

- CI (`.github/workflows/ci.yml`) — jobs `Format`, `Clippy`, `Test`,
  `Build (wasm32v1-none)`; these are the branch-protection required checks.
- `SECURITY.md` — private disclosure, in-scope/out-of-scope, audit status.
- `CONTRIBUTING.md` — code standards, entry-point checklist, test conventions.
- Issue templates and a PR template that demands a security self-review.
- Release notes and a deployments registry (`docs/deployments.md`).

## Demo and verification

| Artifact | Status |
| --- | --- |
| Testnet contract id | ⏳ not yet deployed — published in `docs/deployments.md` after `scripts/deploy-testnet.sh` runs |
| Live app / proxy URL | ⏳ not yet hosted — see `docs/hosting-topology.md` |
| Demo video | ⏳ to be recorded once the proxy is hosted |
| Docs site | ✅ `docs/` in this repo |

Verification a reviewer can run today:

```bash
git clone https://github.com/KeyLease-Security/keylease-core
cd keylease-core
cargo test --all
cargo build --target wasm32v1-none --release --all
```

## Contact

`security@keylease.dev` for security reports; GitHub
[@KeyLease-Security](https://github.com/KeyLease-Security) otherwise.
