//! Persistent storage layout and the TTL policy that guards it.
//!
//! # Who extends a TTL
//!
//! An extension moves the entry's `liveUntilLedgerSeq`, which the host books as
//! a rent bump and bills the caller for across the whole extended horizon. In
//! this package's tests that is 48,684 of the 48,798 stroops `bump_lease`
//! estimates, against 113 for the same call with the bump removed (see
//! `contracts/registry/src/test.rs`), which is why nothing on the read-only path
//! extends anything.
//! Only the calls below write, and only these calls extend a TTL:
//!
//! | Call | Instance | Service entry | Lease entry |
//! | --- | --- | --- | --- |
//! | `init`, `upgrade`, `register_service` | extend | — | — |
//! | `migrate` (when it migrates) | extend | — | — |
//! | `set_token` | extend, unless the token is unchanged | — | — |
//! | `set_service_active` | — | extend (save) | — |
//! | `create_lease` | extend | extend | extend (save) |
//! | `settle_lease`, `revoke_expired_lease` | — | — | extend (save) |
//! | [`crate::KeyLeaseRegistry::bump_service`] | — | extend | — |
//! | [`crate::KeyLeaseRegistry::bump_lease`] | — | — | extend |
//! | `get_service`, `get_lease`, `lease_status`, `remaining_calls`, `locked_balance` | — | — | — |
//!
//! The last row is the read-only surface: it never extends anything, so a
//! simulation of it costs instructions and no rent. A lease or service that is
//! merely *read* for months has to be bumped explicitly, which is what the two
//! `bump_*` entry points are for.
//!
//! # Schema version
//!
//! [`SCHEMA_VERSION`] names the `DataKey` layout and stored value shapes this
//! Wasm reads and writes. `init` stamps it into instance storage, and every
//! state-changing entry point refuses to run unless the stamp matches (see
//! [`check_schema`]). After an upgrade to a Wasm with a newer schema the
//! registry is therefore *stopped* until the admin runs `migrate`; an upgrade
//! to an older schema (a downgrade) stays stopped until it is rolled back.
//! `docs/deployment.md` documents how a migration is proposed, reviewed and
//! executed.

use soroban_sdk::{contracttype, Address, Env};

/// Storage schema this Wasm reads and writes.
///
/// Bump it in the same change that alters a `DataKey` variant, a stored value
/// shape or the meaning of a stored value, and add the matching step to
/// [`migrate_step`]. Purely additive changes that old entries already satisfy
/// (a new key that defaults when absent) do not need a bump.
///
/// | Version | Layout |
/// | --- | --- |
/// | `0` | v0.2.x and earlier: the v1 layout without a `SchemaVersion` entry |
/// | `1` | `Admin`, `Token`, `ServiceCount`, `LeaseCount`, `Service(u64)`, `Lease(u64)`, `SchemaVersion` |
pub const SCHEMA_VERSION: u32 = 1;

/// Version reported for an initialized registry that has no `SchemaVersion`
/// entry, i.e. one deployed before the schema was versioned.
pub const LEGACY_SCHEMA_VERSION: u32 = 0;

use crate::errors::Error;
use crate::types::{Lease, Service};
use crate::SETTLE_GRACE_PERIOD_SECS;

/// Expected Stellar ledger close time, used to translate lease durations into
/// ledger counts for TTL extension.
pub const LEDGER_CLOSE_SECS: u64 = 5;

/// Ledger count below which a persistent entry is extended (~1 day @ 5s ledgers).
pub const LEDGER_BUMP_THRESHOLD: u32 = 17_280;
/// Ledger count that service and instance entries are extended to (~30 days).
pub const LEDGER_BUMP_EXTEND: u32 = 518_400;

/// The longest lifetime this contract will ever ask for an entry, in ledgers
/// (~90 days @ 5s ledgers).
///
/// Every extension in this module is clamped to this constant, and the clamp is
/// a policy bound rather than a workaround: it caps how much rent a single call
/// can create and how long a stale entry can squat in the ledger, independent of
/// whatever `expires_at` happens to say. It also matters if
/// [`crate::MAX_LEASE_DURATION_SECS`] is ever raised or a lease is migrated in by
/// an upgrade, because the lease policy below is derived from `expires_at` and
/// would otherwise grow without limit.
///
/// The value sits between the largest legitimate request and the network's
/// `maxEntryTtl` ledger setting (6,312,000 ledgers on Pubnet and Testnet). The
/// longest lease is `MAX_LEASE_DURATION_SECS` (30 days), whose policy asks for
/// its term plus the settlement grace window plus one buffer day, i.e. 552,960
/// ledgers, so a 90-day ceiling never truncates a real lease while staying well
/// below the network maximum.
///
/// Note on what the clamp is *not*: `soroban-test` does not enforce
/// `maxEntryTtl` on `extend_ttl` — it accepts an extension past the network cap,
/// including `u32::MAX`, and does not evict entries as the ledger advances. So
/// this bound cannot be demonstrated as "prevents a revert" in the test host, and
/// the tests here assert the clamped value, not a host rejection.
pub const MAX_ENTRY_TTL_LEDGERS: u32 = (90 * 24 * 60 * 60 / LEDGER_CLOSE_SECS) as u32;

