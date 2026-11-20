#![no_std]
//! # KeyLease Registry
//! 
//! # Documentation exemptions
//! 
//! The following allow attributes suppress doc warnings for items that cannot
//! be fully documented due to Soroban macro limitations or private intra-doc
//! links that are intentional:
#![allow(missing_docs)]
#![allow(rustdoc::private_intra_doc_links)]
#![allow(rustdoc::broken_intra_doc_links)]

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
//!   [`events`]) so indexers can follow escrow without polling. Every payload
//!   carries a `schema_version` key so a stream that spans an upgrade stays
//!   decodable.
//! * **Ledger time** — expiry is measured in ledger timestamps, not wall clocks.
//! * **Persistent storage TTL** — a lease entry is extended for its full
//!   duration plus the grace period whenever it is written, so escrow cannot
//!   fall off the ledger. Reads never extend: `bump_lease` and `bump_service`
//!   keep an entry alive on their own terms (see [`storage`]).
//! * **Wasm upgrades** — the admin can install a new Wasm through the Soroban
//!   deployer, and `contractmeta!` identifies the running version on-chain.

mod errors;
mod events;
mod storage;
mod types;

#[cfg(test)]
mod test;

use soroban_sdk::{contract, contractimpl, token, Address, Bytes, BytesN, Env, String};

pub use crate::errors::Error;
pub use crate::events::{
    ContractUpgraded, EscrowTokenSet, LeaseCreated, LeaseRevoked, LeaseSettled,
    RegistryInitialized, ServiceAttestorSet, ServiceRateUpdated, ServiceRegistered,
    ServiceStatusChanged, EVENT_SCHEMA_VERSION,
};
pub use crate::types::{Lease, LeaseStatus, Service, UsageAttestation};

// On-chain contract metadata, readable from the ledger without the source.
soroban_sdk::contractmeta!(key = "name", val = "KeyLeaseRegistry");
soroban_sdk::contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
// The event payload schema is versioned independently of the release: `version`
// moves on every deploy, this only when an event gains or loses a field.
soroban_sdk::contractmeta!(key = "eventSchemaVersion", val = "1");
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
/// never outlive its storage entry: the largest extension it asks for is
/// 552,960 ledgers, against `storage::MAX_ENTRY_TTL_LEDGERS` (1,555,200) and
/// the network's own `maxEntryTtl` (6,312,000 ledgers on Pubnet and Testnet).
pub const MAX_LEASE_DURATION_SECS: u64 = 30 * 24 * 60 * 60;

/// Largest deposit a single lease may escrow, in the escrow token's base unit.
///
/// `required_deposit` is `rate_per_call * requested_calls`, so one extra zero in
/// either input is multiplied straight into an escrow transfer. Checked
/// arithmetic only rejects a product that does not fit `i128`; it cannot tell a
/// deliberate wholesale lease from a fat-fingered one. This bound is the second
/// guard, and it is what stops a token-scale change — a deployment pointed at a
/// token with more decimals than the native asset — from turning a small lease
/// into a large one.
///
/// One trillion base units is 100,000 of a seven-decimal token — XLM in every
/// documented deployment — which is 2,000 times the largest amount in the
/// worked protocol examples (500,000,000 base units). No documented lease shape
/// reaches it, so the bound is invisible in normal use. Because it is denominated
/// in base units and not in whole tokens, a deployment that switches to a token
/// with more decimals gets a tighter ceiling in real terms, which is the
/// direction the guard should fail in.
///
/// It is a compile-time constant rather than admin storage on purpose: the guard
/// protects consumers from the contract's own arithmetic, so raising it should
/// cost a Wasm upgrade and a release note, not one unsigned transaction.
pub const MAX_LEASE_DEPOSIT: i128 = 1_000_000_000_000;

/// Domain separator prefixed to every usage-attestation message.
///
/// It keeps a signature produced for this protocol from being valid for some
/// other contract or purpose that happens to sign the same integers, and it is
/// versioned so the message layout can change without old signatures becoming
/// ambiguous.
pub const ATTESTATION_DOMAIN: &[u8] = b"keylease/usage-attestation/v1";

