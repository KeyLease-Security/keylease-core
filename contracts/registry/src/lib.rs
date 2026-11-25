#![no_std]
// Documentation coverage (CONTRIBUTING.md#text-and-documentation-checks).
// `missing_docs` cannot simply be on: the Soroban `#[contract*]` macros emit
// public items (`spec_xdr`, event `publish`, the client's `new`/`env`/`address`)
// that carry no docs and cannot be given any, and a lint level cannot be scoped
// to one macro expansion. scripts/check-missing-docs.sh builds with this cfg and
// fails on every finding outside those expansions; the Rustdoc job runs it.
#![cfg_attr(keylease_doc_lint, warn(missing_docs))]
// No unsafe code in contract crates (CONTRIBUTING.md#unsafe-code-and-std).
// `forbid` on the deployed Wasm. On the host, soroban-sdk's `testutils`
// feature registers contracts through `ctor`, whose expansion carries its own
// `#[allow(unsafe_code)]`, which `forbid` rejects; `deny` still rejects any
// unsafe written in this crate. scripts/check-no-std.sh checks these lines.
#![cfg_attr(target_family = "wasm", forbid(unsafe_code))]
#![cfg_attr(not(target_family = "wasm"), deny(unsafe_code))]

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
//!     |-- settle_lease(used) ----->|  propose payout + refund       |
//!     |                            |  (24h challenge window opens)  |
//!                                  |<-- challenge_settlement ------ |
//!                                  |-- resolve_challenge (admin) -->|
//!     |                            |  finalize after window         |
//!                                  |<-- request_early_cancel ------- |
//!     |-- approve/reject --------->|  consent releases unused funds |
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
//!   settle within [`SETTLE_GRACE_PERIOD_SECS`] of expiry. A reclaimed lease
//!   ends as `LeaseStatus::Revoked`, so a refund is never mistaken for revenue.
//! * A settlement does not pay out immediately: the consumer has
//!   [`CHALLENGE_WINDOW_SECS`] to dispute the reported call count, and the
//!   admin resolves a raised challenge.
//! * A published price is bounded by [`MAX_RATE_PER_CALL`], which equals the
//!   per-lease deposit cap, so the deposit a consumer is quoted can always be
//!   computed and the cheapest lease of any listed service can always be funded.
//! * A lease can be funded by a delegate, but only with the consumer's own
//!   signature, and the deposit and the refund stay on the consumer.
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
//!   [`LeaseCreated`]) so indexers can follow escrow without polling. Every payload
//!   carries a `schema_version` key so a stream that spans an upgrade stays
//!   decodable.
//! * **Ledger time** — expiry is measured in ledger timestamps, not wall clocks.
//! * **Persistent storage TTL** — a lease entry is extended for its full
//!   duration plus the grace period whenever it is written, so escrow cannot
//!   fall off the ledger. Reads never extend: `bump_lease` and `bump_service`
//!   keep an entry alive on their own terms (see `storage`).
//! * **Two-step admin rotation** — the admin role moves only when the current
//!   admin proposes a successor and that successor authorizes acceptance, so a
//!   typo or an unreachable address can never strand upgrade and token control.
//! * **Wasm upgrades** — the admin proposes a new Wasm through the Soroban
//!   deployer and may only execute it after a 24-hour on-ledger delay, giving
//!   consumers time to exit; `contractmeta!` identifies the running version.
//!   The storage schema is versioned: state-changing calls stop until the
//!   admin runs `migrate` after an upgrade, and a downgrade is rejected.

mod errors;
mod events;
mod storage;
mod types;

#[cfg(test)]
mod test;

#[cfg(test)]
mod test_abi;

#[cfg(test)]
mod test_wasm;

use soroban_sdk::{contract, contractimpl, token, Address, Bytes, BytesN, Env, String};

pub use crate::errors::Error;
pub use crate::events::{
    AdminTransferAccepted, AdminTransferCanceled, AdminTransferProposed, ContractUpgraded,
    EscrowTokenSet, LeaseCancellationRejected, LeaseCancellationRequested,
    LeaseCancellationWithdrawn, LeaseCancelled, LeaseChallengeCancelled, LeaseChallengeResolved,
    LeaseChallenged, LeaseCreated, LeaseDelegated, LeaseRevoked, LeaseSettled,
    LeaseSettlementProposed, LeaseTransferred, RegistryInitialized, SchemaMigrated,
    ServiceAttestorSet, ServiceRateUpdated, ServiceRegistered, ServiceStatusChanged,
    UpgradeCancelled, UpgradeProposed, EVENT_SCHEMA_VERSION,
};
pub use crate::storage::SCHEMA_VERSION;
pub use crate::types::{
    CancelRequest, Lease, LeasePage, LeaseQuote, LeaseStatus, PendingAdminTransfer, PendingUpgrade,
    ProviderServicePage, Service, ServicePage, ServiceStats, Settlement, SettlementState,
    UsageAttestation,
};

// On-chain contract metadata, readable from the ledger without the source.
soroban_sdk::contractmeta!(key = "name", val = "KeyLeaseRegistry");
soroban_sdk::contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
// The event payload schema is versioned independently of the release: `version`
// moves on every deploy, this only when an event gains or loses a field.
soroban_sdk::contractmeta!(key = "eventSchemaVersion", val = "1");
// The storage schema this Wasm reads and writes (`storage::SCHEMA_VERSION`),
// readable before the Wasm is installed, so a reviewer can tell from the
// uploaded artifact whether an upgrade will need `migrate`.
soroban_sdk::contractmeta!(key = "storageSchemaVersion", val = "1");
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

/// Mandatory wait between proposing and executing a Wasm upgrade.
///
/// The delay is the consumers' exit window: a proposal is public for 24 hours
/// on-ledger before any new rules can take effect, so a lease created under the
/// current code can be settled before the contract changes underneath it.
pub const UPGRADE_DELAY_SECS: u64 = 24 * 60 * 60;

/// Window after a settlement is proposed during which the consumer may dispute
/// the provider's reported call count. The payout stays locked until it closes.
pub const CHALLENGE_WINDOW_SECS: u64 = 24 * 60 * 60;

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

/// Highest per-call price a provider may publish, in the escrow token's
/// smallest unit. It is deliberately set to [`MAX_LEASE_DEPOSIT`].
///
/// Two properties follow from that equality and both are test-pinned:
///
/// 1. **No listing can price itself out of the market.** The cheapest lease of
///    a service costs one call at `rate_per_call`, so a rate at or below the
///    deposit cap is always fundable, and a rate above it describes a service
///    nobody could ever rent. Reverting at registration is honest feedback
///    rather than a lease-time failure.
/// 2. **Deposit arithmetic stops being a surprise.** Before this bound the only
///    ceiling on a rate was `checked_mul` failing deep inside `create_lease`
///    with `Overflow`, which a consumer cannot predict from a listing. With the
///    bound, the worst legal product is `MAX_LEASE_DEPOSIT * u32::MAX`, about
///    4.3e21 and below 2^72 — inside `i128` (2^127) by roughly 10^17.
///
/// `create_lease` and `settle_lease` keep their own `checked_mul` guards: those
/// cover the deposit cap and the paths this constant does not bound, not
/// pricing.
pub const MAX_RATE_PER_CALL: i128 = MAX_LEASE_DEPOSIT;