/// The largest extension this deployment may ask for right now: the contract
/// ceiling, narrowed by whatever lifetime the network currently publishes.
///
/// Reading the live `max_ttl` rather than hard-coding the network cap means an
/// operator that lowers `maxEntryTtl` is respected instead of contradicted. The
/// test host accepts an over-cap extension without complaint, so the clamp that
/// tests can actually pin down is [`MAX_ENTRY_TTL_LEDGERS`]; this line is the
/// same discipline applied against a live network setting.
fn ttl_ceiling(env: &Env) -> u32 {
    MAX_ENTRY_TTL_LEDGERS.min(env.storage().max_ttl())
}

/// Storage keys for the registry.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Protocol administrator (instance storage).
    Admin,
    /// SEP-41 escrow token used for all deposits and payouts (instance storage).
    Token,
    /// Monotonic service id counter (instance storage).
    ServiceCount,
    /// Monotonic lease id counter (instance storage).
    LeaseCount,
    /// `Service` by id (persistent storage).
    Service(u64),
    /// `Lease` by id (persistent storage).
    Lease(u64),
    /// Storage schema version the stored data conforms to (instance storage).
    /// See [`SCHEMA_VERSION`].
    SchemaVersion,
}

// ---------------------------------------------------------------------------
// Initialization / configuration
// ---------------------------------------------------------------------------

pub fn is_initialized(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::Admin)
}

pub fn set_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&DataKey::Admin, admin);
}

pub fn get_admin(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::Admin)
}

// ---------------------------------------------------------------------------
// Schema version
// ---------------------------------------------------------------------------

pub fn set_schema_version(env: &Env, version: u32) {
    env.storage()
        .instance()
        .set(&DataKey::SchemaVersion, &version);
}

pub fn has_schema_version(env: &Env) -> bool {
    env.storage().instance().has(&DataKey::SchemaVersion)
}

/// Schema the stored data conforms to: the stamped version, or
/// [`LEGACY_SCHEMA_VERSION`] for a registry initialized before stamping
/// existed. `None` for a registry that holds no configuration yet.
pub fn stored_schema_version(env: &Env) -> Option<u32> {
    match env.storage().instance().get(&DataKey::SchemaVersion) {
        Some(version) => Some(version),
        None if is_initialized(env) => Some(LEGACY_SCHEMA_VERSION),
        None => None,
    }
}

/// Fail closed unless the stored data matches the schema this Wasm was built
/// for.
///
/// A registry with no configuration has nothing to misread and passes.
/// Older data needs `migrate` first; newer data means a downgrade, which this
/// code cannot read safely and must not write over.
pub fn check_schema(env: &Env) -> Result<(), Error> {
    match stored_schema_version(env) {
        None => Ok(()),
        Some(v) if v == SCHEMA_VERSION => Ok(()),
        Some(v) if v < SCHEMA_VERSION => Err(Error::SchemaMigrationRequired),
        Some(_) => Err(Error::SchemaVersionUnsupported),
    }
}

/// Apply the migration from schema `from` to `from + 1`.
///
/// Each arm must be bounded: it may rewrite instance storage, but it must not
/// iterate over services or leases. A change to persistent entries is migrated
/// lazily, by the accessor that reads them, and the arm only records that the
/// new readers are in place. Unknown versions have no path and are rejected.
pub fn migrate_step(_env: &Env, from: u32) -> Result<(), Error> {
    match from {
        // 0 -> 1: the layout is unchanged; versioning only adds the
        // `SchemaVersion` stamp, which the caller writes after the last step.
        0 => Ok(()),
        _ => Err(Error::SchemaVersionUnsupported),
    }
}

pub fn set_token(env: &Env, token: &Address) {
    env.storage().instance().set(&DataKey::Token, token);
}

pub fn get_token(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::Token)
}

/// Bump the TTL of the instance (configuration) storage entry.
pub fn extend_instance_ttl(env: &Env) {
    let extend_to = LEDGER_BUMP_EXTEND.min(ttl_ceiling(env));
    env.storage()
        .instance()
        .extend_ttl(LEDGER_BUMP_THRESHOLD, extend_to);
}

// ---------------------------------------------------------------------------
// Monotonic identifiers
// ---------------------------------------------------------------------------

