#![no_std]

//! # KeyLease Registry
//!
//! KeyLease lets API and RPC providers sell **time-bound, ephemeral micro-leases**
//! instead of long-lived static credentials. A consumer locks a micropayment in
//! Soroban escrow and receives an on-chain authorization that the provider can
//! verify cheaply, while the consumer is protected by an automatic refund path.
//!
//! ## Lifecycle
//!
//! ```text
//!  provider                     registry                        consumer
//!     |                            |                                |
//!     |-- register_service(rate) ->|                                |
//!     |                            |<-- create_lease(calls, secs) --|
//!     |                            |    (escrow = rate * calls)     |
//!     |                            |                                |
//!     |-- settle_lease(used) ----->|  pay provider, refund rest --->|
//!     |                            |                                |
//!                                  |  (if provider never settles)   |
//!                                  |<-- revoke_expired_lease ------ |
//!                                  |  (full refund after 24h)       |
//! ```
//!
//! ## Trust model
//!
//! * The provider is trusted only to report *how many* calls were consumed, and
//!   only for leases it owns. It can never withdraw more than
//!   `allocated_calls * rate_per_call`, and it can only settle once.
//! * A consumer can always reclaim the full deposit if the provider fails to
//!   settle within [`SETTLE_GRACE_PERIOD_SECS`] of expiry.
//! * All ledger math uses checked arithmetic; overflow reverts the transaction.
//!
//! ## Stellar / Soroban dependencies
//!
//! This contract is not a generic escrow with a Stellar logo on it; it is built
//! out of Soroban primitives:
//!
//! * **SEP-41 tokens** — every deposit and payout is a cross-contract call to a
//!   Stellar Asset Contract or a custom SEP-41 token.
//! * **`Address` auth** — every actor is a Stellar account (`G...`) or contract
//!   (`C...`) address authenticated by the host's `require_auth()`.
//! * **Contract events** — every transition emits a typed Soroban event (see
//!   [`events`]) so indexers can follow escrow without polling.
//! * **Ledger time** — expiry is measured in ledger timestamps, not wall clocks.
//! * **Persistent storage TTL** — lease entries are extended for their full
//!   duration plus the grace period, so escrow cannot fall off the ledger.
//! * **Wasm upgrades** — the admin can install a new Wasm through the Soroban
//!   deployer, and `contractmeta!` identifies the running version on-chain.

mod errors;
mod events;
mod storage;
mod types;

#[cfg(test)]
mod test;

use soroban_sdk::{contract, contractimpl, token, Address, BytesN, Env, String};

pub use crate::errors::Error;
pub use crate::events::{
    ContractUpgraded, EscrowTokenSet, LeaseCreated, LeaseRevoked, LeaseSettled, ProtocolFeeSet,
    RegistryInitialized, ServiceRegistered, ServiceStatusChanged,
};
pub use crate::types::{Lease, LeaseStatus, Service};

// On-chain contract metadata, readable from the ledger without the source.
soroban_sdk::contractmeta!(key = "name", val = "KeyLeaseRegistry");
soroban_sdk::contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
soroban_sdk::contractmeta!(
    key = "description",
    val = "Time-bound micro-leases for API/RPC access, escrowed with SEP-41 tokens on Stellar."
);
soroban_sdk::contractmeta!(
    key = "repository",
    val = "https://github.com/KeyLease-Security/keylease-core"
);

/// Grace period after lease expiry during which the provider may still settle.
/// After this window elapses, the consumer may reclaim the full deposit.
pub const SETTLE_GRACE_PERIOD_SECS: u64 = 24 * 60 * 60;

/// Longest lease the TTL policy is guaranteed to keep alive.
///
/// Persistent entries are extended to cover the full lease term, the 24-hour
/// grace window and a one-day read buffer, so this bound guarantees a lease can
/// never outlive its storage entry. At thirty days the resulting TTL (~32 days)
/// is well under the network's maximum entry TTL (180 days on Testnet).
pub const MAX_LEASE_DURATION_SECS: u64 = 30 * 24 * 60 * 60;

/// Denominator for protocol fee rates expressed in basis points.
pub const BASIS_POINTS_DENOMINATOR: u32 = 10_000;

