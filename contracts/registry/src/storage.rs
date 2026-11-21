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
//! | `set_service_attestor` | — | attestor entry extends when set, is removed when cleared | — |
//! | `set_service_attestor` | — | attestor entry extends when set, is removed when cleared | — |
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

use soroban_sdk::{contracttype, Address, BytesN, Env, Map, Symbol, TryFromVal, Val};

use crate::errors::Error;
use crate::types::{Lease, PendingUpgrade, Service};
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
    /// Optional ed25519 attestor key for a service (persistent storage).
    ///
    /// Kept out of [`Service`] on purpose: `Service` is a stored struct, so a
    /// new field would change the encoding of every existing service entry. A
    /// new key is invisible until the provider opts the service in.
    ServiceAttestor(u64),
    /// Pending, time-locked Wasm upgrade (instance storage). Append-only:
    /// existing keys keep their wire encoding.
    PendingUpgrade,
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