pub fn next_service_id(env: &Env) -> Result<u64, Error> {
    let next = service_count(env).checked_add(1).ok_or(Error::Overflow)?;
    env.storage().instance().set(&DataKey::ServiceCount, &next);
    Ok(next)
}

pub fn next_lease_id(env: &Env) -> Result<u64, Error> {
    let next = lease_count(env).checked_add(1).ok_or(Error::Overflow)?;
    env.storage().instance().set(&DataKey::LeaseCount, &next);
    Ok(next)
}

pub fn service_count(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::ServiceCount)
        .unwrap_or(0)
}

pub fn lease_count(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&DataKey::LeaseCount)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

/// Persist a service and keep its entry alive for the standard horizon.
///
/// Every write path (`register_service`, `set_service_active`) goes through
/// here, so a service that is being maintained never needs an explicit bump.
pub fn save_service(env: &Env, service: &Service) {
    let key = DataKey::Service(service.service_id);
    env.storage().persistent().set(&key, service);
    let extend_to = LEDGER_BUMP_EXTEND.min(ttl_ceiling(env));
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, extend_to);
}

/// Bump a service entry's TTL without reading it back to the caller.
///
/// This is the only path that extends a service's lifetime other than saving
/// it; [`get_service`] is a pure lookup.
pub fn extend_service_ttl(env: &Env, service_id: u64) {
    let key = DataKey::Service(service_id);
    let extend_to = LEDGER_BUMP_EXTEND.min(ttl_ceiling(env));
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, extend_to);
}

/// Pure lookup: reads a service and touches no storage metadata.
///
/// Reads must not extend TTL: a "view" that bumps an entry bills the caller for
/// the whole extended horizon, which is why `get_service` costs 113 stroops and
/// `bump_service` over it costs tens of thousands.
pub fn get_service(env: &Env, service_id: u64) -> Result<Service, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Service(service_id))
        .ok_or(Error::ServiceNotFound)
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

/// Persist a lease and extend its entry for as long as the lease can matter.
pub fn save_lease(env: &Env, lease: &Lease) {
    let key = DataKey::Lease(lease.lease_id);
    env.storage().persistent().set(&key, lease);
    env.storage().persistent().extend_ttl(
        &key,
        LEDGER_BUMP_THRESHOLD,
        lease_ttl_extend(env, lease),
    );
}

/// Bump a lease entry's TTL to whatever its own policy asks for.
///
/// Separated from [`get_lease`] so that keeping an entry alive is an explicit,
/// metered operation rather than a side effect of reading it. Soroban's
/// `extend_ttl` can only raise `liveUntilLedgerSeq`, so this never shortens a
/// lease that was already extended further.
pub fn extend_lease_ttl(env: &Env, lease: &Lease) {
    let key = DataKey::Lease(lease.lease_id);
    env.storage().persistent().extend_ttl(
        &key,
        LEDGER_BUMP_THRESHOLD,
        lease_ttl_extend(env, lease),
    );
}

/// Ledgers a lease entry must survive: its remaining term, the provider's
/// settlement grace window, and a one-day read buffer.
///
/// Only the time the lease can actually be acted on is paid for. Extending every
/// lease to the generic thirty-day horizon made Soroban rent dominate short
/// leases on Testnet; the buffer here keeps the entry alive through expiry, the
/// 24-hour grace window, and a day of slack for the consumer's revoke call.
///
/// `create_lease` caps `duration_secs` at `MAX_LEASE_DURATION_SECS`, so a
/// well-formed lease can never ask for more than 552,960 ledgers and the clamp
/// below never truncates one. What it does bound is a lease whose `expires_at`
/// says otherwise — an entry carried in by an upgrade, or one created after a
/// future raise of the duration cap — whose ask would otherwise grow in
/// proportion to a value no caller can police.
pub(crate) fn lease_ttl_extend(env: &Env, lease: &Lease) -> u32 {
    let now = env.ledger().timestamp();
    let remaining_secs = lease
        .expires_at
        .saturating_sub(now)
        .saturating_add(SETTLE_GRACE_PERIOD_SECS);
    let ledgers = remaining_secs / LEDGER_CLOSE_SECS;
    let extend = ledgers.saturating_add(LEDGER_BUMP_THRESHOLD as u64);
    extend.min(ttl_ceiling(env) as u64) as u32
}

/// Pure lookup: reads a lease and touches no storage metadata.
///
/// A lease that needs to outlive its current horizon is kept alive by
/// [`crate::KeyLeaseRegistry::bump_lease`], not by looking at it.
pub fn get_lease(env: &Env, lease_id: u64) -> Result<Lease, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Lease(lease_id))
        .ok_or(Error::LeaseNotFound)
}