/// Maximum protocol fee: 1,000 basis points (10%).
pub const MAX_PROTOCOL_FEE_BPS: u32 = 1_000;

#[contract]
pub struct KeyLeaseRegistry;

#[contractimpl]
impl KeyLeaseRegistry {
    // -----------------------------------------------------------------------
    // Administration
    // -----------------------------------------------------------------------

    /// Initialize the registry and appoint `admin`.
    ///
    /// Can only be called once. The admin is the only account allowed to
    /// configure the escrow token via [`Self::set_token`] or upgrade the
    /// contract via [`Self::upgrade`].
    pub fn init(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        if storage::is_initialized(&env) {
            return Err(Error::AlreadyInitialized);
        }
        storage::set_admin(&env, &admin);
        storage::extend_instance_ttl(&env);
        RegistryInitialized {
            admin: admin.clone(),
        }
        .publish(&env);
        Ok(())
    }

    /// Configure the SEP-41 token used to escrow deposits. Admin only.
    ///
    /// Kept as a separate, admin-gated entry point so that deployments can point
    /// the registry at the native Stellar asset contract (SAC) or a custom SEP-41
    /// token without redeploying the registry logic.
    pub fn set_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;
        storage::set_token(&env, &token);
        storage::extend_instance_ttl(&env);
        EscrowTokenSet {
            admin: admin.clone(),
            token: token.clone(),
        }
        .publish(&env);
        Ok(())
    }

    /// Configure the protocol fee and its recipient. Admin only.
    ///
    /// `fee_bps` is in basis points (10,000 = 100%) and cannot exceed
    /// [`MAX_PROTOCOL_FEE_BPS`]. A zero fee may have no recipient; a positive
    /// fee requires one. The registry itself cannot be the recipient.
    pub fn set_protocol_fee(
        env: Env,
        admin: Address,
        fee_bps: u32,
        fee_recipient: Option<Address>,
    ) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        if fee_bps > MAX_PROTOCOL_FEE_BPS {
            return Err(Error::FeeTooHigh);
        }
        if fee_bps > 0 && fee_recipient.is_none() {
            return Err(Error::FeeRecipientNotSet);
        }
        if fee_recipient
            .as_ref()
            .is_some_and(|recipient| recipient == &env.current_contract_address())
        {
            return Err(Error::InvalidFeeRecipient);
        }

        storage::set_protocol_fee_bps(&env, fee_bps);
        storage::set_fee_recipient(&env, fee_recipient.clone());
        storage::extend_instance_ttl(&env);
        ProtocolFeeSet {
            admin,
            fee_bps,
            fee_recipient,
        }
        .publish(&env);
        Ok(())
    }

    /// Install a new registry Wasm through the Soroban deployer. Admin only.
    ///
    /// `new_wasm_hash` is the hash produced by `stellar contract upload`. The
    /// contract id stays the same; storage, balances and in-flight leases are
    /// preserved, which is why upgrades must never change the storage layout.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        ContractUpgraded {
            admin: admin.clone(),
            new_wasm_hash,
        }
        .publish(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Services
    // -----------------------------------------------------------------------

    /// Publish a new leasable service and return its `service_id`.
    ///
    /// `rate_per_call` is denominated in the escrow token's smallest unit and
    /// must be strictly positive. `endpoint_hash` commits to the off-chain
    /// endpoint descriptor.
    pub fn register_service(
        env: Env,
        provider: Address,
        rate_per_call: i128,
        endpoint_hash: BytesN<32>,
    ) -> Result<u64, Error> {
        provider.require_auth();

        if rate_per_call <= 0 {
            return Err(Error::InvalidRate);
        }

        let service_id = storage::next_service_id(&env)?;
        let service = Service {
            service_id,
            provider: provider.clone(),
            rate_per_call,
            is_active: true,
            endpoint_hash: endpoint_hash.clone(),
        };
        storage::save_service(&env, &service);
        storage::extend_instance_ttl(&env);
        ServiceRegistered {
            service_id,
            provider,
            rate_per_call,
            endpoint_hash,
        }
        .publish(&env);

        Ok(service_id)
    }

    /// Activate or deactivate a service. Only the owning provider may call this.
    ///
    /// Existing leases remain valid; deactivation only blocks *new* leases.
    pub fn set_service_active(
        env: Env,
        provider: Address,
        service_id: u64,
        is_active: bool,
    ) -> Result<(), Error> {
        provider.require_auth();

        let mut service = storage::get_service(&env, service_id)?;
        if service.provider != provider {
            return Err(Error::Unauthorized);
        }
        service.is_active = is_active;
        storage::save_service(&env, &service);
        ServiceStatusChanged {
            service_id,
            provider,
            is_active,
        }
        .publish(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Leases
    // -----------------------------------------------------------------------

    /// Lock a deposit and mint a time-bound lease.
    ///
    /// Transfers exactly `rate_per_call * requested_calls` of the escrow token
    /// from `consumer` into the registry, and records a lease that expires at
    /// `ledger.timestamp() + duration_secs`. Tokens with non-exact transfer
    /// semantics are rejected. Requires `consumer` auth.
    ///
    /// `duration_secs` must be in `1..=MAX_LEASE_DURATION_SECS`. The upper bound
    /// is a Soroban storage constraint: the lease's persistent entry is extended
    /// for the whole term plus the grace window, so a longer lease could be
    /// evicted while escrow is still locked.
    ///
    /// Returns the new `lease_id`.
    pub fn create_lease(
        env: Env,
        consumer: Address,
        service_id: u64,
        requested_calls: u32,
        duration_secs: u64,
    ) -> Result<u64, Error> {
        consumer.require_auth();

        if requested_calls == 0 {
            return Err(Error::InvalidCalls);
        }
        if duration_secs == 0 {
            return Err(Error::InvalidDuration);
        }
        if duration_secs > MAX_LEASE_DURATION_SECS {
            return Err(Error::DurationTooLong);
        }

        let service = storage::get_service(&env, service_id)?;
        if !service.is_active {
            return Err(Error::ServiceInactive);
        }

        // Escrow math is checked: an overflowing deposit must never be
        // silently truncated into a cheaper lease.
        let required_deposit = service
            .rate_per_call
            .checked_mul(requested_calls as i128)
            .ok_or(Error::Overflow)?;
        if required_deposit <= 0 {
            return Err(Error::InvalidRate);
        }

        // A lease with no token configured can never be funded or settled, so
        // fail before mutating state.
        let token_addr = storage::get_token(&env).ok_or(Error::TokenNotSet)?;

        let expires_at = env
            .ledger()
            .timestamp()
            .checked_add(duration_secs)
            .ok_or(Error::Overflow)?;

        let lease_id = storage::next_lease_id(&env)?;
        let lease = Lease {
            lease_id,
            service_id,
            consumer: consumer.clone(),
            allocated_calls: requested_calls,
            used_calls: 0,
            expires_at,
            locked_deposit: required_deposit,
            settled: false,
        };

        // checks-effects-interactions: the lease exists before the token is
        // called, and the whole transaction reverts if the transfer does.
        storage::save_lease(&env, &lease);
        storage::extend_instance_ttl(&env);
        transfer_exact(
            &env,
            &token_addr,
            &consumer,
            &env.current_contract_address(),
            &required_deposit,
        )?;

        LeaseCreated {
            lease_id,
            service_id,
            consumer,
            allocated_calls: requested_calls,
            expires_at,
            locked_deposit: required_deposit,
        }
        .publish(&env);

        Ok(lease_id)
    }

    /// Settle a lease, paying the provider for `verified_calls` consumed calls
    /// and refunding the remainder to the consumer. Requires provider auth.
    ///
    /// A lease may only be settled when it has **expired** or when
    /// `verified_calls == allocated_calls` (the consumer got everything it
    /// paid for, so there is no reason to wait). The protocol fee is floored
    /// from the amount earned for verified calls; the remainder is paid to the
    /// provider, and the unused deposit is returned in full. A lease can be
    /// settled once.
    pub fn settle_lease(
        env: Env,
        provider: Address,
        lease_id: u64,
        verified_calls: u32,
    ) -> Result<(), Error> {
        provider.require_auth();

        let mut lease = storage::get_lease(&env, lease_id)?;
        if lease.settled {
            return Err(Error::LeaseAlreadySettled);
        }
        if verified_calls > lease.allocated_calls {
            return Err(Error::InvalidCalls);
        }

        let service = storage::get_service(&env, lease.service_id)?;
        if service.provider != provider {
            return Err(Error::Unauthorized);
        }

        let now = env.ledger().timestamp();
        let fully_consumed = verified_calls == lease.allocated_calls;
        if now < lease.expires_at && !fully_consumed {
            return Err(Error::LeaseNotSettleable);
        }

        let earned = service
            .rate_per_call
            .checked_mul(verified_calls as i128)
            .ok_or(Error::Overflow)?;
        let refund = lease
            .locked_deposit
            .checked_sub(earned)
            .ok_or(Error::Overflow)?;
        let fee_bps = storage::protocol_fee_bps(&env);
        if fee_bps > MAX_PROTOCOL_FEE_BPS {
            return Err(Error::FeeTooHigh);
        }
        let protocol_fee = calculate_protocol_fee(earned, fee_bps)?;
        let payout = earned.checked_sub(protocol_fee).ok_or(Error::Overflow)?;
        let fee_recipient = if protocol_fee > 0 {
            Some(storage::fee_recipient(&env).ok_or(Error::FeeRecipientNotSet)?)
        } else {
            None
        };

        // checks-effects-interactions: persist the settlement before any token
        // transfer, so a re-entrant token sees a settled lease and reverts.
        lease.used_calls = verified_calls;
        lease.settled = true;
        storage::save_lease(&env, &lease);

        let token_addr = storage::get_token(&env).ok_or(Error::TokenNotSet)?;
        let contract = env.current_contract_address();

        if payout > 0 {
            transfer_exact(&env, &token_addr, &contract, &service.provider, &payout)?;
        }
        if let Some(recipient) = fee_recipient {
            transfer_exact(&env, &token_addr, &contract, &recipient, &protocol_fee)?;
        }
        if refund > 0 {
            transfer_exact(&env, &token_addr, &contract, &lease.consumer, &refund)?;
        }

        LeaseSettled {
            lease_id,
            provider,
            verified_calls,
            payout,
            protocol_fee,
            refund,
        }
        .publish(&env);

        Ok(())
    }

    /// Reclaim the full deposit for a lease the provider never settled.
    ///
    /// The lease must be expired and at least [`SETTLE_GRACE_PERIOD_SECS`]
    /// (24 hours) must have elapsed since expiry. Requires `consumer` auth.
    /// This is the consumer's escape hatch: a provider that goes offline cannot
    /// strand escrowed funds.
    pub fn revoke_expired_lease(env: Env, consumer: Address, lease_id: u64) -> Result<(), Error> {
        consumer.require_auth();

        let mut lease = storage::get_lease(&env, lease_id)?;
        if lease.consumer != consumer {
            return Err(Error::Unauthorized);
        }
        if lease.settled {
            return Err(Error::LeaseAlreadySettled);
        }

        let now = env.ledger().timestamp();
        if now < lease.expires_at {
            return Err(Error::LeaseNotExpired);
        }

        let revoke_at = lease
            .expires_at
            .checked_add(SETTLE_GRACE_PERIOD_SECS)
            .ok_or(Error::Overflow)?;
        if now < revoke_at {
            return Err(Error::RevokeWindowNotElapsed);
        }

        let refund = lease.locked_deposit;

        // checks-effects-interactions: mark the lease settled before the refund
        // transfer so a re-entrant token cannot reclaim twice.
        lease.settled = true;
        storage::save_lease(&env, &lease);

        if refund > 0 {
            let token_addr = storage::get_token(&env).ok_or(Error::TokenNotSet)?;
            transfer_exact(
                &env,
                &token_addr,
                &env.current_contract_address(),
                &consumer,
                &refund,
            )?;
        }

        LeaseRevoked {
            lease_id,
            consumer,
            refund,
        }
        .publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Read-only views
    // -----------------------------------------------------------------------

    /// Return the configured admin, or `NotInitialized` before `init`.
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        storage::get_admin(&env).ok_or(Error::NotInitialized)
    }

    /// Return the configured escrow token, or `TokenNotSet`.
    pub fn get_token(env: Env) -> Result<Address, Error> {
        storage::get_token(&env).ok_or(Error::TokenNotSet)
    }

    /// Return the configured protocol fee in basis points and its recipient.
    ///
    /// Older instances without fee configuration return `(0, None)`.
    pub fn get_protocol_fee(env: Env) -> (u32, Option<Address>) {
        (
            storage::protocol_fee_bps(&env),
            storage::fee_recipient(&env),
        )
    }

    /// Version string embedded in the Wasm by `contractmeta!`.
    pub fn version(env: Env) -> String {
        String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    /// Total number of services registered.
    pub fn service_count(env: Env) -> u64 {
        storage::service_count(&env)
    }

    /// Total number of leases ever created.
    pub fn lease_count(env: Env) -> u64 {
        storage::lease_count(&env)
    }

    /// Fetch a registered service.
    pub fn get_service(env: Env, service_id: u64) -> Result<Service, Error> {
        storage::get_service(&env, service_id)
    }

    /// Fetch a lease.
    pub fn get_lease(env: Env, lease_id: u64) -> Result<Lease, Error> {
        storage::get_lease(&env, lease_id)
    }

    /// Derive the lifecycle status of a lease at the current ledger time.
    pub fn lease_status(env: Env, lease_id: u64) -> Result<LeaseStatus, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        Ok(LeaseStatus::of(&lease, env.ledger().timestamp()))
    }

    /// Number of calls still available on a lease (zero once settled/expired).
    pub fn remaining_calls(env: Env, lease_id: u64) -> Result<u32, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        let now = env.ledger().timestamp();
        if lease.settled || now >= lease.expires_at {
            return Ok(0);
        }
        Ok(lease.allocated_calls.saturating_sub(lease.used_calls))
    }

    /// Escrow balance owed to a lease, in token stroops, at the current ledger time.
    pub fn locked_balance(env: Env, lease_id: u64) -> Result<i128, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        if lease.settled {
            return Ok(0);
        }
        Ok(lease.locked_deposit)
    }
}

