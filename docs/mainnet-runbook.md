# Mainnet deployment and key-custody runbook

> **Status: intentionally deferred.** Do not deploy or execute a mainnet
> transaction until the maintainer-approved gate below is complete.

## Design approval gate

Implementation and deployment are separate decisions. Maintainer approval must
record all of the following in the tracking issue or PR:

- approved architecture and threat model;
- completed independent security audit;
- remediation and maintainer re-review;
- pinned release commit and Wasm SHA-256;
- named admin, provider, operator, signer, and recovery accounts;
- signed mainnet transaction plan and rollback owner.

The audit/review gate passes only when all code and configuration is final,
`cargo fmt`, `cargo clippy`, `cargo test`, and the Wasm build pass, no unresolved
critical/high findings remain, and a maintainer has linked approval to the
deployment ticket. "Audited" means independent review of the exact commit and
Wasm, not merely review of a branch that later changed.

## Key custody

Use Stellar CLI identities backed by the Stellar CLI keystore. Never place seeds
in `.env`, shell history, source files, CI logs, or deployment scripts.

| Role | Authority | Minimum custody |
| --- | --- | --- |
| Admin | `init`, token configuration, upgrade proposal/cancellation/execution | Mainnet multisig account, threshold 2-of-3 or stronger |
| Provider | service registration, rates, active status, attestor and settlement | Dedicated provider account |
| Operator | optional settlement-only role | Separate hot wallet with no rate or ownership authority |
| Deployer | Wasm upload/deploy transactions | Dedicated low-value account |
| Recovery | replace lost signer, rotate multisig, emergency upgrade/cancel | Offline 2-of-3 recovery multisig, geographically separated |

The admin is a Stellar account configured by `init --admin`; the contract stores
that account and all admin entry points require it to sign. Configure the
account as a multisig with named signer IDs, a low security threshold for
deploy/review transactions, and a high threshold for signer replacement and
recovery. Store one signing key offline and one in an independently managed
HSM or secure enclave. The second operator key is not an emergency admin.

Provider and operator keys must be rotated by revoking the old operator or
replacing provider-controlled service configuration. For provider-key loss,
disable new leases, revoke affected operators, halt payouts if implemented,
and migrate services through the maintainer-approved service-transfer procedure.
For admin signer loss, use the recovery quorum to remove the signer and raise
thresholds; if the admin key itself is lost, recovery requires the original
admin account's multisig recovery process and the deployed contract cannot be
made to trust a new address without an upgrade.

## Pre-deployment checklist

1. Pin the release commit and verify a clean worktree.
2. Run all gates and archive complete outputs:

   ```bash
   git rev-parse HEAD
   git status --porcelain
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all
   rustup target add wasm32v1-none
   cargo build --target wasm32v1-none --release --all
   sha256sum target/wasm32v1-none/release/keylease_registry.wasm
   ```

3. Independently reproduce the Wasm hash from a clean checkout.
4. Confirm RPC, network passphrase, native SAC/token id, multisig threshold,
   provider address, endpoint hash, and planned service id.
5. Dry-run every command and obtain recorded maintainer approval.

## Deploy

Set public values only. Signing identities remain in the Stellar CLI keystore.

```bash
export NETWORK=mainnet
export RPC_URL=https://<approved-mainnet-rpc>
export DEPLOYER=keylease-mainnet-deployer
export ADMIN=G<MAINNET_MULTISIG_ADDRESS>
export PROVIDER=G<DEDICATED_PROVIDER_ADDRESS>
export TOKEN=C<APPROVED_SEP41_TOKEN_ADDRESS>
export ENDPOINT_HASH=<64-HEX-CHARACTERS>
export RATE_PER_CALL=<STROOPS_PER_CALL>

stellar network add mainnet \
  --rpc-url "$RPC_URL" \
  --network-passphrase "Public Global Stellar Network ; September 2015"

stellar contract upload \
  target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account "$DEPLOYER" --network "$NETWORK"

REGISTRY=$(stellar contract deploy \
  --wasm target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account "$DEPLOYER" --network "$NETWORK")

stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" --send=yes -- \
  init --admin "$ADMIN"

stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" --send=yes -- \
  set_token --admin "$ADMIN" --token "$TOKEN"

stellar contract invoke --id "$REGISTRY" --source-account "$PROVIDER" \
  --network "$NETWORK" --send=yes -- \
  register_service --provider "$PROVIDER" \
  --rate_per_call "$RATE_PER_CALL" --endpoint_hash "$ENDPOINT_HASH"
```

