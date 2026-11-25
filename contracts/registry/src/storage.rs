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
//! | `propose_admin`, `cancel_admin_transfer`, `accept_admin` | extend | — | — |
//! | `set_token` | extend, unless the token is unchanged | — | — |
//! | `set_service_active` | — | extend (save) | — |
//! | `set_service_attestor` | — | attestor entry extends when set, is removed when cleared | — |
//! | `set_service_attestor` | — | attestor entry extends when set, is removed when cleared | — |
//! | `create_lease` | extend | extend | extend (save) |
//! | `settle_lease`, `revoke_expired_lease` | — | — | extend (save) |
//! | [`crate::KeyLeaseRegistry::bump_service`] | — | extend | — |
//! | [`crate::KeyLeaseRegistry::bump_lease`] | — | — | extend |
//! | `get_pending_admin`, `get_service`, `get_lease`, `lease_status`, `remaining_calls`, `locked_balance`, `services_for_provider` | — | — | — |
//! | [`crate::KeyLeaseRegistry::get_services`], [`crate::KeyLeaseRegistry::get_leases`] | — | — | — |
//!
//! The last row is the read-only surface: it never extends anything, so a
//! simulation of it costs instructions and no rent. A lease or service that is
//! merely *read* for months has to be bumped explicitly, which is what the two
//! `bump_*` entry points are for.
//!
//! `ProviderServices(address)`, the service-id index behind
//! [`crate::KeyLeaseRegistry::services_for_provider`], follows the service rule
//! rather than the lease one: [`save_service`] extends it beside the record it
//! indexes, so any write to any of a provider's services keeps that provider's
//! catalogue alive, and no read extends it.
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

use soroban_sdk::{contracttype, Address, BytesN, Env, Map, Symbol, TryFromVal, Val, Vec};

/// Storage schema this Wasm reads and writes.
///
/// Bump it in the same change that alters a `DataKey` variant, a stored value
/// shape or the meaning of a stored value, and add the matching step to
/// `storage::migrate_step`. Purely additive changes that old entries already satisfy
/// (a new key that defaults when absent) do not need a bump.
///
/// | Version | Layout |
/// | --- | --- |
/// | `0` | v0.2.x and earlier: the v1 layout without a `SchemaVersion` entry |
/// | `1` | `Admin`, `Token`, `ServiceCount`, `LeaseCount`, `Service(u64)`, `Lease(u64)`, `ServiceStats(u64)`, `LeaseRevoked(u64)`, `ServiceAttestor(u64)`, `PendingUpgrade`, `ProviderServices(Address)`, `CancelRequest(u64)`, `Settlement(u64)`, `SchemaVersion`, `PendingAdmin` |
///
/// Every key in `1` other than `SchemaVersion` that v0.2.x did not have reads
/// as absent on legacy data and defaults safely, so `0 -> 1` rewrites nothing.
pub const SCHEMA_VERSION: u32 = 1;

/// Version reported for an initialized registry that has no `SchemaVersion`
/// entry, i.e. one deployed before the schema was versioned.
pub const LEGACY_SCHEMA_VERSION: u32 = 0;

use crate::errors::Error;
use crate::types::{
    CancelRequest, Lease, LeasePage, PendingAdminTransfer, PendingUpgrade, ProviderServicePage,
    Service, ServicePage, ServiceStats, Settlement,
};
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
    /// Cumulative settlement totals by service id (persistent storage).
    ServiceStats(u64),
    /// Presence marker for a lease reclaimed by its consumer (persistent storage).
    LeaseRevoked(u64),
    /// Optional ed25519 attestor key for a service (persistent storage).
    ///
    /// Kept out of [`Service`] on purpose: `Service` is a stored struct, so a
    /// new field would change the encoding of every existing service entry. A
    /// new key is invisible until the provider opts the service in.
    ServiceAttestor(u64),
    /// Pending, time-locked Wasm upgrade (instance storage). Append-only:
    /// existing keys keep their wire encoding.
    PendingUpgrade,
    /// Service ids registered by a provider, oldest first (persistent storage).
    ///
    /// Derived state, appended to by [`save_service`]: `Service.provider` stays
    /// the authority on who owns what, and the read path re-checks every id it
    /// returns. An entry here can therefore only ever make a page shorter or
    /// contain an id the provider no longer owns — never report a service that
    /// belongs to someone else.
    ProviderServices(Address),
    /// Pending early-cancellation request by lease id (persistent storage).
    CancelRequest(u64),
    /// `Settlement` by lease id (persistent storage).
    Settlement(u64),
    /// Storage schema version this registry's data was written under
    /// (instance storage). See [`SCHEMA_VERSION`].
    SchemaVersion,
    /// Admin hand-over awaiting acceptance (instance storage).
    ///
    /// Kept in instance storage on purpose: it shares the configuration
    /// entry's TTL, which every admin call extends, and it is bounded to a
    /// single value, so it adds no new rent policy of its own.
    PendingAdmin,
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