/// Largest page [`KeyLeaseRegistry::services_for_provider`], [`KeyLeaseRegistry::get_services`]
/// and [`KeyLeaseRegistry::get_leases`] will serve in one call.
///
/// Every entry on a page costs one ledger read, so an unbounded `limit` would let
/// a single call read a provider's whole index or catalogue and blow the transaction budget.
///
/// Asking for more is rejected with [`Error::InvalidLimit`] rather than
/// silently clamped, so a caller cannot mistake a truncated page for the whole
/// answer.
pub const MAX_PAGE_SIZE: u32 = 100;

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
/// The attestor signs these bytes directly using Ed25519. Soroban's
/// `Crypto::ed25519_verify` verifies the supplied message as-is, so callers
/// must not pre-hash it. Exposed so an attestor service and its callers build
/// the message with the same code the contract verifies against.
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

/// How long a proposed admin transfer stays acceptable.
///
/// Long enough for a multisig successor to collect signatures across time zones,
/// short enough that a forgotten proposal does not sit around as a latent
/// hand-over to an address whose key may since have leaked. After the window the
/// proposal can only be cancelled or replaced, never accepted.
pub const ADMIN_TRANSFER_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;

/// The KeyLease registry contract: services, escrowed leases and settlement.
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
    /// configure the escrow token via [`KeyLeaseRegistry::set_token`] or manage upgrades via
    /// [`KeyLeaseRegistry::propose_upgrade`], [`KeyLeaseRegistry::execute_upgrade`] and
    /// [`KeyLeaseRegistry::cancel_upgrade`].
    /// The role can later be handed over with [`KeyLeaseRegistry::propose_admin`]
    /// and [`KeyLeaseRegistry::accept_admin`].
    pub fn init(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        // A stamped schema without an admin can only come from foreign or
        // corrupted state; refuse it rather than adopt data we did not write.
        if storage::is_initialized(&env) || storage::has_schema_version(&env) {
            return Err(Error::AlreadyInitialized);
        }
        storage::set_admin(&env, &admin);
        storage::set_schema_version(&env, SCHEMA_VERSION);
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
        storage::check_schema(&env)?;

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

    /// Queue a Wasm upgrade behind the mandatory [`UPGRADE_DELAY_SECS`] delay.
    ///
    /// Admin only. `new_wasm_hash` is the hash produced by
    /// `stellar contract upload`. A new proposal replaces a pending one and
    /// restarts the delay, so the exit window can never be shortened. The hash
    /// and the execution ledger timestamp are stored and reported by
    /// [`UpgradeProposed`]; clients can read them with
    /// [`KeyLeaseRegistry::get_pending_upgrade`].
    ///
    /// The upgrade entry points are deliberately **not** gated on the schema
    /// check: if an upgrade installs the wrong Wasm (for example an older
    /// schema, which stops the registry with `SchemaVersionUnsupported`), the
    /// admin must still be able to propose and execute the right one. After an
    /// upgrade that raises the schema, every state-changing entry point returns
    /// `SchemaMigrationRequired` until [`KeyLeaseRegistry::migrate`] runs.
    pub fn propose_upgrade(
        env: Env,
        admin: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        let execute_after = env
            .ledger()
            .timestamp()
            .checked_add(UPGRADE_DELAY_SECS)
            .ok_or(Error::Overflow)?;
        storage::set_pending_upgrade(
            &env,
            &PendingUpgrade {
                new_wasm_hash: new_wasm_hash.clone(),
                execute_after,
            },
        );
        storage::extend_instance_ttl(&env);
        UpgradeProposed {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: admin.clone(),
            new_wasm_hash,
            execute_after,
        }
        .publish(&env);
        Ok(())
    }

    /// Drop a pending upgrade so it can never be executed. Admin only.
    ///
    /// Emits [`UpgradeCancelled`] with the abandoned hash. Cancelling when no
    /// proposal is pending fails with [`Error::NoPendingUpgrade`].
    pub fn cancel_upgrade(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        let pending = storage::get_pending_upgrade(&env).ok_or(Error::NoPendingUpgrade)?;
        storage::remove_pending_upgrade(&env);
        storage::extend_instance_ttl(&env);
        UpgradeCancelled {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: admin.clone(),
            new_wasm_hash: pending.new_wasm_hash,
        }
        .publish(&env);
        Ok(())
    }

    /// Install the pending Wasm, but only once the delay has elapsed. Admin only.
    ///
    /// Before execution, `ledger.timestamp()` must have reached the
    /// `execute_after` recorded at proposal time; an early call fails with
    /// [`Error::UpgradeDelayNotElapsed`]. The proposal is consumed atomically
    /// with the install and [`ContractUpgraded`] reports both the installed hash
    /// and the ledger timestamp that had to be reached.
    ///
    /// The contract id stays the same; storage, balances and in-flight leases
    /// are preserved, which is why a layout change must bump [`SCHEMA_VERSION`]
    /// and ship a migration.
    pub fn execute_upgrade(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        let pending = storage::get_pending_upgrade(&env).ok_or(Error::NoPendingUpgrade)?;
        if env.ledger().timestamp() < pending.execute_after {
            return Err(Error::UpgradeDelayNotElapsed);
        }

        // Persist the consumed proposal and publish the event before handing
        // control to the deployer. A failing install reverts the whole
        // transaction, so no event or storage change can outlive it.
        storage::remove_pending_upgrade(&env);
        storage::extend_instance_ttl(&env);
        ContractUpgraded {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: admin.clone(),
            new_wasm_hash: pending.new_wasm_hash.clone(),
            execute_after: pending.execute_after,
        }
        .publish(&env);
        env.deployer()
            .update_current_contract_wasm(pending.new_wasm_hash);
        Ok(())
    }

    /// Migrate stored data to [`SCHEMA_VERSION`] and lift the post-upgrade
    /// stop. Admin only. Returns the schema version now in force.
    ///
    /// Runs every step from the stored version up to the code's version, then
    /// stamps the new version, all in one transaction: either the whole path
    /// applies or nothing does. Steps are bounded (see `storage::migrate_step`),
    /// so a migration never iterates over leases or services.
    ///
    /// - **Auth:** `admin`, which must be the stored admin.
    /// - **Already current:** no-op; nothing is written and no event is
    ///   published, so it is safe to call after every upgrade.
    /// - **Stored schema newer than this Wasm:** `SchemaVersionUnsupported`;
    ///   the upgrade was a downgrade and must be rolled back, not migrated.
    /// - **Events:** `schema_migrated { admin, from_version, to_version }`.
    /// - **Errors:** `NotInitialized`, `Unauthorized`,
    ///   `SchemaVersionUnsupported`.
    pub fn migrate(env: Env, admin: Address) -> Result<u32, Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;
        let from = storage::stored_schema_version(&env).ok_or(Error::NotInitialized)?;
        if from == SCHEMA_VERSION {
            return Ok(SCHEMA_VERSION);
        }
        if from > SCHEMA_VERSION {
            return Err(Error::SchemaVersionUnsupported);
        }

        // Bounded by the compile-time distance between two schema versions.
        for version in from..SCHEMA_VERSION {
            storage::migrate_step(&env, version)?;
        }
        storage::set_schema_version(&env, SCHEMA_VERSION);
        storage::extend_instance_ttl(&env);
        SchemaMigrated {
            schema_version: EVENT_SCHEMA_VERSION,
            admin,
            from_version: from,
            to_version: SCHEMA_VERSION,
        }
        .publish(&env);
        Ok(SCHEMA_VERSION)
    }

    // -----------------------------------------------------------------------
    // Admin rotation
    // -----------------------------------------------------------------------

    /// Propose `new_admin` as the next registry admin and return the ledger
    /// timestamp at which the proposal expires.
    ///
    /// This is the first half of a two-step hand-over; nothing changes until
    /// `new_admin` calls [`KeyLeaseRegistry::accept_admin`] before `expires_at`
    /// (`now + ADMIN_TRANSFER_WINDOW_SECS`). Splitting the hand-over means the
    /// current admin's authorization alone can never move the role, and the
    /// successor's authorization alone can never claim it.
    ///
    /// - **Auth:** `admin`, which must be the stored admin.
    /// - **Preconditions:** `new_admin` differs from the current admin.
    /// - **Effects:** stores (or replaces) the single pending proposal and
    ///   extends the instance TTL.
    /// - **Events:** `admin_transfer_proposed`.
    /// - **Errors:** `NotInitialized`, `Unauthorized`, `InvalidAdmin`, `Overflow`,
    ///   `SchemaMigrationRequired`, `SchemaVersionUnsupported`.
    pub fn propose_admin(env: Env, admin: Address, new_admin: Address) -> Result<u64, Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;
        storage::check_schema(&env)?;
        if new_admin == admin {
            return Err(Error::InvalidAdmin);
        }

        let now = env.ledger().timestamp();
        let expires_at = now
            .checked_add(ADMIN_TRANSFER_WINDOW_SECS)
            .ok_or(Error::Overflow)?;
        storage::set_pending_admin(
            &env,
            &PendingAdminTransfer {
                proposed_by: admin.clone(),
                new_admin: new_admin.clone(),
                proposed_at: now,
                expires_at,
            },
        );
        storage::extend_instance_ttl(&env);
        AdminTransferProposed {
            schema_version: EVENT_SCHEMA_VERSION,
            admin,
            new_admin,
            expires_at,
        }
        .publish(&env);
        Ok(expires_at)
    }

    /// Withdraw the pending admin transfer. Admin only.
    ///
    /// Works whether or not the proposal has expired, so a stale proposal can
    /// always be cleared explicitly.
    ///
    /// - **Auth:** `admin`, which must be the stored admin.
    /// - **Preconditions:** a proposal is pending.
    /// - **Effects:** removes the proposal and extends the instance TTL.
    /// - **Events:** `admin_transfer_canceled`.
    /// - **Errors:** `NotInitialized`, `Unauthorized`, `NoPendingAdminTransfer`,
    ///   `SchemaMigrationRequired`, `SchemaVersionUnsupported`.
    pub fn cancel_admin_transfer(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;
        storage::check_schema(&env)?;
        let pending = storage::get_pending_admin(&env).ok_or(Error::NoPendingAdminTransfer)?;

        storage::clear_pending_admin(&env);
        storage::extend_instance_ttl(&env);
        AdminTransferCanceled {
            schema_version: EVENT_SCHEMA_VERSION,
            admin,
            new_admin: pending.new_admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Accept a pending admin transfer and become the registry admin.
    ///
    /// The second half of the hand-over started by [`KeyLeaseRegistry::propose_admin`].
    /// Together the two calls require authorization from both the outgoing and
    /// the incoming admin, so no third party can take over the registry, and a
    /// mistyped or unreachable successor simply never accepts.
    ///
    /// - **Auth:** `new_admin`, which must be the proposed address.
    /// - **Preconditions:** a proposal is pending, it names `new_admin`, it was
    ///   made by the admin still in office, and `now < expires_at`.
    /// - **Effects:** stores `new_admin` as admin, clears the proposal and
    ///   extends the instance TTL. The previous admin loses every admin right in
    ///   the same transaction.
    /// - **Events:** `admin_transfer_accepted`.
    /// - **Errors:** `NotInitialized`, `NoPendingAdminTransfer`, `Unauthorized`,
    ///   `AdminTransferExpired`, `SchemaMigrationRequired`, `SchemaVersionUnsupported`.
    pub fn accept_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        new_admin.require_auth();
        let current = storage::get_admin(&env).ok_or(Error::NotInitialized)?;
        storage::check_schema(&env)?;
        let pending = storage::get_pending_admin(&env).ok_or(Error::NoPendingAdminTransfer)?;

        // Only the named successor may accept, and only a proposal made by the
        // admin still in office counts. The second check cannot fail through
        // this contract's own entry points (acceptance clears the proposal),
        // but it keeps a proposal carried across an upgrade from outliving the
        // admin that made it.
        if pending.new_admin != new_admin || pending.proposed_by != current {
            return Err(Error::Unauthorized);
        }
        if env.ledger().timestamp() >= pending.expires_at {
            return Err(Error::AdminTransferExpired);
        }

        storage::set_admin(&env, &new_admin);
        storage::clear_pending_admin(&env);
        storage::extend_instance_ttl(&env);
        AdminTransferAccepted {
            schema_version: EVENT_SCHEMA_VERSION,
            previous_admin: current,
            new_admin,
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
    /// must be in `1..=MAX_RATE_PER_CALL`. `endpoint_hash` commits to the
    /// off-chain endpoint descriptor.
    pub fn register_service(
        env: Env,
        provider: Address,
        rate_per_call: i128,
        endpoint_hash: BytesN<32>,
    ) -> Result<u64, Error> {
        provider.require_auth();
        storage::check_schema(&env)?;

        if rate_per_call <= 0 {
            return Err(Error::InvalidRate);
        }
        // The bound is checked here, where a listing is made, rather than at
        // lease time: a consumer should not be able to be quoted a price whose
        // deposit cannot be computed, and a provider should not be able to
        // advertise a service no lease can fund.
        if rate_per_call > MAX_RATE_PER_CALL {
            return Err(Error::RateTooHigh);
        }
        // An all-zero digest commits to nothing, so it cannot be a real
        // endpoint commitment. Reject it here, where the listing is made, so
        // no service is ever stored with an unattested descriptor.
        if endpoint_hash == BytesN::from_array(&env, &[0u8; 32]) {
            return Err(Error::InvalidEndpointHash);
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
        storage::check_schema(&env)?;

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
    /// Values above [`MAX_RATE_PER_CALL`] are rejected with `Error::RateTooHigh`,
    /// so an update cannot publish a price the listing path would have refused.
    pub fn update_service(
        env: Env,
        provider: Address,
        service_id: u64,
        rate_per_call: i128,
        endpoint_hash: BytesN<32>,
    ) -> Result<(), Error> {
        provider.require_auth();
        storage::check_schema(&env)?;

        let mut service = storage::get_service(&env, service_id)?;
        if service.provider != provider {
            return Err(Error::Unauthorized);
        }
        if rate_per_call <= 0 {
            return Err(Error::InvalidRate);
        }
        if rate_per_call > MAX_RATE_PER_CALL {
            return Err(Error::RateTooHigh);
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
    /// only be settled through [`KeyLeaseRegistry::settle_lease_attested`] with a payload
    /// signed by that key; [`KeyLeaseRegistry::settle_lease`] fails with
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
    /// `get_service_attestor` before leasing if it wants one.
    pub fn set_service_attestor(
        env: Env,
        provider: Address,
        service_id: u64,
        attestor: Option<BytesN<32>>,
    ) -> Result<(), Error> {
        provider.require_auth();
        storage::check_schema(&env)?;

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
        storage::check_schema(&env)?;

        mint_lease(&env, &consumer, service_id, requested_calls, duration_secs)
    }

    /// Mint a lease for `consumer`, submitted and co-signed by `delegate`.
    ///
    /// This is the cold-storage path: an integration service that runs on a
    /// schedule can top up a lease without ever holding the consumer's spending
    /// key.
    ///
    /// **Both** the delegate and the consumer must authorize the call, and what
    /// each is signing for is fixed by this contract's design:
    ///
    /// * **Debit:** the deposit leaves the *consumer's* token balance, never
    ///   the delegate's. The registry calls `transfer(consumer → contract,
    ///   rate × calls)`, and the SEP-41 token itself requires the consumer's
    ///   signature for that withdrawal — which is exactly why the consumer has
    ///   to co-sign here.
    /// * **Refund and ownership:** the lease is recorded against the consumer
    ///   (`Lease.consumer`), so every later `settle_lease` refund and every
    ///   `revoke_expired_lease` goes to the consumer. The delegate is not
    ///   stored on the lease and gains no claim on it; its only role is to pay
    ///   the transaction fee.
    ///
    /// A delegate that acts without the consumer's signature is rejected before
    /// any state is written, so it cannot lock funds it does not own. Smart
    /// wallet (multisig) consumer accounts work as `consumer` unchanged: their
    /// own `__check_auth` decides that the signature set is satisfied.
    ///
    /// Returns the new `lease_id`.
    pub fn create_lease_for(
        env: Env,
        delegate: Address,
        consumer: Address,
        service_id: u64,
        requested_calls: u32,
        duration_secs: u64,
    ) -> Result<u64, Error> {
        delegate.require_auth();
        consumer.require_auth();
        storage::check_schema(&env)?;

        let lease_id = mint_lease(&env, &consumer, service_id, requested_calls, duration_secs)?;

        LeaseDelegated {
            lease_id,
            schema_version: EVENT_SCHEMA_VERSION,
            delegate,
            consumer,
        }
        .publish(&env);

        Ok(lease_id)
    }

    /// Request an early cancellation of an active lease. Requires consumer auth.
    ///
    /// The consumer is asking the provider to release the unused balance now
    /// instead of waiting for expiry and the full grace window. The request
    /// alone moves no funds and changes no balances; the provider decides by
    /// calling [`KeyLeaseRegistry::approve_early_cancel`] or [`KeyLeaseRegistry::reject_early_cancel`].
    ///
    /// Only an unsettled, unexpired lease can be cancelled early, and a lease
    /// can have at most one pending request. The consumer may withdraw the
    /// request with [`KeyLeaseRegistry::withdraw_early_cancel`].
    pub fn request_early_cancel(env: Env, consumer: Address, lease_id: u64) -> Result<(), Error> {
        consumer.require_auth();
        storage::check_schema(&env)?;

        let lease = storage::get_lease(&env, lease_id)?;
        if lease.consumer != consumer {
            return Err(Error::Unauthorized);
        }
        if lease.settled {
            return Err(Error::LeaseAlreadySettled);
        }

        let now = env.ledger().timestamp();
        if now >= lease.expires_at {
            return Err(Error::LeaseExpired);
        }
        if storage::get_cancel_request(&env, lease_id).is_some() {
            return Err(Error::CancellationAlreadyRequested);
        }

        storage::save_cancel_request(
            &env,
            &CancelRequest {
                lease_id,
                requested_at: now,
            },
        );

        LeaseCancellationRequested {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            consumer,
        }
        .publish(&env);

        Ok(())
    }

    /// Withdraw a pending early-cancellation request. Requires consumer auth.
    pub fn withdraw_early_cancel(env: Env, consumer: Address, lease_id: u64) -> Result<(), Error> {
        consumer.require_auth();
        storage::check_schema(&env)?;

        let lease = storage::get_lease(&env, lease_id)?;
        if lease.consumer != consumer {
            return Err(Error::Unauthorized);
        }
        if storage::get_cancel_request(&env, lease_id).is_none() {
            return Err(Error::CancellationNotRequested);
        }

        storage::remove_cancel_request(&env, lease.lease_id);

        LeaseCancellationWithdrawn {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            consumer,
        }
        .publish(&env);

        Ok(())
    }

    /// Decline a pending early-cancellation request. Requires the service
    /// provider's auth; the lease stays active and unchanged.
    pub fn reject_early_cancel(env: Env, provider: Address, lease_id: u64) -> Result<(), Error> {
        provider.require_auth();
        storage::check_schema(&env)?;

        let lease = storage::get_lease(&env, lease_id)?;
        require_lease_provider(&env, &lease, &provider)?;

        if storage::get_cancel_request(&env, lease_id).is_none() {
            return Err(Error::CancellationNotRequested);
        }

        storage::remove_cancel_request(&env, lease.lease_id);

        LeaseCancellationRejected {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            provider,
        }
        .publish(&env);

        Ok(())
    }

    /// Approve an early cancellation and release the refund immediately.
    /// Requires the service provider's auth.
    ///
    /// The provider reports the `verified_calls` it actually served for the
    /// lease. The payout is `verified_calls * rate_per_call` and the refund is
    /// `locked_deposit - payout`, which is exactly the unused-call amount at the
    /// stored rate. Both amounts are integers in the token's smallest unit, so
    /// there is no rounding. The lease is settled in the same transaction, so it
    /// cannot be settled or revoked again.
    pub fn approve_early_cancel(
        env: Env,
        provider: Address,
        lease_id: u64,
        verified_calls: u32,
    ) -> Result<(), Error> {
        provider.require_auth();
        storage::check_schema(&env)?;

        let mut lease = storage::get_lease(&env, lease_id)?;
        require_lease_provider(&env, &lease, &provider)?;
        if lease.settled {
            return Err(Error::LeaseAlreadySettled);
        }
        if storage::get_cancel_request(&env, lease_id).is_none() {
            return Err(Error::CancellationNotRequested);
        }
        if verified_calls > lease.allocated_calls {
            return Err(Error::InvalidCalls);
        }

        let service = storage::get_service(&env, lease.service_id)?;
        let rate = service.rate_per_call;

        // Refund is the unused-call value. `verified_calls <= allocated_calls`,
        // so the subtraction cannot underflow, and the multiplication is
        // checked like every other money path. Rate is a per-call integer, so
        // no division or rounding is involved.
        let unused_calls = lease
            .allocated_calls
            .checked_sub(verified_calls)
            .ok_or(Error::Overflow)?;
        let payout = rate
            .checked_mul(verified_calls as i128)
            .ok_or(Error::Overflow)?;
        let refund = rate
            .checked_mul(unused_calls as i128)
            .ok_or(Error::Overflow)?;
        if payout.checked_add(refund) != Some(lease.locked_deposit) {
            return Err(Error::Overflow);
        }

        // checks-effects-interactions: settle the lease and clear the request
        // before any token transfer.
        lease.used_calls = verified_calls;
        lease.settled = true;
        storage::save_lease(&env, &lease);
        storage::remove_cancel_request(&env, lease.lease_id);
        // An approved cancellation pays the provider for served calls, so it is
        // a settlement of that service and belongs in the cumulative totals —
        // otherwise `service_stats` would under-report revenue that left
        // escrow. Revocation, which pays the provider nothing, stays out.
        storage::record_settlement(&env, lease.service_id, verified_calls, payout)?;

        let token_addr = storage::get_token(&env).ok_or(Error::TokenNotSet)?;
        let client = token::Client::new(&env, &token_addr);
        let contract = env.current_contract_address();
        if payout > 0 {
            client.transfer(&contract, &provider, &payout);
        }
        if refund > 0 {
            client.transfer(&contract, &lease.consumer, &refund);
        }

        LeaseCancelled {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            provider,
            consumer: lease.consumer,
            verified_calls,
            payout,
            refund,
        }
        .publish(&env);

        Ok(())
    }

    /// Transfer a lease to a new consumer, moving the access right and the
    /// refund path together. Requires the current consumer's auth.
    ///
    /// The transfer is a direct assignment: the new consumer does not have to
    /// accept it. Because the provider and every settlement path read
    /// `lease.consumer`, a later settlement refunds `new_consumer` and the old
    /// consumer loses both the access right and any claim on the deposit.
    ///
    /// Only an unsettled, unrevoked lease can change hands. A settled lease has
    /// no rights left to transfer, and a settled lease is exactly what a
    /// proposed settlement produces, so a lease inside a challenge window cannot
    /// be moved out from under its dispute.
    pub fn transfer_lease(
        env: Env,
        consumer: Address,
        lease_id: u64,
        new_consumer: Address,
    ) -> Result<(), Error> {
        consumer.require_auth();

        let mut lease = storage::get_lease(&env, lease_id)?;
        if lease.consumer != consumer {
            return Err(Error::Unauthorized);
        }
        if lease.settled {
            return Err(Error::LeaseNotTransferable);
        }
        if lease.consumer == new_consumer {
            return Err(Error::InvalidConsumer);
        }

        let previous = lease.consumer.clone();
        lease.consumer = new_consumer.clone();
        storage::save_lease(&env, &lease);
        storage::remove_cancel_request(&env, lease.lease_id);

        LeaseTransferred {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            from: previous,
            to: new_consumer,
        }
        .publish(&env);

        Ok(())
    }

    /// Propose a settlement and open the consumer challenge window. Requires
    /// provider auth.
    ///
    /// A lease may only be settled when it has **expired** or when
    /// `verified_calls == allocated_calls` (the consumer got everything it
    /// paid for, so there is no reason to wait). The proposal records
    /// `verified_calls * rate_per_call` as the provider's payout and
    /// `locked_deposit - payout` as the consumer's refund, but moves no funds.
    ///
    /// The lease can no longer be settled again or revoked from this point. The
    /// payout stays locked in escrow for [`CHALLENGE_WINDOW_SECS`] (24 hours),
    /// during which the consumer may call
    /// [`crate::KeyLeaseRegistry::challenge_settlement`]. Once the window closes,
    /// anyone may call [`crate::KeyLeaseRegistry::finalize_settlement`] to release
    /// the funds. A raised challenge is decided by the admin through
    /// [`crate::KeyLeaseRegistry::resolve_challenge`]. The finalized split is
    /// added to the service's cumulative totals, which
    /// [`KeyLeaseRegistry::service_stats`] reports.
    pub fn settle_lease(
        env: Env,
        provider: Address,
        lease_id: u64,
        verified_calls: u32,
    ) -> Result<(), Error> {
        provider.require_auth();
        storage::check_schema(&env)?;

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
    /// Every non-attestation rule of [`KeyLeaseRegistry::settle_lease`] still applies: the
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
        storage::check_schema(&env)?;

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
        let challengeable_until = now
            .checked_add(CHALLENGE_WINDOW_SECS)
            .ok_or(Error::Overflow)?;

        // The lease is committed to this settlement: it can no longer be settled
        // again or revoked, even though the funds stay in escrow.
        lease.used_calls = verified_calls;
        lease.settled = true;
        storage::save_lease(&env, &lease);
        // A pending early-cancellation request is moot once the lease settles.
        storage::remove_cancel_request(&env, lease.lease_id);

        storage::save_settlement(
            &env,
            &Settlement {
                lease_id: lease.lease_id,
                service_id: lease.service_id,
                consumer: lease.consumer.clone(),
                provider: service.provider.clone(),
                verified_calls,
                payout,
                refund,
                proposed_at: now,
                challengeable_until,
                state: SettlementState::Pending,
                challenge_evidence: BytesN::from_array(&env, &[0u8; 32]),
                challenged_at: 0,
            },
        );

        LeaseSettlementProposed {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id: lease.lease_id,
            provider: service.provider,
            verified_calls,
            payout,
            refund,
            challengeable_until,
        }
        .publish(&env);

        Ok(())
    }

    /// Release a proposed settlement after its challenge window has closed.
    ///
    /// Permissionless: the provider, the consumer or any keeper can call it once
    /// `now >= challengeable_until` and no challenge is active. Funds move to
    /// the provider and consumer exactly as proposed by the provider.
    pub fn finalize_settlement(env: Env, lease_id: u64) -> Result<(), Error> {
        let mut settlement = storage::get_settlement(&env, lease_id)?;
        match settlement.state {
            SettlementState::Finalized => return Err(Error::SettlementFinalized),
            SettlementState::Challenged => return Err(Error::SettlementChallenged),
            SettlementState::Pending => {}
        }

        if env.ledger().timestamp() < settlement.challengeable_until {
            return Err(Error::ChallengeWindowStillOpen);
        }

        let (payout, refund) = (settlement.payout, settlement.refund);
        pay_settlement(&env, &mut settlement, payout, refund)
    }

    /// Dispute a proposed settlement with an evidence hash. Requires consumer
    /// auth and must happen inside the challenge window.
    ///
    /// The settlement is frozen until the admin resolves the challenge; the
    /// payout cannot be finalized while it is active. A consumer may withdraw
    /// the challenge with
    /// [`crate::KeyLeaseRegistry::cancel_challenge`].
    pub fn challenge_settlement(
        env: Env,
        consumer: Address,
        lease_id: u64,
        evidence_hash: BytesN<32>,
    ) -> Result<(), Error> {
        consumer.require_auth();

        let mut settlement = storage::get_settlement(&env, lease_id)?;
        if settlement.consumer != consumer {
            return Err(Error::Unauthorized);
        }
        match settlement.state {
            SettlementState::Finalized => return Err(Error::SettlementFinalized),
            SettlementState::Challenged => return Err(Error::SettlementChallenged),
            SettlementState::Pending => {}
        }

        let now = env.ledger().timestamp();
        if now >= settlement.challengeable_until {
            return Err(Error::ChallengeWindowClosed);
        }
        if evidence_hash == BytesN::from_array(&env, &[0u8; 32]) {
            return Err(Error::InvalidEvidence);
        }

        settlement.challenge_evidence = evidence_hash.clone();
        settlement.challenged_at = now;
        settlement.state = SettlementState::Challenged;
        storage::save_settlement(&env, &settlement);

        LeaseChallenged {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            consumer,
            evidence_hash,
        }
        .publish(&env);

        Ok(())
    }

    /// Withdraw an active challenge, returning the settlement to `Pending`.
    /// Requires consumer auth.
    ///
    /// Cancelling does not pay out immediately: the original challenge window
    /// still has to close before
    /// [`crate::KeyLeaseRegistry::finalize_settlement`] can release the funds.
    /// While the window remains open the consumer may challenge again.
    pub fn cancel_challenge(env: Env, consumer: Address, lease_id: u64) -> Result<(), Error> {
        consumer.require_auth();

        let mut settlement = storage::get_settlement(&env, lease_id)?;
        if settlement.consumer != consumer {
            return Err(Error::Unauthorized);
        }
        match settlement.state {
            SettlementState::Finalized => return Err(Error::SettlementFinalized),
            SettlementState::Pending => return Err(Error::NoActiveChallenge),
            SettlementState::Challenged => {}
        }

        settlement.challenge_evidence = BytesN::from_array(&env, &[0u8; 32]);
        settlement.challenged_at = 0;
        settlement.state = SettlementState::Pending;
        storage::save_settlement(&env, &settlement);

        LeaseChallengeCancelled {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            consumer,
        }
        .publish(&env);

        Ok(())
    }

    /// Resolve an active challenge. Admin only.
    ///
    /// `upheld = true` refunds the entire deposit to the consumer and pays the
    /// provider nothing. `upheld = false` releases the proposal unchanged. The
    /// admin may resolve before or after the challenge window closes.
    pub fn resolve_challenge(
        env: Env,
        admin: Address,
        lease_id: u64,
        upheld: bool,
    ) -> Result<(), Error> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        let mut settlement = storage::get_settlement(&env, lease_id)?;
        match settlement.state {
            SettlementState::Finalized => return Err(Error::SettlementFinalized),
            SettlementState::Pending => return Err(Error::NoActiveChallenge),
            SettlementState::Challenged => {}
        }

        let (payout, refund) = if upheld {
            (
                0,
                settlement
                    .payout
                    .checked_add(settlement.refund)
                    .ok_or(Error::Overflow)?,
            )
        } else {
            (settlement.payout, settlement.refund)
        };

        LeaseChallengeResolved {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            admin: admin.clone(),
            upheld,
        }
        .publish(&env);

        pay_settlement(&env, &mut settlement, payout, refund)
    }

    /// Reclaim the full deposit for a lease the provider never settled.
    ///
    /// The lease must be expired and at least [`SETTLE_GRACE_PERIOD_SECS`]
    /// (24 hours) must have elapsed since expiry. Requires `consumer` auth.
    /// This is the consumer's escape hatch: a provider that goes offline cannot
    /// strand escrowed funds.
    ///
    /// The lease ends as [`LeaseStatus::Revoked`], distinct from a provider
    /// settlement, and `used_calls` stays `0` because the provider earned
    /// nothing. Revocation leaves the service's revenue totals untouched.
    pub fn revoke_expired_lease(env: Env, consumer: Address, lease_id: u64) -> Result<(), Error> {
        consumer.require_auth();
        storage::check_schema(&env)?;

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
        // The marker is what lets `lease_status` report `Revoked` rather than
        // the generic `Settled`. It is persisted with the lease and before the
        // refund, and it never counts toward the service's revenue: a revoked
        // lease paid the provider nothing.
        storage::mark_lease_revoked(&env, &lease);

        // A pending early-cancellation request is moot once the lease is revoked.
        storage::remove_cancel_request(&env, lease.lease_id);

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
    ///
    /// Not gated on the schema check: it changes no stored value, and keeping
    /// entries alive is exactly what a stopped registry awaiting `migrate`
    /// needs.
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

    /// The pending admin transfer, if any. Pure read.
    ///
    /// An expired proposal is still returned (compare `expires_at` with the
    /// ledger time) until the admin cancels or replaces it; it can no longer be
    /// accepted.
    pub fn get_pending_admin(env: Env) -> Option<PendingAdminTransfer> {
        storage::get_pending_admin(&env)
    }

    /// Return the configured escrow token, or `TokenNotSet`.
    pub fn get_token(env: Env) -> Result<Address, Error> {
        storage::get_token(&env).ok_or(Error::TokenNotSet)
    }

    /// Pending upgrade waiting out its delay, if one has been proposed.
    ///
    /// The returned `execute_after` is a ledger timestamp; clients can compare
    /// it with `ledger.timestamp()` to know when execution becomes possible.
    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        storage::get_pending_upgrade(&env)
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

    /// List the services `provider` owns, oldest first, one bounded page at a time.
    ///
    /// Reads `limit` entries from the provider's index starting at `after` and
    /// returns them with the cursor to pass back for the next page. Keep paging
    /// while `done` is `false`; `next` counts index entries consumed, not ids
    /// returned, so a page can be legitimately short when an entry no longer
    /// belongs to the provider.
    ///
    /// Ownership is re-checked against each `Service` record, which is the
    /// authority: the index is derived state that is appended to on write but
    /// pruned only on read. A provider with no index yet returns an empty page
    /// rather than an error, so a new account can page before it registers.
    ///
    /// Pure read: like every other view, it extends no TTL, its own index entry
    /// included. Use [`KeyLeaseRegistry::register_service`] or [`KeyLeaseRegistry::set_service_active`]
    /// to keep a catalogue's index alive.
    pub fn services_for_provider(
        env: Env,
        provider: Address,
        after: u32,
        limit: u32,
    ) -> Result<ProviderServicePage, Error> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(Error::InvalidLimit);
        }
        storage::get_provider_services(&env, &provider, after, limit)
    }

    /// Fetch a lease. Pure read: never extends a TTL.
    ///
    /// Use [`KeyLeaseRegistry::bump_lease`] to keep an entry alive.
    pub fn get_lease(env: Env, lease_id: u64) -> Result<Lease, Error> {
        storage::get_lease(&env, lease_id)
    }

    /// Predict what [`KeyLeaseRegistry::create_lease`] would lock, without auth or a transfer.
    ///
    /// Consumers and SDKs used to re-derive the deposit and the expiry off-chain,
    /// where any change to contract semantics turns into silent drift. This view
    /// runs the same validation, in the same order, as `create_lease`, so every
    /// error it returns is the error `create_lease` would return for the same
    /// arguments — with the single exception of a balance the registry never
    /// reads: a quote says nothing about whether `consumer` can actually pay.
    ///
    /// `expires_at` is derived from the current ledger time, so a quote goes
    /// stale as soon as the ledger advances.
    pub fn quote_lease(
        env: Env,
        service_id: u64,
        requested_calls: u32,
        duration_secs: u64,
    ) -> Result<LeaseQuote, Error> {
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

        let locked_deposit = service
            .rate_per_call
            .checked_mul(requested_calls as i128)
            .ok_or(Error::Overflow)?;
        if locked_deposit <= 0 {
            return Err(Error::InvalidRate);
        }
        // Same position in create_lease's order, for the same reason: `Overflow`
        // means the product does not fit `i128`, `DepositTooLarge` means it fits
        // and is still more than one lease may lock.
        if locked_deposit > MAX_LEASE_DEPOSIT {
            return Err(Error::DepositTooLarge);
        }

        // Mirrors create_lease: a registry with no escrow token could never fund
        // the lease this quote predicts.
        if storage::get_token(&env).is_none() {
            return Err(Error::TokenNotSet);
        }

        let expires_at = env
            .ledger()
            .timestamp()
            .checked_add(duration_secs)
            .ok_or(Error::Overflow)?;

        Ok(LeaseQuote {
            locked_deposit,
            expires_at,
        })
    }

    /// Derive the lifecycle status of a lease at the current ledger time.
    /// Derive the lifecycle status of a lease at the current ledger time.
    /// Pure read.
    ///
    /// A lease whose escrow was reclaimed by the consumer reports
    /// [`LeaseStatus::Revoked`]; one the provider was paid for reports
    /// [`LeaseStatus::Settled`]. Both are final.
    pub fn lease_status(env: Env, lease_id: u64) -> Result<LeaseStatus, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        let revoked = storage::is_lease_revoked(&env, &lease);
        Ok(LeaseStatus::of(&lease, env.ledger().timestamp(), revoked))
    }

    /// Lifetime settlement totals for a service: leases settled, calls billed
    /// and tokens paid out.
    ///
    /// Providers and indexers read revenue here instead of replaying
    /// `lease_settled` from the first ledger. Revoked leases never appear: the
    /// consumer took the deposit back and the provider earned nothing.
    ///
    /// Errors with `ServiceNotFound` for an unknown id, so a typo never reads
    /// as a service that has made nothing.
    pub fn service_stats(env: Env, service_id: u64) -> Result<ServiceStats, Error> {
        storage::get_service(&env, service_id)?;
        Ok(storage::get_service_stats(&env, service_id))
    }

    /// Storage schema the stored data conforms to. Pure read.
    ///
    /// `0` for a registry deployed before the schema was versioned. When this
    /// differs from [`KeyLeaseRegistry::code_schema_version`], state-changing calls are
    /// stopped until the admin runs [`KeyLeaseRegistry::migrate`]. `NotInitialized` before
    /// `init`.
    pub fn schema_version(env: Env) -> Result<u32, Error> {
        storage::stored_schema_version(&env).ok_or(Error::NotInitialized)
    }

    /// Storage schema the running Wasm was built for ([`SCHEMA_VERSION`]).
    /// Pure read.
    pub fn code_schema_version(_env: Env) -> u32 {
        SCHEMA_VERSION
    }

    /// Number of calls still available on a lease (zero once settled/expired).
    /// Number of calls still available on a lease (zero once settled/expired).
    /// Pure read.
    ///
    /// A revoked lease reports `0` like any other final lease: its allotment
    /// expired unused and the deposit is gone, so there is nothing left to
    /// authorize. The provider's earned count for a revoked lease is the
    /// `used_calls` field itself, which revocation leaves at `0`.
    pub fn remaining_calls(env: Env, lease_id: u64) -> Result<u32, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        let now = env.ledger().timestamp();
        if lease.settled || now >= lease.expires_at {
            return Ok(0);
        }
        Ok(lease.allocated_calls.saturating_sub(lease.used_calls))
    }

    /// Fetch the settlement proposed for a lease.
    pub fn get_settlement(env: Env, lease_id: u64) -> Result<Settlement, Error> {
        storage::get_settlement(&env, lease_id)
    }

    /// Escrow balance owed to a lease, in token stroops, at the current ledger
    /// time. Includes a proposed settlement's payout and refund until it is
    /// finalized.
    pub fn locked_balance(env: Env, lease_id: u64) -> Result<i128, Error> {
        let lease = storage::get_lease(&env, lease_id)?;
        if !lease.settled {
            return Ok(lease.locked_deposit);
        }
        match storage::get_settlement(&env, lease_id) {
            Ok(settlement) if settlement.state != SettlementState::Finalized => settlement
                .payout
                .checked_add(settlement.refund)
                .ok_or(Error::Overflow),
            // Revoked leases never have a settlement, and finalized ones hold nothing.
            _ => Ok(0),
        }
    }

    /// One bounded page of the service catalogue, ascending by id.
    ///
    /// This exists so a dashboard or indexer does not have to replay the whole
    /// event history to learn what is registered. `start` is the first id to
    /// read (ids begin at 1, so `start` of 0 reads from the beginning) and
    /// `limit` is at most [`MAX_PAGE_SIZE`]. Pass `next_start` back as `start`
    /// until `done` is `true`.
    ///
    /// `done`, not the page length, is the end of the catalogue: ids whose
    /// entry has been evicted are skipped, so a short page with `done == false`
    /// just means the walk passed entries that are no longer on the ledger.
    /// [`KeyLeaseRegistry::service_count`] still counts them.
    ///
    /// Pure read: extends no TTL, including the entries it walks over.
    pub fn get_services(env: Env, start: u64, limit: u32) -> Result<ServicePage, Error> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(Error::InvalidLimit);
        }
        Ok(storage::services_page(&env, start, limit))
    }

    /// One bounded page of the lease record, ascending by id.
    ///
    /// Same paging contract as [`KeyLeaseRegistry::get_services`]: page until `done`, and
    /// treat a short page as evicted entries rather than as the end. Lease ids
    /// are global and monotonic across services, so a page mixes services;
    /// filter by `service_id` client side.
    ///
    /// Pure read: extends no TTL. Use [`KeyLeaseRegistry::bump_lease`] to keep an entry
    /// readable.
    pub fn get_leases(env: Env, start: u64, limit: u32) -> Result<LeasePage, Error> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(Error::InvalidLimit);
        }
        Ok(storage::leases_page(&env, start, limit))
    }
}

