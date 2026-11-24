use soroban_sdk::{contracttype, Address, BytesN, Env};

use crate::errors::Error;
use crate::types::{Lease, Service};
use crate::SETTLE_GRACE_PERIOD_SECS;

/// Expected Stellar ledger close time, used to translate lease durations into
/// ledger counts for TTL extension.
pub const LEDGER_CLOSE_SECS: u64 = 5;

/// Ledger count below which a persistent entry is extended (~1 day @ 5s ledgers).
pub const LEDGER_BUMP_THRESHOLD: u32 = 17_280;
/// Ledger count that persistent entries are extended to (~30 days @ 5s ledgers).
pub const LEDGER_BUMP_EXTEND: u32 = 518_400;

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
    /// Optional ed25519 public key that must sign usage receipts for a service
    /// (persistent storage). Absent means settlements stay unsigned.
    ServiceReceiptKey(u64),
    /// Pull-payment balance claimable by an address, in token stroops
    /// (persistent storage).
    Claimable(Address),
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
    env.storage()
        .instance()
        .extend_ttl(LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
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

pub fn save_service(env: &Env, service: &Service) {
    let key = DataKey::Service(service.service_id);
    env.storage().persistent().set(&key, service);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
}

pub fn get_service(env: &Env, service_id: u64) -> Result<Service, Error> {
    let key = DataKey::Service(service_id);
    let service = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::ServiceNotFound)?;
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
    Ok(service)
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

pub fn save_lease(env: &Env, lease: &Lease) {
    let key = DataKey::Lease(lease.lease_id);
    env.storage().persistent().set(&key, lease);
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
/// `create_lease` caps `duration_secs` at `MAX_LEASE_DURATION_SECS`, so this is
/// always bounded and a locked deposit can never outlive its lease entry.
fn lease_ttl_extend(env: &Env, lease: &Lease) -> u32 {
    let now = env.ledger().timestamp();
    let remaining_secs = lease
        .expires_at
        .saturating_sub(now)
        .saturating_add(SETTLE_GRACE_PERIOD_SECS);
    let ledgers = remaining_secs / LEDGER_CLOSE_SECS;
    let extend = ledgers.saturating_add(LEDGER_BUMP_THRESHOLD as u64);
    extend.min(u32::MAX as u64) as u32
}

pub fn get_lease(env: &Env, lease_id: u64) -> Result<Lease, Error> {
    let key = DataKey::Lease(lease_id);
    let lease = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::LeaseNotFound)?;
    env.storage().persistent().extend_ttl(
        &key,
        LEDGER_BUMP_THRESHOLD,
        lease_ttl_extend(env, &lease),
    );
    Ok(lease)
}

// ---------------------------------------------------------------------------
// Optional signed usage receipts
// ---------------------------------------------------------------------------

/// Return the ed25519 public key a service requires on usage receipts, if any.
pub fn get_service_receipt_key(env: &Env, service_id: u64) -> Option<BytesN<32>> {
    let key = DataKey::ServiceReceiptKey(service_id);
    let receipt_key = env.storage().persistent().get(&key);
    if receipt_key.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
    }
    receipt_key
}

pub fn set_service_receipt_key(env: &Env, service_id: u64, receipt_key: &BytesN<32>) {
    let key = DataKey::ServiceReceiptKey(service_id);
    env.storage().persistent().set(&key, receipt_key);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
}

pub fn remove_service_receipt_key(env: &Env, service_id: u64) {
    env.storage()
        .persistent()
        .remove(&DataKey::ServiceReceiptKey(service_id));
}

// ---------------------------------------------------------------------------
// Pull-payment claimable balances
// ---------------------------------------------------------------------------

/// Credit `amount` to `beneficiary`'s claimable balance. Zero amounts are a
/// no-op so settlement paths do not pay rent for empty entries.
pub fn add_claimable(env: &Env, beneficiary: &Address, amount: i128) -> Result<(), Error> {
    if amount <= 0 {
        return Ok(());
    }
    let key = DataKey::Claimable(beneficiary.clone());
    let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    let updated = current.checked_add(amount).ok_or(Error::Overflow)?;
    env.storage().persistent().set(&key, &updated);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
    Ok(())
}

/// Read (without mutating) the balance `beneficiary` can currently claim.
pub fn get_claimable(env: &Env, beneficiary: &Address) -> i128 {
    let key = DataKey::Claimable(beneficiary.clone());
    let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if amount > 0 {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_BUMP_THRESHOLD, LEDGER_BUMP_EXTEND);
    }
    amount
}

/// Remove and return `beneficiary`'s claimable balance.
///
/// The entry is deleted before the caller transfers tokens, so a re-entrant
/// token that calls `claim` again sees an empty balance.
pub fn take_claimable(env: &Env, beneficiary: &Address) -> i128 {
    let key = DataKey::Claimable(beneficiary.clone());
    let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if amount > 0 {
        env.storage().persistent().remove(&key);
    }
    amount
}
