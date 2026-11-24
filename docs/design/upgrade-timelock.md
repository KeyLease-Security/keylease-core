# Design note: time-locked registry upgrades

> **Status: draft for maintainer review.** This note is required by
> `CONTRIBUTING.md` for a 200-point (`high`) issue. Implementation was requested
> explicitly, but merging the change is gated on a maintainer approving this
> design.

## 1. Problem

`KeyLeaseRegistry::upgrade` accepts a Wasm hash and calls
`deployer().update_current_contract_wasm` in the same transaction. The admin can
therefore replace the rules governing in-flight leases atomically with the
upload, giving consumers no window to observe the change and revoke before new
rules apply. Storage and balances survive an upgrade, which makes an exit window
both useful and practical.

Impact: a compromised or hostile admin key can change settlement/refund logic
without any warning period, defeating the consumer exit guarantee that the rest
of the protocol is built around.

## 2. Invariants

1. No Wasm may be installed until a delay has elapsed since the proposal was
   recorded on-ledger.
2. The admins who propose and execute may be the same key; the delay, not a
   second party, is the safety mechanism.
3. There is always a cancel path, and cancellation never installs code.
4. A proposal records the exact Wasm hash **and** the ledger timestamp after
   which it may execute, and both are readable by clients.
5. Every state transition is authenticated by the current admin and emits a
   typed event; events never contain secret material.
6. Upgrades keep the contract id, `DataKey` layout, stored values, token balances
   and in-flight leases. This change only adds an instance key.

## 3. Approach

* Add `UPGRADE_DELAY_SECS = 86_400` (24 hours) as a public, documented constant,
  matching the existing settlement grace period.
* Replace `upgrade` with three admin-only entry points:
  * `propose_upgrade(admin, new_wasm_hash)` — stores
    `PendingUpgrade { new_wasm_hash, execute_after }` under
    `DataKey::PendingUpgrade` (instance storage) and emits `upgrade_proposed`.
    A new proposal replaces a pending one and restarts the delay.
  * `execute_upgrade(admin)` — requires a pending proposal and
    `ledger.timestamp() >= execute_after`, installs the Wasm, clears the pending
    entry, and emits `contract_upgraded` with both the hash and the ledger
    timestamp. Executing early returns `UpgradeDelayNotElapsed`.
  * `cancel_upgrade(admin)` — clears the proposal and emits `upgrade_cancelled`.
* Add `get_pending_upgrade() -> Option<PendingUpgrade>` as a read-only view.
* Append error codes only (17, 18); never renumber existing codes.
* Update the README interface/event tables and the deployment guide so operators
  use the new flow.

### Rejected alternatives

* **Two-party approval / multisig** — changes the trust model and needs a
  governance design outside this issue's scope.
* **Delay stored as a ledger sequence number** — ledger timestamps are already
  how lease expiry works, and they make the delay human-readable (24h). A
  sequence number would couple the delay to close-time assumptions.
* **Keep the old instant `upgrade` for emergencies** — that is exactly the
  unsafe path being removed; emergencies are covered by `cancel_upgrade` plus a
  fresh proposal.

## 4. Threat model

* **Admin key compromise:** the attacker can propose, but cannot install for at
  least 24 hours. Consumers can watch `upgrade_proposed`, use
  `get_pending_upgrade`, and revoke/settle before `execute_after`.
* **Front-running the execution:** `execute_upgrade` re-checks the ledger
  timestamp at execution time; a proposal cannot be backdated.
* **Griefing by repeated proposals:** an admin (or stolen key) can restart the
  clock, but this only ever delays an upgrade and never shortens the window.
* **Storage eviction:** the pending entry lives in instance storage, whose TTL is
  extended to ~30 days by `extend_instance_ttl`; the 24-hour delay is well inside
  that horizon.
* **Event spoofing:** events are published by the registry contract only, and
  the hash/ledger come from the stored proposal.

## 5. Test plan

* `propose_upgrade` stores the hash and execution ledger in the view and emits
  `upgrade_proposed` carrying both.
* `execute_upgrade` before `execute_after` fails with `UpgradeDelayNotElapsed`
  and leaves the proposal intact.
* `cancel_upgrade` removes the proposal and emits `upgrade_cancelled`; execution
  afterwards fails with `NoPendingUpgrade`.
* Non-admins cannot propose, execute or cancel.
* A proposal replaces a previous one and resets the clock.
* Execution at/after the boundary installs the Wasm and publishes
  `contract_upgraded` with the hash and ledger. A 40-byte, hand-built module
  with a `contractenvmetav0` metadata section is uploaded to the test host so
  the real `update_current_contract_wasm` primitive is exercised without a
  build-time Wasm dependency or third-party fixture.
* Full CI gate: `cargo fmt --all -- --check`, `cargo clippy --all-targets --
  -D warnings`, `cargo test --all`, and
  `cargo build --target wasm32v1-none --release --all`.