/// Ensure `caller` matches the stored admin.
fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
    let admin = storage::get_admin(env).ok_or(Error::NotInitialized)?;
    if admin != *caller {
        return Err(Error::Unauthorized);
    }
    Ok(())
}

/// Move exactly `amount` according to both token balance views.
///
/// Any mismatch returns an error, causing the entire Soroban transaction
/// (including the token call and earlier storage writes) to roll back.
fn transfer_exact(
    env: &Env,
    token_addr: &Address,
    from: &Address,
    to: &Address,
    amount: &i128,
) -> Result<(), Error> {
    if *amount <= 0 || from == to {
        return Err(Error::InvalidTokenBehavior);
    }

    let client = token::Client::new(env, token_addr);
    let from_before = client.balance(from);
    let to_before = client.balance(to);
    client.transfer(from, to, amount);
    let expected_from = from_before
        .checked_sub(*amount)
        .ok_or(Error::InvalidTokenBehavior)?;
    let expected_to = to_before.checked_add(*amount).ok_or(Error::Overflow)?;

    if client.balance(from) != expected_from || client.balance(to) != expected_to {
        return Err(Error::InvalidTokenBehavior);
    }
    Ok(())
}

/// Calculate a basis-point fee with floor rounding and no overflowing
/// intermediate product.
fn calculate_protocol_fee(earned: i128, fee_bps: u32) -> Result<i128, Error> {
    if fee_bps > MAX_PROTOCOL_FEE_BPS {
        return Err(Error::FeeTooHigh);
    }
    if earned < 0 {
        return Err(Error::Overflow);
    }

    let denominator = BASIS_POINTS_DENOMINATOR as i128;
    let fee_bps = fee_bps as i128;
    let whole = (earned / denominator)
        .checked_mul(fee_bps)
        .ok_or(Error::Overflow)?;
    let fractional = (earned % denominator)
        .checked_mul(fee_bps)
        .ok_or(Error::Overflow)?
        .checked_div(denominator)
        .ok_or(Error::Overflow)?;
    whole.checked_add(fractional).ok_or(Error::Overflow)
}
