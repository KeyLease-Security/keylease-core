# Changes since the submission — Drips Wave appeal

**Repository:** `KeyLease-Security/keylease-core`
**Submission state:** `01581ef` (2026-10-09, the tree as reviewed)
**Current state:** `main` (2026-10-25) — everything below is the single
commit `03dd6fe`, plus this file.
**Scope of this document:** everything that changed in this repository after the
submission was rejected, with pointers so every claim can be verified
independently.

---

## TL;DR

Since the submission, the project moved from "code + docs, unproven on a live
network" to:

1. **A contract rebase onto Soroban primitives** — typed `#[contractevent]`
   events for every lifecycle transition, `contractmeta!` on-chain metadata, an
   admin-only `upgrade` path, checks-effects-interactions on the money paths, and
   a term-aware storage-TTL policy.
2. **A real Testnet deployment** — contract
   `CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62` running
   `version() == 0.2.1`, with two full lease lifecycles created, settled,
   refunded and event-decoded on-chain, and a Wasm hash reproducible from this
   tree.
3. **A bug the deployment exposed, fixed and released (v0.2.1)** — the original
   storage-TTL policy rented ~30 days for every lease, making a 10 XLM lease cost
   ~20 XLM in rent. The policy now rents only the lease's remaining term plus
   grace.
4. **40 passing tests, green CI, and restructured documentation** including a
   `deployments.md` that records every transaction hash of the live deployment.

One commit carries all of it: `03dd6fe` — **41 files changed, 2,211
insertions, 1,304 deletions.**

---

## Timeline

| Date | Event |
| --- | --- |
| 2026-10-07 / 08 | Initial implementation and `Prepare keylease-core for Drips Wave review` (`a6e1d89`); Wave issues #1–#9 filed. |
| 2026-10-09 | Submission state (`01581ef`). Docs published to GitHub Pages. |
| — | **Submission rejected (no itemised feedback).** |
| 2026-10-25 | `03dd6fe` — contract rebase, Testnet deployment, v0.2.1 release, docs restructure. CI and Docs workflows green on push to `main`. |

---

## 1. Contract changes

All in `contracts/registry/src/`; the public entry points, storage keys and
error codes 1–15 are unchanged (additive only), so the change is reviewable
against the submitted ABI without re-learning the protocol.

### New: typed Soroban events (`events.rs`, new file, 121 lines)

Every state transition now emits a `#[contractevent]`, with `service_id` /
`lease_id` carried in the topics so the lifecycle is indexable from the ledger
without polling storage:

`registry_initialized`, `escrow_token_set`, `service_registered`,
`service_status_changed`, `lease_created`, `lease_settled`, `lease_revoked`,
`contract_upgraded`.

This closes the gap the submitted version had: it was impossible to reconstruct
protocol activity from ledger data alone.

### On-chain metadata + versioning (`lib.rs`, +163 lines)

- `contractmeta!` embeds name, version, description and repository in the Wasm,
  so `stellar contract info meta` identifies the running build without calling
  the contract.
- `version()` returns `0.2.1`; the release docs pin it.

### Upgrade path

`upgrade(admin, new_wasm_hash)` installs a new Wasm through the Soroban
deployer, preserving contract id, storage and balances, and emits
`contract_upgraded`. Admin-authorised only.

### Hardening of the money paths

- **Checks-effects-interactions.** `settle_lease` and `revoke_expired_lease`
  persist `settled = true` *before* any token call.
- **Re-entrancy test.** The suite installs a malicious SEP-41 token that calls
  back into `settle_lease` and asserts the second attempt is rejected
  (`LeaseAlreadySettled`).
- **Duration bound.** `create_lease` rejects `duration_secs` above 30 days with
  the new `DurationTooLong` (16), so escrow can never outlive its ledger entry.

### Storage TTL policy (`storage.rs`, +42 lines) — the v0.2.1 fix

The submitted/initial build extended **every** lease to a fixed ~30-day horizon.
Deploying it made the economics fail: on Testnet, creating a 100-call, one-hour
lease cost ~20 XLM of rent against a 10 XLM deposit. `save_lease` and `get_lease`
now extend to *remaining term + the provider's 24 h grace + a one-day read
buffer*. Verified on-chain after redeploy (see §2). The host's minimum
persistent TTL (~7 days on Testnet) still applies and is documented rather than
hidden.

### Real SAC coverage in tests (`test.rs`, +360 lines)

The suite now escrows through `register_stellar_asset_contract_v2` — the same
Stellar Asset Contract that wraps native XLM — in addition to the mock token.

---

## 2. Live Testnet deployment (new evidence)

Full record with every hash, ledger and fee: [`docs/deployments.md`](docs/deployments.md).