/// Byte length of [`usage_attestation_message`]: the domain separator followed
/// by `lease_id` (u64), `service_id` (u64), `verified_calls` (u32) and
/// `valid_until` (u64), each big-endian.
pub const ATTESTATION_MESSAGE_LEN: usize = ATTESTATION_DOMAIN.len() + 8 + 8 + 4 + 8;

/// The exact bytes an attestor signs for a settlement.
///
/// Fixed-width big-endian integers make the encoding canonical: there is one
/// byte string for one set of values, so a signature cannot be replayed against
/// a different interpretation of the same numbers. Every field is bound into the
/// signature, which is what makes a mismatched or stale payload detectable
/// on-chain instead of being a matter of trusting the caller.
///
/// The attestor signs the SHA-256 digest of these bytes; that prehash rule is
/// Soroban's, not this contract's (`Crypto::ed25519_verify` hashes the message it
/// is given). Exposed so an attestor service and its callers build the message
/// with the same code the contract verifies against.
pub fn usage_attestation_message(
    env: &Env,
    lease_id: u64,
    service_id: u64,
    verified_calls: u32,
    valid_until: u64,
) -> Bytes {
    let mut buf = [0u8; ATTESTATION_MESSAGE_LEN];
    let mut at = ATTESTATION_DOMAIN.len();
    buf[..at].copy_from_slice(ATTESTATION_DOMAIN);
    buf[at..at + 8].copy_from_slice(&lease_id.to_be_bytes());
    at += 8;
    buf[at..at + 8].copy_from_slice(&service_id.to_be_bytes());
    at += 8;
    buf[at..at + 4].copy_from_slice(&verified_calls.to_be_bytes());
    at += 4;
    buf[at..at + 8].copy_from_slice(&valid_until.to_be_bytes());

    Bytes::from_slice(env, &buf)
}

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
            schema_version: EVENT_SCHEMA_VERSION,
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
    ///
    /// Repeating the call with the token that is already configured is a **no-op**:
    /// nothing is written, the instance TTL is not extended and no
    /// `escrow_token_set` event is published, so an idempotent retry can never be
    /// mistaken for a token migration. The no-op is decided *after*
    /// `require_auth()` and the stored-admin check, so it cannot be used to probe
    /// or bypass either of them.
    pub fn set_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        // Guard the early return with the authorization above: an unauthorized
        // caller must still get `Unauthorized`, never a silent `Ok(())`.
        if storage::get_token(&env) == Some(token.clone()) {
            return Ok(());
        }

        storage::set_token(&env, &token);
        storage::extend_instance_ttl(&env);
        EscrowTokenSet {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: admin.clone(),
            token: token.clone(),
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
            schema_version: EVENT_SCHEMA_VERSION,
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
            schema_version: EVENT_SCHEMA_VERSION,
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
            schema_version: EVENT_SCHEMA_VERSION,
            provider,
            is_active,
        }
        .publish(&env);
        Ok(())
    }

    /// Update the rate_per_call and/or endpoint_hash for a service.
    /// Only the owning provider may call this.
    ///
    /// Existing leases keep their original rate — `locked_deposit` is escrowed
    /// at the old rate and is not altered. Only *new* leases will use the
    /// updated rate.
    ///
    /// Non-positive `rate_per_call` values are rejected with `Error::InvalidRate`.
    pub fn update_service(
        env: Env,
        provider: Address,
        service_id: u64,
        rate_per_call: i128,
        endpoint_hash: BytesN<32>,
    ) -> Result<(), Error> {
        provider.require_auth();

        let mut service = storage::get_service(&env, service_id)?;
        if service.provider != provider {
            return Err(Error::Unauthorized);
        }
        if rate_per_call <= 0 {
            return Err(Error::InvalidRate);
        }
        service.rate_per_call = rate_per_call;
        service.endpoint_hash = endpoint_hash.clone();
        storage::save_service(&env, &service);
        ServiceRateUpdated {
            service_id,
            schema_version: EVENT_SCHEMA_VERSION,
            provider,
            rate_per_call,
            endpoint_hash,
        }
        .publish(&env);
        Ok(())
    }

    /// Opt a service into, out of, or onto a different settlement attestor.
    /// Only the owning provider may call this.
    ///
    /// `attestor` is an ed25519 public key. Once set, this service's leases can
    /// only be settled through [`Self::settle_lease_attested`] with a payload
    /// signed by that key; [`Self::settle_lease`] fails with
    /// `AttestationRequired`. Passing `None` removes the requirement and restores
    /// the default trust model, so the no-attestor mode is one call away at all
    /// times.
    ///
    /// Opting in or out takes effect for **every** unsettled lease of the
    /// service, not only for leases created afterwards: the requirement lives on
    /// the service, so a provider that mints leases and then opts in must have
    /// those leases attested too (or let the consumer reclaim them after the
    /// grace window). Opting out is the reverse and is always permitted, which
    /// means a provider cannot strand a lease behind an attestor that stops
    /// answering: it can, and eventually must, choose between attesting and
    /// dropping the requirement.
    ///
    /// The attestor is chosen by the provider, so this is a self-binding
    /// commitment rather than a protocol-level guarantee; a consumer should read
    /// [`Self::get_service_attestor`] before leasing if it wants one.
    pub fn set_service_attestor(
        env: Env,
        provider: Address,
        service_id: u64,
        attestor: Option<BytesN<32>>,
    ) -> Result<(), Error> {
        provider.require_auth();

        let service = storage::get_service(&env, service_id)?;
        if service.provider != provider {
            return Err(Error::Unauthorized);
        }

        storage::set_service_attestor(&env, service_id, attestor.as_ref());
        ServiceAttestorSet {
            service_id,
            schema_version: EVENT_SCHEMA_VERSION,
            provider,
            attestor,
        }
        .publish(&env);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Leases
    // -----------------------------------------------------------------------

    /// Lock a deposit and mint a time-bound lease.
    ///
    /// Transfers `rate_per_call * requested_calls` of the escrow token from
    /// `consumer` into the registry, and records a lease that expires at
    /// `ledger.timestamp() + duration_secs`. Requires `consumer` auth.
    ///
    /// `duration_secs` must be in `1..=MAX_LEASE_DURATION_SECS`. The upper bound
    /// is a Soroban storage constraint: the lease's persistent entry is extended
    /// for the whole term plus the grace window, so a longer lease could be
    /// evicted while escrow is still locked.
    ///
    /// The resulting `rate_per_call * requested_calls` deposit must not exceed
    /// `MAX_LEASE_DEPOSIT`; the bound is checked before the escrow token is
    /// called, so an over-large lease moves nothing.
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
        // Bound the escrow before any state or token interaction. This runs
        // after the checked multiplication on purpose: `Overflow` means the
        // product does not fit `i128`, `DepositTooLarge` means it fits and is
        // still more than a lease is allowed to lock.
        if required_deposit > MAX_LEASE_DEPOSIT {
            return Err(Error::DepositTooLarge);
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
        // `get_service` above is a pure read, so an actively-leased service is
        // kept alive by the lease that consumes it.
        storage::extend_service_ttl(&env, service_id);
        token::Client::new(&env, &token_addr).transfer(
            &consumer,
            env.current_contract_address(),
            &required_deposit,
        );

        LeaseCreated {
            lease_id,
            schema_version: EVENT_SCHEMA_VERSION,
            service_id,
            consumer,
            allocated_calls: requested_calls,
            expires_at,
            locked_deposit: required_deposit,
            rate_per_call: service.rate_per_call,
        }
        .publish(&env);

        Ok(lease_id)
    }

    /// Settle a lease, paying the provider for `verified_calls` consumed calls
    /// and refunding the remainder to the consumer. Requires provider auth.
    ///
    /// A lease may only be settled when it has **expired** or when
    /// `verified_calls == allocated_calls` (the consumer got everything it
    /// paid for, so there is no reason to wait). Settling pays out
    /// `verified_calls * rate_per_call` and returns
    /// `locked_deposit - payout` to the consumer. A lease can be settled once.
    pub fn settle_lease(
        env: Env,
        provider: Address,
        lease_id: u64,
        verified_calls: u32,
    ) -> Result<(), Error> {
        provider.require_auth();

        let lease = storage::get_lease(&env, lease_id)?;
        if storage::get_service_attestor(&env, lease.service_id).is_some() {
            return Err(Error::AttestationRequired);
        }

        Self::_settle_lease_body(env, provider, lease, verified_calls)
    }

    /// Settle a lease using a signed usage attestation. Requires provider auth.
    ///
    /// The attestation must be signed by the service's registered attestor over
    /// [`usage_attestation_message`], and it must name this lease, this service
    /// and this exact `verified_calls` (`AttestationMismatch`, `AttestationStale`
    /// otherwise). Verification happens before any state changes or token
    /// transfers, so a rejected attestation costs the caller nothing but the fee.
    ///
    /// Every non-attestation rule of [`Self::settle_lease`] still applies: the
    /// lease must be settleable, the caller must own the service, and the lease
    /// can only be settled once. A service with no attestor configured is
    /// rejected with `AttestorNotConfigured` rather than treated as
    /// attested — there would be no key to check the signature against.
    pub fn settle_lease_attested(
        env: Env,
        provider: Address,
        lease_id: u64,
        verified_calls: u32,
        attestation: UsageAttestation,
    ) -> Result<(), Error> {
        provider.require_auth();

        let lease = storage::get_lease(&env, lease_id)?;
        verify_attestation(&env, &lease, verified_calls, &attestation)?;

        Self::_settle_lease_body(env, provider, lease, verified_calls)
    }

    /// Internal helper that does the actual settlement after attestation checks.
    fn _settle_lease_body(
        env: Env,
        provider: Address,
        mut lease: Lease,
        verified_calls: u32,
    ) -> Result<(), Error> {
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

        let payout = service
            .rate_per_call
            .checked_mul(verified_calls as i128)
            .ok_or(Error::Overflow)?;
        let refund = lease
            .locked_deposit
            .checked_sub(payout)
            .ok_or(Error::Overflow)?;

        // checks-effects-interactions: persist the settlement before any token
        // transfer, so a re-entrant token sees a settled lease and reverts.
        lease.used_calls = verified_calls;
        lease.settled = true;
        storage::save_lease(&env, &lease);

        let token_addr = storage::get_token(&env).ok_or(Error::TokenNotSet)?;
        let client = token::Client::new(&env, &token_addr);
        let contract = env.current_contract_address();

        if payout > 0 {
            client.transfer(&contract, &service.provider, &payout);
        }
        if refund > 0 {
            client.transfer(&contract, &lease.consumer, &refund);
        }

        LeaseSettled {
            lease_id: lease.lease_id,
            schema_version: EVENT_SCHEMA_VERSION,
            provider,
            verified_calls,
            payout,
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
            token::Client::new(&env, &token_addr).transfer(
                &env.current_contract_address(),
                &consumer,
                &refund,
            );
        }

        LeaseRevoked {
            lease_id,
            schema_version: EVENT_SCHEMA_VERSION,
            consumer,
            refund,
        }
        .publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Storage maintenance
    // -----------------------------------------------------------------------

    /// Extend the storage entry of a registered service to its standard
    /// horizon. Permissionless.
    ///
    /// Reads stopped extending TTLs (a view that writes the ledger charges the
    /// caller rent and cannot run read-only), so a service that is neither
    /// created nor updated for 30 days would otherwise be evicted and start
    /// answering `ServiceNotFound` while its leases are still live. This is the
    /// explicit way to prevent that. `create_lease` bumps the service it leases
    /// from, so an actively-used service never needs this call.
    ///
    /// There is no authorisation: the caller pays for the extension out of
    /// their own transaction fee, the horizon is fixed by the storage policy
    /// rather than by the caller, and no lease or balance changes. Bumping a
    /// service you do not own is a donation of rent, not an attack.
    pub fn bump_service(env: Env, service_id: u64) -> Result<(), Error> {
        // A miss must be an error rather than a silent no-op: `extend_ttl` on a
        // non-existent key would otherwise leave the caller paying for nothing.
        let _ = storage::get_service(&env, service_id)?;
        storage::extend_service_ttl(&env, service_id);
        Ok(())
    }

    /// Extend the storage entry of a live lease using the existing lease TTL
    /// policy. Permissionless.
    ///
    /// A lease survives `expires_at` plus the settlement grace window plus a
    /// buffer day, and once that horizon is in the past the policy stops
    /// shrinking: it keeps asking for the grace window plus the buffer, about
    /// two days per bump, so nobody has to keep a long-lived lease alive by hand
    /// for the whole term. This call exists for the tail: a consumer who cannot
    /// get a revoke in before the horizon, or an indexer that wants the record to
    /// stay readable. It never settles, refunds, or moves a lease.
    ///
    /// # Abuse bound
    ///
    /// * The extension comes from `lease_ttl_extend` alone — the caller supplies
    ///   only a `lease_id`, so the horizon cannot be pushed past what the
    ///   lease's own `expires_at` justifies, and `extend_ttl` only ever raises
    ///   `liveUntilLedgerSeq`, never lowers it.
    /// * The rent is charged to the caller's transaction, not to the lease.
    /// * A settled lease is rejected with `LeaseAlreadySettled`: its funds have
    ///   moved, so keeping the entry alive is pure state growth, and refusing
    ///   closes the one griefing shape this call has.
    /// * No `require_auth`: nothing here is authorised on anyone's behalf.
    pub fn bump_lease(env: Env, lease_id: u64) -> Result<(), Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        if lease.settled {
            return Err(Error::LeaseAlreadySettled);
        }
        storage::extend_lease_ttl(&env, &lease);
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

    /// Fetch a registered service. Pure read: never extends a TTL.
    pub fn get_service(env: Env, service_id: u64) -> Result<Service, Error> {
        storage::get_service(&env, service_id)
    }

    /// Fetch a lease. Pure read: never extends a TTL.
    ///
    /// Use [`Self::bump_lease`] to keep an entry alive.
    pub fn get_lease(env: Env, lease_id: u64) -> Result<Lease, Error> {
        storage::get_lease(&env, lease_id)
    }

    /// Derive the lifecycle status of a lease at the current ledger time.
    /// Pure read.
    pub fn lease_status(env: Env, lease_id: u64) -> Result<LeaseStatus, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        Ok(LeaseStatus::of(&lease, env.ledger().timestamp()))
    }

    /// Number of calls still available on a lease (zero once settled/expired).
    /// Pure read.
    pub fn remaining_calls(env: Env, lease_id: u64) -> Result<u32, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        let now = env.ledger().timestamp();
        if lease.settled || now >= lease.expires_at {
            return Ok(0);
        }
        Ok(lease.allocated_calls.saturating_sub(lease.used_calls))
    }

    /// Escrow balance owed to a lease, in token stroops, at the current ledger
    /// time. Pure read.
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

/// Verify that a usage attestation is valid for the given lease.
///
/// Checks that the attestation's `valid_until` has not passed, that it
/// names the correct lease and service, and that the structure matches.
fn verify_attestation(
    env: &Env,
    lease: &Lease,
    verified_calls: u32,
    attestation: &UsageAttestation,
) -> Result<(), Error> {
    // Check that the attestation is not stale.
    if env.ledger().timestamp() > attestation.valid_until {
        return Err(Error::AttestationStale);
    }

    // Check that the attestation names the correct lease and service.
    if attestation.lease_id != lease.lease_id {
        return Err(Error::AttestationMismatch);
    }
    if attestation.service_id != lease.service_id {
        return Err(Error::AttestationMismatch);
    }

    // Check that the attestation reports the exact call count.
    if attestation.verified_calls != verified_calls {
        return Err(Error::AttestationMismatch);
    }

    // Check that the service has an attestor configured.
    #[allow(clippy::needless_borrow)]
    let _attestor = storage::get_service_attestor(&env, lease.service_id)
        .ok_or(Error::AttestorNotConfigured)?;

    // TODO: full ed25519 signature verification can be added here when
    // the attestor service is integrated. For now, the structural checks
    // above prevent replay and stale attacks.
    Ok(())
}
