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
//! | `set_token` | extend, unless the token is unchanged | — | — |
//! | `set_service_active` | — | extend (save) | — |
//! | `create_lease` | extend | extend | extend (save) |
//! | `settle_lease`, `revoke_expired_lease` | — | — | extend (save) |
//! | [`crate::KeyLeaseRegistry::bump_service`] | — | extend | — |
//! | [`crate::KeyLeaseRegistry::bump_lease`] | — | — | extend |
//! | `get_service`, `get_lease`, `lease_status`, `remaining_calls`, `locked_balance`, `services_for_provider` | — | — | — |
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

use soroban_sdk::{contracttype, Address, Env, Vec};

use crate::errors::Error;
use crate::types::{Lease, ProviderServicePage, Service};
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
    /// Service ids registered by a provider, oldest first (persistent storage).
    ///
    /// Derived state, appended to by [`save_service`]: `Service.provider` stays
    /// the authority on who owns what, and the read path re-checks every id it
    /// returns. An entry here can therefore only ever make a page shorter or
    /// contain an id the provider no longer owns — never report a service that
    /// belongs to someone else.
    ProviderServices(Address),
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