Expected:

- upload output is the Wasm hash and it equals the pinned SHA-256;
- deploy output starts with `C` and is the new contract id;
- `init`, `set_token`, and `register_service` each return success and emit
  `registry_initialized`, `escrow_token_set`, and `service_registered`;
- no mock token or test identity is used.

## Verify

Run reads and record transaction hashes, ledger numbers, contract id, and output:

```bash
stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" -- version
stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" -- get_admin
stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" -- get_token
stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" -- get_service --service_id 1
stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" -- service_count
```

Expected: `version()` equals the workspace release version; `get_admin()` equals
`$ADMIN`; `get_token()` equals `$TOKEN`; service 1 has `$PROVIDER`,
`$RATE_PER_CALL`, and `$ENDPOINT_HASH`; `service_count` equals `1`. Independently
verify contract metadata, Wasm hash, events, and live balances in a block
explorer. A maintainer must sign off the verification record before any consumer
funds are advertised.

## Monitor

Run 24/7 alerting for:

- every `lease_created`, `lease_settled`, `lease_revoked`, and
  `lease_cancelled` event;
- aggregate payout increase or circuit pause event;
- payout/refund conservation mismatch;
- unexpected admin, token, provider, operator, upgrade, or rate change;
- RPC or ledger lag, failed authentication, and contract balance anomalies;
- consumer or provider token balances diverging from published settlement totals.

Example event query:

```bash
stellar events --id "$REGISTRY" --network "$NETWORK" --start-ledger "$START_LEDGER"
```

Only an RPC endpoint whose event window is documented may be used. If the
window does not cover the deployment ledger, restore an independently managed
indexer and retain signed query results. Alerts require a human acknowledgement;
indexing alone is not sufficient.

## Upgrade

Upgrades require the 24-hour proposal delay and an approved maintenance window.

```bash
NEW_HASH=$(stellar contract upload \
  target/wasm32v1-none/release/keylease_registry.wasm \
  --source-account "$DEPLOYER" --network "$NETWORK")

stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" --send=yes -- \
  propose_upgrade --admin "$ADMIN" --new_wasm_hash "$NEW_HASH"

stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" -- get_pending_upgrade
```

Wait until `execute_after` plus at least 24 hours and re-run audit diff,
verification, Wasm hash, and signer approval. To execute:

```bash
stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" --send=yes -- \
  execute_upgrade --admin "$ADMIN"

stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" -- version
```

Expected `execute_upgrade` to return success and emit `contract_upgraded`;
`version()` must equal the pinned release. To cancel before execution:

```bash
stellar contract invoke --id "$REGISTRY" --source-account "$DEPLOYER" \
  --network "$NETWORK" --send=yes -- \
  cancel_upgrade --admin "$ADMIN"
```

## Rollback

If verification fails or anomalous payouts occur:

1. Do not send a new lease or payout transaction.
2. Pause the service, revoke operators, or raise/circuit-break per the approved
   incident runbook.
3. If an upgrade was proposed, execute `cancel_upgrade` from the multisig and
   verify no pending proposal remains.
4. Keep the contract id and existing leases unchanged; no funds can be reclaimed
   globally by redeploying.
5. If the deployed Wasm is malicious or materially defective, use the approved
   upgrade to a pinned remediated Wasm after re-audit; otherwise continue only
   the consumer revoke and documented settlement paths.
6. Publish incident status, affected ledger range, token balances, transaction
   hashes, and a recovery decision.

Rollback is not "deploy the old Wasm" unless the old Wasm hash was built from
an audited, storage-compatible revision and the proposal delay/maintainer gate
has passed. Never overwrite the contract id or migrate balances manually.