// ---------------------------------------------------------------------------
// Admin rotation
// ---------------------------------------------------------------------------

pub fn set_pending_admin(env: &Env, pending: &PendingAdminTransfer) {
    env.storage()
        .instance()
        .set(&DataKey::PendingAdmin, pending);
}

pub fn get_pending_admin(env: &Env) -> Option<PendingAdminTransfer> {
    env.storage().instance().get(&DataKey::PendingAdmin)
}

pub fn clear_pending_admin(env: &Env) {
    env.storage().instance().remove(&DataKey::PendingAdmin);
}

pub fn set_token(env: &Env, token: &Address) {
    env.storage().instance().set(&DataKey::Token, token);
}

pub fn get_token(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::Token)
}

/// Store a pending upgrade, replacing any previous proposal.
pub fn set_pending_upgrade(env: &Env, pending: &PendingUpgrade) {
    env.storage()
        .instance()
        .set(&DataKey::PendingUpgrade, pending);
}

/// Return the pending upgrade, if one is queued.
pub fn get_pending_upgrade(env: &Env) -> Option<PendingUpgrade> {
    env.storage().instance().get(&DataKey::PendingUpgrade)
}

/// Drop a pending upgrade after execution or cancellation.
pub fn remove_pending_upgrade(env: &Env) {
    env.storage().instance().remove(&DataKey::PendingUpgrade);
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

/// Persist a service, keep its entry alive for the standard horizon, and maintain
/// the per-provider index in the same step.
///
/// Every write path (`register_service`, `set_service_active`) goes through here,
/// so a service that is being maintained never needs an explicit bump. This is
/// also the only write path for a `Service`, so the index cannot be forgotten by a
/// future entry point: when a service transfer rewrites the record, the id lands
/// in the new owner's index on its own. See [`index_service`].
pub fn save_service(env: &Env, service: &Service) {
    let key = DataKey::Service(service.service_id);
    env.storage().persistent().set(&key, service);
    let extend_to = LEDGER_BUMP_EXTEND.min(ttl_ceiling(env));
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, extend_to);
    index_service(env, service);
}

/// Bump a service entry's TTL without reading it back to the caller.
///
/// This is the only path that extends a service's lifetime other than saving
/// it; [`get_service`] is a pure lookup. The service's attestor entry, when it
/// has one, is carried along: a lease keeps the service it leases from alive,
/// and the attestor key decides whether a settlement needs a signature, so the
/// two must not drift apart on the same call.
pub fn extend_service_ttl(env: &Env, service_id: u64) {
    let extend_to = LEDGER_BUMP_EXTEND.min(ttl_ceiling(env));
    env.storage().persistent().extend_ttl(
        &DataKey::Service(service_id),
        LEDGER_BUMP_THRESHOLD,
        extend_to,
    );

    let attestor_key = DataKey::ServiceAttestor(service_id);
    if env.storage().persistent().has(&attestor_key) {
        env.storage()
            .persistent()
            .extend_ttl(&attestor_key, LEDGER_BUMP_THRESHOLD, extend_to);
    }
}

// ---------------------------------------------------------------------------
// Optional attestor
// ---------------------------------------------------------------------------

/// Register, replace or clear the ed25519 attestor key of a service.
///
/// `None` removes the entry, which is also how a service opts back out: no
/// attestor key means `settle_lease` needs no signature, exactly as before this
/// feature existed.
pub fn set_service_attestor(env: &Env, service_id: u64, attestor: Option<&BytesN<32>>) {
    let key = DataKey::ServiceAttestor(service_id);
    match attestor {
        Some(attestor) => {
            env.storage().persistent().set(&key, attestor);
            let extend_to = LEDGER_BUMP_EXTEND.min(ttl_ceiling(env));
            env.storage()
                .persistent()
                .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, extend_to);
        }
        None => env.storage().persistent().remove(&key),
    }
}

