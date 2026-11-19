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
//! | `get_service`, `get_lease`, `lease_status`, `remaining_calls`, `locked_balance` | — | — | — |
//!
//! The last row is the read-only surface: it never extends anything, so a
//! simulation of it costs instructions and no rent. A lease or service that is
//! merely *read* for months has to be bumped explicitly, which is what the two
//! `bump_*` entry points are for.

use soroban_sdk::{contracttype, Address, Env};

use crate::errors::Error;
use crate::types::{Lease, Service, ServiceStats};
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