| Field | Value |
| --- | --- |
| Contract | `CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62` |
| `version()` | `0.2.1` |
| Wasm sha256 | `5362d4cb866a0405f17d1adfe8179e63afaca8a7dc741df3c3b87ee0ab75146a` — byte-identical to a local `cargo build --target wasm32v1-none --release` |
| Escrow token | native SAC `CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC` |
| Admin / provider | `GBDHDKBXURVBLAJNGBSAH4AHXRI2R3GFCEU5PCP6LI34SCETSBBLMUUT` |
| Explorer | <https://stellar.expert/explorer/testnet/contract/CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62> |

What was exercised against the live contract:

- `init` → `set_token` → `register_service`, each with its transaction hash and
  ledger recorded.
- **Lease 1** (100 calls, 1 h): created, settled 100/100, payout
  `100000000` stroops, refund `0`.
- **Lease 2** (10 calls, 60 s): created, settled 4/10 after expiry, payout
  `4000000`, refund `6000000` through the SAC.
- `lease_created` / `lease_settled` events decoded from the ledger with
  `stellar events --start-ledger 5382788`.
- A 31-day lease correctly rejected with `Error(Contract, #16)`
  (`DurationTooLong`).
- `contractmeta` on the deployed Wasm reports `KeyLeaseRegistry / 0.2.1`.

Deployment is reproducible: `scripts/deploy-testnet.sh` performs the whole
sequence, and the recorded Wasm hash can be re-derived from this tree.

---

## 3. Verification

Run from the repository root:

```bash
cargo fmt --all -- --check        # passes
cargo clippy --all-targets -- -D warnings   # passes, no warnings
cargo test --all                  # 40 passed, 0 failed (0.8 s)
cargo build --target wasm32v1-none --release --all   # passes
python3 -m mkdocs build --strict  # docs build with no warnings
```

CI on push to `main` (`.github/workflows/ci.yml`, four jobs: Format, Clippy,
Test, Build) is green for `03dd6fe` — run `40201944080`, success; the Docs
workflow run `40201943995`, success.

---

## 4. Documentation

- **`docs/deployments.md`** (new): the live-address record, on-chain metadata,
  full lifecycle evidence, deployment economics, and a "verify this address
  yourself" section (version, admin, token, event replay, reproducible Wasm
  hash).
- **`docs/releases/v0.2.0.md`, `docs/releases/v0.2.1.md`** (new): release notes
  with explicit compatibility/upgrade sections; `v0.1.0` corrected.
- **`docs/networks.md`, `docs/integration.md`** (new): network/passphrase/RPC
  table, the native-SAC derivation, and a concrete fail-closed verification
  pipeline (SEP-10 → `get_lease` → ownership → quota → settle). The gateway
  pages were consolidated into this provider-facing guidance so the docs describe
  one supported integration path instead of an unbuilt one.
- **Rewritten in place:** `README.md`, `architecture`, `protocol`,
  `contract-reference`, `security`, `developer/provider/consumer guides`,
  `deployment`, `introduction`, `index`.
- Docs are published with MkDocs (`mkdocs.yml`, `docs/SUMMARY.md`) via the Docs
  workflow to GitHub Pages.

---

## 5. Tooling

- **`scripts/deploy-testnet.sh`**: idempotent deploy + init + `set_token` +
  service registration, printing every id, ledger and hash needed for
  `deployments.md`; safe to re-run.
- **`scripts/create-issues.sh` / `scripts/setup-repo.sh`**: fixed label
  argument-splitting and made each section independent so a permission failure
  is reported instead of aborting.
- CI: fmt, clippy `-D warnings`, tests and the `wasm32v1-none` release build are
  all required checks.

---

## 6. What is *not* claimed

Stated plainly so the record is honest:

- **Mainnet is not deployed** — intentionally, until the contract is audited.
- **Not audited.** Pre-1.0; `docs/security.md` and `SECURITY.md` carry the
  threat model and disclosure process.
- **The admin key used for the Testnet deployment is a local CLI identity.**
  It must be moved to a cold or multisig account before any value-bearing use.
- **Nine Wave issues remain open** (#1–#9): Wasm size budget, cargo-audit,
  read-only view tests, structured service events, rate updates, provider key
  rotation, re-entrancy fuzzing, escrow-invariant fuzzing, TTL archival tests.
  Some are partially addressed by the v0.2.0/v0.2.1 work (events, TTL
  behaviour, SAC coverage); they are tracked rather than closed silently.
- **Known limitations** are enumerated in `docs/releases/v0.2.0.md` (upgrade
  trust, provider-supplied call counts, no key rotation).

---

## Reproducing the whole record

```bash
git log --oneline            # submission → rejection → 03dd6fe
git show --stat 03dd6fe      # the full diff described above
cargo test --all             # 40 tests
stellar events --id CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62 \
  --network testnet --start-ledger 5382788   # the on-chain history
```
