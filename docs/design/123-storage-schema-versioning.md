# Design note — Storage schema versioning and safe upgrades (#123)

> **Status:** Proposed — **awaiting maintainer approval**. Per
> [CONTRIBUTING](https://github.com/KeyLease-Security/keylease-core/blob/main/CONTRIBUTING.md),
> a maintainer must approve this design; the accompanying PR implements it so
> the review can happen against working code and tests.
>
> **Issue:** [#123 — Version the storage schema and reject unsafe upgrades](https://github.com/KeyLease-Security/keylease-core/issues/123)
> **Touches:** `storage.rs`, `lib.rs`, `errors.rs`, `events.rs`, `scripts/deploy-testnet.sh`

## 1. Problem

`execute_upgrade` replaces the Wasm but keeps every stored entry. The code assumes the
`DataKey` variants and the `Service` / `Lease` shapes never change, but nothing
records which layout a deployment holds, and nothing checks it after an upgrade.
An upgrade that renames a variant or retypes a field silently orphans or
misreads live escrow (`LeaseNotFound` on a funded lease, or a `locked_deposit`
decoded as something else). A rollback after new data has been written has the
same effect in reverse.

## 2. Invariants

1. The registry always knows which storage schema its data conforms to.
2. Code never **writes** through a state-changing entry point unless its
   compiled schema equals the stored one. A mismatch fails closed.
3. Older data reaches the new schema only through an explicit, admin-authorized
   and **bounded** `migrate`, applied atomically.
4. Newer data is never migrated "down" and never written by older code.
5. The upgrade entry points (`propose_upgrade`, `cancel_upgrade`,
   `execute_upgrade`) are always reachable by the admin, so a bad install can be
   reverted. The 24h upgrade time lock still applies to that revert.
6. Existing deployments are not stranded. Their data is schema `0`, and `0 → 1`
   only adds the stamp.
7. Existing keys, event payloads and error codes are unchanged (additive only).

## 3. Approach

### Version record

- `storage::SCHEMA_VERSION: u32 = 1` is the layout this Wasm reads and writes.
  It is mirrored in the `storageSchemaVersion` `contractmeta!` entry, so a
  reviewer can read it from the uploaded artifact *before* installing it. A test
  fails if the two diverge.
- `DataKey::SchemaVersion` (instance storage, `u32`) is the layout the stored
  data conforms to. `init` stamps it.
- An initialized registry without a stamp reports `LEGACY_SCHEMA_VERSION = 0`.
  An uninitialized one has no data to misread and is not gated.

### Checks

- **`init`:** writes the stamp. It refuses (`AlreadyInitialized`) if a stamp
  already exists without an admin, since that is state this contract did not
  write.
- **After an upgrade:** the old code cannot inspect the new Wasm (the swap takes
  effect after the invocation), so the check runs in the **new** code on every
  state-changing call. `storage::check_schema` is called by `set_token`,
  `register_service`, `set_service_active`, `create_lease`, `settle_lease` and
  `revoke_expired_lease`:
  - stored `==` code: run normally.
  - stored `<` code: `SchemaMigrationRequired` (29).
  - stored `>` code (a downgrade): `SchemaVersionUnsupported` (30).

  This puts the registry into a **stopped** state from the upgrade transaction
  until `migrate` succeeds, without needing a separate pause flag.
- **Not gated:** the upgrade entry points (so a rollback is always possible),
  `migrate`,
  `bump_service` / `bump_lease` (which change no stored value and keep entries
  alive during the stop) and every view.

### `migrate(admin) -> u32`

Admin only. If the stored version equals the code's, it is a silent no-op and
returns the version, so the deploy script can always call it. If the stored
version is newer, it returns `SchemaVersionUnsupported`. Otherwise it applies
`storage::migrate_step(v)` for each `v` in `stored..SCHEMA_VERSION`, writes the
stamp, extends the instance TTL and emits
`schema_migrated { admin, from_version, to_version }`, all in one transaction.
`migrate_step` is a `match` on the source version. A version with no arm has no
path and is rejected. Today the only arm is `0 => Ok(())`.

Steps must be bounded. They may rewrite instance storage but never iterate over
services or leases. A change to persistent entries is migrated lazily by an
accessor that reads both shapes and writes the new one, and the step only marks
that those readers are live.

### Procedure

How a migration is proposed, reviewed and executed is documented in
[Deployment → Storage schema migrations](../deployment.md#storage-schema-migrations).
In short: the PR that changes the layout bumps the constant and the metadata,
adds the `migrate_step` arm, a stopped → migrate → running test and a design
note. The reviewer checks the arm is bounded and verifies `storageSchemaVersion`
on the uploaded Wasm. The operator runs `execute_upgrade` and then `migrate` in
the next transaction. `deploy-testnet.sh --execute-upgrade` now does both.

### Rejected alternatives

- **Check the new Wasm inside `execute_upgrade`.** Soroban gives the running contract no
  way to read another Wasm's metadata or code, so the old code cannot validate
  the new one. The check has to live in the new code.
- **A separate admin `pause` flag.** This would duplicate the stop the schema
  gate already provides, and it relies on the operator remembering to pause.
  The gate stops the registry automatically in exactly the window that matters.
  A general pause (open in other PRs) composes with this design and is not
  required by it.
- **Run the migration inside the new code's first call.** That is implicit and
  unauthorized, a migration failure would surface in an unrelated user's
  transaction, and there would be no single reviewed event marking the switch.
- **Version each `Service` / `Lease` entry.** This makes every entry larger and
  doesn't detect a wholesale layout mismatch. Lazy per-entry migration remains
  available inside a step when it is needed.
- **Treat the absent stamp as `1`.** That would skip the stop on the first
  upgrade, but leave legacy deployments indistinguishable from stamped ones.
  Using `0` makes the first rollout exercise the full procedure once, at no
  risk, because the `0 → 1` step is a no-op.

## 4. Threat model

| Case | Handling |
| --- | --- |
| Upgrade to code with a new layout, `migrate` forgotten | Every state-changing call reverts with `SchemaMigrationRequired`. No value moves. |
| Downgrade after new data was written | `SchemaVersionUnsupported` everywhere, `migrate` refuses, and only a re-upgrade resumes. |
| A third party triggers a migration | `migrate` requires the admin's auth and identity (`Unauthorized`). |
| A migration with no defined path | `migrate_step` rejects it, and the whole transaction reverts. |
| A migration runs out of budget halfway | Steps are bounded, and a failure reverts atomically with no partial stamp. |
| Bad upgrade blocks its own fix | The upgrade entry points are not gated. |
| Stopped registry lets entries expire | `bump_service` and `bump_lease` stay available. |
| Foreign or corrupted state before `init` | `init` refuses a pre-existing stamp. |

**Trade-off:** while stopped, consumers cannot `revoke_expired_lease` either.
That is deliberate, because code that may misread a lease must not pay one
out. The window is one transaction long when the procedure is followed, and the
24-hour grace window already exceeds it by orders of magnitude.

## 5. Test plan

In `contracts/registry/src/test.rs`, under *Storage schema versioning*:

- `SCHEMA_VERSION` is pinned and matches the `storageSchemaVersion` metadata.
- `init` stamps the version. `schema_version` before `init` is `NotInitialized`.
  `init` refuses a pre-existing stamp.
- Newer stored schema: all fourteen gated entry points return
  `SchemaVersionUnsupported`, `migrate` refuses, and escrow is unchanged.
- Older (legacy) stored schema: all fourteen return `SchemaMigrationRequired`.
  `migrate` emits `schema_migrated` and stamps the version, then a pre-upgrade
  lease settles and new services register.
- `migrate` is admin-only (even as a no-op), requires `init`, and is a silent
  no-op when the schema is current. An undefined step is rejected.
- `propose_upgrade`, `cancel_upgrade` and `execute_upgrade` are not blocked by a
  mismatch, but still enforce admin.
- TTL bumps and views keep working while stopped.