/// Pure lookup for a service's attestor key. An absent entry means the service
/// has not opted in to attestation.
pub fn get_service_attestor(env: &Env, service_id: u64) -> Option<BytesN<32>> {
    env.storage()
        .persistent()
        .get(&DataKey::ServiceAttestor(service_id))
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

/// Read up to `limit` service entries from `start`, ascending by id.
///
/// Id spaces are monotonic from 1, and an id whose entry was evicted is
/// skipped, so `done` rather than the page length is the end marker. Looking
/// at a service extends no TTL; callers that need an entry to stay alive use
/// [`crate::KeyLeaseRegistry::bump_service`].
pub fn services_page(env: &Env, start: u64, limit: u32) -> ServicePage {
    let count = service_count(env);
    let first = start.max(1);
    let end = first.saturating_add(limit as u64);
    let mut services = Vec::new(env);
    let mut next = first;

    while next < end && next <= count {
        if let Some(service) = env.storage().persistent().get(&DataKey::Service(next)) {
            services.push_back(service);
        }
        next += 1;
    }

    ServicePage {
        services,
        next_start: next,
        done: next > count,
    }
}

/// Append `service.service_id` to its provider's index and keep that index alive.
///
/// Idempotence comes from the index itself rather than from comparing against the
/// stored record: re-registering the same service appends nothing, so
/// `save_service` can call this unconditionally. The TTL is extended on every save
/// rather than only on an append, which is what lets `set_service_active` refresh a
/// catalogue it does not add to. The index is append-only, which makes it
/// self-healing in one direction and deliberately lazy in the other — if a
/// service is later transferred, its id stays in the old owner's page and
/// [`get_provider_services`] drops it on read. Nothing here can leak a service
/// the provider does not own.
fn index_service(env: &Env, service: &Service) {
    let key = DataKey::ProviderServices(service.provider.clone());
    let mut ids: Vec<u64> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env));
    if !ids.contains(service.service_id) {
        ids.push_back(service.service_id);
        env.storage().persistent().set(&key, &ids);
    }
    // Extended on every save, not only on an append: `set_service_active` rewrites a
    // record whose id is already indexed, and a provider maintaining its catalogue is
    // also keeping the index that lists it alive. By the time control reaches here the
    // entry exists — an absent index always takes the append above — so the extension
    // cannot hit the missing-key path.
    let extend_to = LEDGER_BUMP_EXTEND.min(ttl_ceiling(env));
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, extend_to);
}

/// Return up to `limit` of `provider`'s services, starting at index entry `after`.
///
/// The page is bounded by `limit` regardless of how long the index is, and the
/// only walk-in-storage cost is one read per entry consumed. This is a pure read
/// in the sense the module doc means: it extends nothing, neither the listed
/// `Service` records nor the provider's own index entry.
pub fn get_provider_services(
    env: &Env,
    provider: &Address,
    after: u32,
    limit: u32,
) -> Result<ProviderServicePage, Error> {
    let key = DataKey::ProviderServices(provider.clone());
    let Some(ids) = env.storage().persistent().get::<DataKey, Vec<u64>>(&key) else {
        // A provider that has never had an index entry — never registered, or let it
        // expire — has an empty catalogue as far as discovery is concerned. That is a
        // normal answer, not an error, and callers cannot tell the two apart.
        return Ok(ProviderServicePage {
            service_ids: Vec::new(env),
            next: 0,
            done: true,
        });
    };

    let total = ids.len();
    let mut cursor = after.min(total);
    let mut service_ids = Vec::new(env);
    let mut consumed = 0u32;
    while consumed < limit {
        let Some(service_id) = ids.get(cursor) else {
            break;
        };
        cursor += 1;
        consumed += 1;
        if indexed_service(env, provider, service_id) {
            service_ids.push_back(service_id);
        }
    }

    Ok(ProviderServicePage {
        service_ids,
        next: cursor,
        done: cursor >= total,
    })
}