/// Release the funds recorded in `settlement` and mark it finalized.
///
/// checks-effects-interactions: the finalized state is persisted before any
/// token transfer, so a re-entrant token cannot release the same funds twice.
fn pay_settlement(
    env: &Env,
    settlement: &mut Settlement,
    payout: i128,
    refund: i128,
) -> Result<(), Error> {
    let token_addr = storage::get_token(env).ok_or(Error::TokenNotSet)?;

    // Revenue is counted once funds actually leave escrow, not when the
    // proposal is made: a challenged settlement can still be overturned.
    let paid_calls = if payout == 0 {
        0
    } else {
        settlement.verified_calls
    };
    storage::record_settlement(env, settlement.service_id, paid_calls, payout)?;

    settlement.payout = payout;
    settlement.refund = refund;
    settlement.state = SettlementState::Finalized;
    storage::save_settlement(env, settlement);

    let client = token::Client::new(env, &token_addr);
    let contract = env.current_contract_address();
    if payout > 0 {
        client.transfer(&contract, &settlement.provider, &payout);
    }
    if refund > 0 {
        client.transfer(&contract, &settlement.consumer, &refund);
    }

    LeaseSettled {
        schema_version: EVENT_SCHEMA_VERSION,
        lease_id: settlement.lease_id,
        provider: settlement.provider.clone(),
        verified_calls: settlement.verified_calls,
        payout,
        refund,
    }
    .publish(env);
    Ok(())
}

/// Ensure `caller` is the provider that owns the lease's service.
fn require_lease_provider(env: &Env, lease: &Lease, caller: &Address) -> Result<(), Error> {
    let service = storage::get_service(env, lease.service_id)?;
    if service.provider != *caller {
        return Err(Error::Unauthorized);
    }
    Ok(())
}

/// Ensure `caller` matches the stored admin.
fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
    let admin = storage::get_admin(env).ok_or(Error::NotInitialized)?;
    if admin != *caller {
        return Err(Error::Unauthorized);
    }
    Ok(())
}

/// Fund the escrow and record the lease, shared by `create_lease` and
/// `create_lease_for`.
///
/// Authorization is the callers' job: this helper runs only after the consumer
/// has signed, because the deposit is withdrawn from `consumer` and `consumer`
/// is who owns the refund path afterwards.
fn mint_lease(
    env: &Env,
    consumer: &Address,
    service_id: u64,
    requested_calls: u32,
    duration_secs: u64,
) -> Result<u64, Error> {
    if requested_calls == 0 {
        return Err(Error::InvalidCalls);
    }
    if duration_secs == 0 {
        return Err(Error::InvalidDuration);
    }
    if duration_secs > MAX_LEASE_DURATION_SECS {
        return Err(Error::DurationTooLong);
    }

    let service = storage::get_service(env, service_id)?;
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
    let token_addr = storage::get_token(env).ok_or(Error::TokenNotSet)?;

    let now = env.ledger().timestamp();
    let expires_at = now.checked_add(duration_secs).ok_or(Error::Overflow)?;

    let lease_id = storage::next_lease_id(env)?;
    let lease = Lease {
        lease_id,
        service_id,
        consumer: consumer.clone(),
        allocated_calls: requested_calls,
        used_calls: 0,
        expires_at,
        locked_deposit: required_deposit,
        settled: false,
        created_at: now,
    };

    // checks-effects-interactions: the lease exists before the token is
    // called, and the whole transaction reverts if the transfer does.
    storage::save_lease(env, &lease);
    storage::extend_instance_ttl(env);
    // `get_service` above is a pure read, so an actively-leased service is
    // kept alive by the lease that consumes it.
    storage::extend_service_ttl(env, service_id);
    token::Client::new(env, &token_addr).transfer(
        consumer,
        env.current_contract_address(),
        &required_deposit,
    );

    LeaseCreated {
        lease_id,
        schema_version: EVENT_SCHEMA_VERSION,
        service_id,
        consumer: consumer.clone(),
        allocated_calls: requested_calls,
        expires_at,
        locked_deposit: required_deposit,
        rate_per_call: service.rate_per_call,
    }
    .publish(env);

    Ok(lease_id)
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
    let attestor = storage::get_service_attestor(&env, lease.service_id)
        .ok_or(Error::AttestorNotConfigured)?;

    // Bind the structural checks above to the configured public key. The host
    // crypto primitive aborts the invocation when the signature is invalid,
    // before settlement writes or token transfers are reached.
    let message = usage_attestation_message(
        env,
        attestation.lease_id,
        attestation.service_id,
        attestation.verified_calls,
        attestation.valid_until,
    );
    env.crypto()
        .ed25519_verify(&attestor, &message, &attestation.signature);

    Ok(())
}