/// Whether `service_id` still belongs to `provider` according to the record, the
/// single authority on ownership.
fn indexed_service(env: &Env, provider: &Address, service_id: u64) -> bool {
    env.storage()
        .persistent()
        .get::<DataKey, Service>(&DataKey::Service(service_id))
        .is_some_and(|service| service.provider == *provider)
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------
//
// `Lease` gained a `created_at` field, and a `#[contracttype]` struct decodes by
// unpacking the exact field-name keys it knows: a value written before that field
// existed cannot be decoded into the current `Lease`, and the host raises (it does
// not return an error) when the key count differs. `get_lease` therefore inspects
// the stored map first and reads the pre-`created_at` shape through `LegacyLease`,
// reporting `created_at = 0` (unknown) instead of trapping. That keeps pre-upgrade
// leases settleable and revocable, i.e. escrow stays releasable across an upgrade.
//
// The change is append-only in every other sense: no `DataKey` variant was added,
// removed or renumbered, and the new field is last in the struct. Nothing has to
// be migrated up front, and an entry is rewritten in the current shape the next
// time it is saved (settlement or revocation).

/// Number of fields in the pre-`created_at` lease encoding.
const LEGACY_LEASE_FIELDS: u32 = 8;

/// The `Lease` shape stored by releases before `created_at` existed. Used only to
/// decode existing entries; never written except by tests.
#[contracttype]
struct LegacyLease {
    lease_id: u64,
    service_id: u64,
    consumer: Address,
    allocated_calls: u32,
    used_calls: u32,
    expires_at: u64,
    locked_deposit: i128,
    settled: bool,
}

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
    let key = DataKey::Lease(lease_id);
    let val: Val = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::LeaseNotFound)?;

    // Decide which shape is stored *before* decoding: the derive unpacks an exact
    // field-name key set and the host escalates a size mismatch instead of
    // returning a recoverable error.
    let map = Map::<Symbol, Val>::try_from_val(env, &val).map_err(|_| Error::LeaseNotFound)?;
    if map.contains_key(Symbol::new(env, "created_at")) {
        return Lease::try_from_val(env, &val).map_err(|_| Error::LeaseNotFound);
    }
    if map.len() != LEGACY_LEASE_FIELDS {
        // Neither a current nor a pre-`created_at` lease entry.
        return Err(Error::LeaseNotFound);
    }

    // A lease written before `created_at` existed. Its creation time is not
    // recoverable from storage, so report `0` (unknown) rather than failing a
    // settlement or refund that is still owed.
    let legacy = LegacyLease::try_from_val(env, &val).map_err(|_| Error::LeaseNotFound)?;
    Ok(Lease {
        lease_id: legacy.lease_id,
        service_id: legacy.service_id,
        consumer: legacy.consumer,
        allocated_calls: legacy.allocated_calls,
        used_calls: legacy.used_calls,
        created_at: 0,
        expires_at: legacy.expires_at,
        locked_deposit: legacy.locked_deposit,
        settled: legacy.settled,
    })
}

/// Test-only: rewrite a lease entry in the pre-`created_at` shape so the legacy
/// decode path in [`get_lease`] can be exercised.
#[cfg(test)]
pub(crate) fn save_legacy_lease_for_test(env: &Env, lease: &Lease) {
    let legacy = LegacyLease {
        lease_id: lease.lease_id,
        service_id: lease.service_id,
        consumer: lease.consumer.clone(),
        allocated_calls: lease.allocated_calls,
        used_calls: lease.used_calls,
        expires_at: lease.expires_at,
        locked_deposit: lease.locked_deposit,
        settled: lease.settled,
    };
    env.storage()
        .persistent()
        .set(&DataKey::Lease(lease.lease_id), &legacy);
}

// ---------------------------------------------------------------------------
// Settlements
// ---------------------------------------------------------------------------

pub fn save_settlement(env: &Env, settlement: &Settlement) {
    let key = DataKey::Settlement(settlement.lease_id);
    env.storage().persistent().set(&key, settlement);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
}

// ---------------------------------------------------------------------------
// Derived per-entity state: settlement stats and revocation markers
// ---------------------------------------------------------------------------
//
// Both keys below describe an entity that already has its own entry, and both
// are additive: neither changes the layout of `Service` or `Lease`, so entries
// written by an earlier Wasm keep decoding. Their TTL policy is therefore
// "stay readable at least as long as the entity they describe":
//
// * `ServiceStats` uses the generic persistent policy
//   (`LEDGER_BUMP_THRESHOLD` / `LEDGER_BUMP_EXTEND`, ~30 days) and is rewritten
//   by every settlement, so a service that is still being leased cannot age out
//   mid-life. If an entry does age out — a service idle for thirty days —
//   `get_service_stats` reads [`ServiceStats::zero`]. The counters are a
//   convenience over the ledger; `lease_settled` stays the authoritative record.
// * `LeaseRevoked` is extended with `lease_ttl_extend`, the horizon the lease
//   entry itself is given in the same transaction, so any lease that is still
//   readable always has its marker readable.
//
// Both reads are pure, in line with `get_service` and `get_lease`: neither
// extends anything. Longevity comes from the write that created the entry, so a
// query never bills the caller for rent on state it only wanted to look at.
//
// Neither key has a user-controlled lifetime: the marker is written once per
// lease by `revoke_expired_lease`, and the counters only move inside
// `settle_lease`, so no caller can grow storage without bound.

/// Pure lookup: reads a service's cumulative totals and touches no storage
/// metadata.
///
/// The entry is kept alive by [`record_settlement`], not by reading it, so a
/// stats query costs the caller rent for nothing it cannot report another way.
pub fn get_service_stats(env: &Env, service_id: u64) -> ServiceStats {
    let key = DataKey::ServiceStats(service_id);
    env.storage()
        .persistent()
        .get(&key)
        // A service with no settlement has no entry yet, and reads as zero.
        .unwrap_or(ServiceStats::zero())
}

/// Add one provider settlement to its service's cumulative totals.
///
/// Totals are checked, never saturating: a silently clamped revenue counter is
/// worse for a reporter than a reverted settlement. The bound is arithmetic,
/// not economic — with `MAX_RATE_PER_CALL` capping one settlement's payout
/// below 2^72, overflowing `total_payout` would take ~4 × 10^16 such
/// settlements, i.e. far more tokens than exist.
pub fn record_settlement(
    env: &Env,
    service_id: u64,
    settled_calls: u32,
    payout: i128,
) -> Result<(), Error> {
    let key = DataKey::ServiceStats(service_id);
    let mut stats: ServiceStats = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(ServiceStats::zero());

    stats.settled_leases = stats.settled_leases.checked_add(1).ok_or(Error::Overflow)?;
    stats.settled_calls = stats
        .settled_calls
        .checked_add(settled_calls as u64)
        .ok_or(Error::Overflow)?;
    stats.total_payout = stats
        .total_payout
        .checked_add(payout)
        .ok_or(Error::Overflow)?;

    env.storage().persistent().set(&key, &stats);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
    Ok(())
}

/// Record that `lease` was reclaimed by its consumer instead of settled.
pub fn mark_lease_revoked(env: &Env, lease: &Lease) {
    let key = DataKey::LeaseRevoked(lease.lease_id);
    env.storage().persistent().set(&key, &true);
    env.storage().persistent().extend_ttl(
        &key,
        LEDGER_BUMP_THRESHOLD,
        lease_ttl_extend(env, lease),
    );
}

/// Pure lookup: whether `lease` was reclaimed by its consumer rather than
/// settled by its provider.
///
/// No extension here. The marker was given the same horizon as the lease entry
/// in the transaction that wrote it, and `bump_lease` refuses a lease this
/// settled — so the marker can never outlive the entry it describes, and reading
/// it cannot buy anything the lease does not already have.
pub fn is_lease_revoked(env: &Env, lease: &Lease) -> bool {
    let key = DataKey::LeaseRevoked(lease.lease_id);
    env.storage().persistent().get(&key) == Some(true)
}

// ---------------------------------------------------------------------------
// Early-cancellation requests
// ---------------------------------------------------------------------------

pub fn save_cancel_request(env: &Env, request: &CancelRequest) {
    let key = DataKey::CancelRequest(request.lease_id);
    env.storage().persistent().set(&key, request);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
}

pub fn get_settlement(env: &Env, lease_id: u64) -> Result<Settlement, Error> {
    let key = DataKey::Settlement(lease_id);
    let settlement = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::SettlementNotFound)?;
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
    Ok(settlement)
}

pub fn get_cancel_request(env: &Env, lease_id: u64) -> Option<CancelRequest> {
    let key = DataKey::CancelRequest(lease_id);
    let request = env.storage().persistent().get(&key);
    if request.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
    }
    request
}

pub fn remove_cancel_request(env: &Env, lease_id: u64) {
    env.storage()
        .persistent()
        .remove(&DataKey::CancelRequest(lease_id));
}

/// Read up to `limit` lease entries from `start`, ascending by id.
///
/// Same bounded-window shape as [`services_page`]: the ids are `1..=
/// lease_count`, a lease whose entry was evicted is skipped, and no TTL is
/// extended by looking at one. Callers that need a lease to stay readable use
/// [`crate::KeyLeaseRegistry::bump_lease`].
pub fn leases_page(env: &Env, start: u64, limit: u32) -> LeasePage {
    let count = lease_count(env);
    let first = start.max(1);
    let end = first.saturating_add(limit as u64);
    let mut leases = Vec::new(env);
    let mut next = first;

    while next < end && next <= count {
        if let Some(lease) = env.storage().persistent().get(&DataKey::Lease(next)) {
            leases.push_back(lease);
        }
        next += 1;
    }

    LeasePage {
        leases,
        next_start: next,
        done: next > count,
    }
}
