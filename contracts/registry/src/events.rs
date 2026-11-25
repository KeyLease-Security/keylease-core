//! Typed Soroban contract events.
//!
//! Every state transition in the registry publishes one of these events. Each
//! event name becomes the first topic (the struct name in lower snake case), and
//! the identifiers marked `#[topic]` follow it, so a Stellar indexer can filter
//! by `lease_created` / `lease_id` without decoding the payload.
//!
//! Example topics for a settlement:
//!
//! ```text
//! ("lease_settled", lease_id)  ->  { schema_version, provider, verified_calls, payout, refund }
//! ```
//!
//! Events never carry secret material: `endpoint_hash` is already a commitment.
//!
//! # Schema version
//!
//! Every payload carries `schema_version`, currently [`EVENT_SCHEMA_VERSION`].
//! It exists so an indexer reading a stream that spans a contract upgrade can
//! tell which revision produced a record instead of guessing from timestamps.
//!
//! The rules that make that work:
//!
//! * Fields are **added** by a new version; a field is never removed, renamed
//!   or retyped, because the payload is a symbol-keyed map and doing so breaks a
//!   decoder that already reads the old key.
//! * `topics` are frozen. The event name and the id that follows it are the
//!   filter keys indexers hard-code, so a version bump never touches them.
//! * A bump therefore means exactly one thing: this payload may contain keys you
//!   do not know.
//!
//! ## How to branch
//!
//! Decode the payload as a map, then read `schema_version`:
//!
//! * **key absent** → revision `0`: the pre-versioning contract. Its fields are
//!   today's fields minus `schema_version`.
//! * **`0 < version <= EVENT_SCHEMA_VERSION`** → parse the keys you know and
//!   ignore the rest.
//! * **a version newer than anything you know** → the stream is ahead of you.
//!   Store the event, do not guess a shape, and surface it as unhandled.
//!
//! Prefer the payload key over the contract's `version` metadata: the metadata
//! says which Wasm is deployed *now*, not which one emitted an event recorded
//! six months ago.

use soroban_sdk::{contractevent, Address, BytesN};

/// Payload schema version stamped into every event this contract publishes.
///
/// One name for tests, indexers and the contract metadata to agree on, so a
/// future revision changes one line.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Emitted once, when the registry is initialized.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryInitialized {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Address that will administer the registry.
    pub admin: Address,
}

/// Emitted when the admin points the registry at the SEP-41 escrow token.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowTokenSet {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Admin that authorized the change.
    pub admin: Address,
    /// SEP-41 token contract (SAC or custom) used for deposits and payouts.
    pub token: Address,
}

/// Emitted when a provider publishes a leasable service.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRegistered {
    /// Monotonic service identifier.
    #[topic]
    pub service_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Provider that owns the service.
    pub provider: Address,
    /// Price per metered call, in the escrow token's smallest unit.
    pub rate_per_call: i128,
    /// Commitment to the off-chain endpoint descriptor.
    pub endpoint_hash: BytesN<32>,
}

/// Emitted when a provider pauses or resumes new leases for a service.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceStatusChanged {
    /// Service whose status changed.
    #[topic]
    pub service_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Provider that owns the service.
    pub provider: Address,
    /// New status: `true` accepts new leases, `false` only lets existing ones settle.
    pub is_active: bool,
}

/// Emitted when a provider updates the rate_per_call and/or endpoint_hash of a service.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRateUpdated {
    /// Service whose rate/endpoint was updated.
    #[topic]
    pub service_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Provider that owns the service.
    pub provider: Address,
    /// New per-call rate in the escrow token's smallest unit.
    pub rate_per_call: i128,
    /// New commitment to the off-chain endpoint descriptor.
    pub endpoint_hash: BytesN<32>,
}

/// Emitted when a provider opts a service into or out of settlement
/// attestation.
///
/// `attestor` is the ed25519 key a settlement of this service's leases must be
/// signed with, or `None` when the service opted back out. A new event shape
/// does not bump `EVENT_SCHEMA_VERSION`: the version tracks fields of an
/// existing payload, and no existing payload changed here.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceAttestorSet {
    /// Service whose attestation policy changed.
    #[topic]
    pub service_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Provider that owns the service.
    pub provider: Address,
    /// The attestor key now required for settlement, or `None` when disabled.
    pub attestor: Option<BytesN<32>>,
}

/// Emitted when a consumer escrows a deposit and a lease is minted.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseCreated {
    /// Monotonic lease identifier.
    #[topic]
    pub lease_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Service the lease authorizes access to.
    pub service_id: u64,
    /// Address that owns the refund path.
    pub consumer: Address,
    /// Calls purchased up front.
    pub allocated_calls: u32,
    /// Ledger timestamp after which the lease no longer authorizes calls.
    pub expires_at: u64,
    /// Deposit held in escrow: `rate_per_call * allocated_calls`.
    pub locked_deposit: i128,
    /// Service rate per metered call, in the escrow token's smallest unit.
    ///
    /// This is a snapshot taken when the lease is created, so an indexer can
    /// interpret `locked_deposit` without reading the service at the event's
    /// ledger. Later changes to the service rate do not affect this value or
    /// the lease's settlement math.
    pub rate_per_call: i128,
}

/// Emitted when a provider proposes a settlement and the consumer challenge
/// window opens. No funds move yet.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseSettlementProposed {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease being settled.
    #[topic]
    pub lease_id: u64,
    /// Provider that proposed the settlement.
    pub provider: Address,
    /// Calls the provider reported as consumed.
    pub verified_calls: u32,
    /// Amount the provider receives if the proposal is finalized.
    pub payout: i128,
    /// Amount the consumer receives if the proposal is finalized.
    pub refund: i128,
    /// First ledger timestamp at which the proposal may be finalized.
    pub challengeable_until: u64,
}

/// Emitted when a consumer disputes a proposed settlement.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseChallenged {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease whose settlement was challenged.
    #[topic]
    pub lease_id: u64,
    /// Consumer that filed the challenge.
    pub consumer: Address,
    /// Commitment to the consumer's evidence.
    pub evidence_hash: BytesN<32>,
}

/// Emitted when a consumer withdraws their challenge.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseChallengeCancelled {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease whose challenge was withdrawn.
    #[topic]
    pub lease_id: u64,
    /// Consumer that withdrew the challenge.
    pub consumer: Address,
}

/// Emitted when the admin resolves a challenge.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseChallengeResolved {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease whose challenge was resolved.
    #[topic]
    pub lease_id: u64,
    /// Admin that resolved the challenge.
    pub admin: Address,
    /// `true` if the consumer's dispute was upheld.
    pub upheld: bool,
}

/// Emitted in addition to `lease_created` when a delegate minted the lease for
/// a consumer.
///
/// `lease_created` keeps its existing shape so indexers still see one event for
/// every lease; this one records only who submitted it. The delegate never
/// holds a claim on the lease: the consumer's balance funded the deposit and the
/// consumer owns every later refund.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseDelegated {
    /// Lease created on behalf of `consumer`.
    #[topic]
    pub lease_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Address that co-signed the creation and paid the transaction fee.
    pub delegate: Address,
    /// Address whose balance was debited and that owns the refund path.
    pub consumer: Address,
}

/// Emitted when a consumer asks to end a lease early.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseCancellationRequested {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease the consumer wants to end.
    #[topic]
    pub lease_id: u64,
    /// Consumer that requested cancellation.
    pub consumer: Address,
}

/// Emitted when a provider declines an early-cancellation request.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseCancellationRejected {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease whose cancellation was declined.
    #[topic]
    pub lease_id: u64,
    /// Provider that declined it.
    pub provider: Address,
}

/// Emitted when a consumer withdraws an early-cancellation request.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseCancellationWithdrawn {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease whose request was withdrawn.
    #[topic]
    pub lease_id: u64,
    /// Consumer that withdrew it.
    pub consumer: Address,
}

/// Emitted when an early cancellation is approved and funds are released.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseCancelled {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease that was ended early.
    #[topic]
    pub lease_id: u64,
    /// Provider that consented and received the payout.
    pub provider: Address,
    /// Consumer that received the refund.
    pub consumer: Address,
    /// Calls the provider reported as served.
    pub verified_calls: u32,
    /// Amount paid to the provider.
    pub payout: i128,
    /// Unused-call amount refunded to the consumer.
    pub refund: i128,
}

/// Emitted when a lease is transferred to a new consumer.
///
/// The access right and the refund path move together, so an indexer can follow
/// who is entitled to use the lease and reclaim its deposit.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseTransferred {
    /// Schema version of this payload; see [`EVENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Lease that changed hands.
    #[topic]
    pub lease_id: u64,
    /// Previous consumer, which authorized the transfer.
    pub from: Address,
    /// New consumer and owner of the refund path.
    pub to: Address,
}

/// Emitted when a settlement is finalized and funds leave escrow.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseSettled {
    /// Lease that was settled.
    #[topic]
    pub lease_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Provider that received the payout.
    pub provider: Address,
    /// Calls the provider reported as consumed.
    pub verified_calls: u32,
    /// Amount paid to the provider.
    pub payout: i128,
    /// Amount refunded to the consumer.
    pub refund: i128,
}

/// Emitted when a consumer reclaims an expired, unsettled lease.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseRevoked {
    /// Lease that was revoked.
    #[topic]
    pub lease_id: u64,
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Consumer that received the refund.
    pub consumer: Address,
    /// Full deposit returned to the consumer.
    pub refund: i128,
}

/// Emitted when the admin migrates stored data to the running Wasm's schema,
/// lifting the stop that an upgrade put in place.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaMigrated {
    /// Payload schema version; see the module docs. (Not the storage schema.)
    pub schema_version: u32,
    /// Admin that authorized the migration.
    pub admin: Address,
    /// Storage schema the data conformed to before the migration.
    pub from_version: u32,
    /// Storage schema the data conforms to now.
    pub to_version: u32,
}

/// Emitted when the admin proposes a Wasm upgrade.
///
/// `execute_after` is the ledger timestamp at which `execute_upgrade` becomes
/// legal, giving consumers a window to exit before the rules change.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeProposed {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Admin that proposed the upgrade.
    pub admin: Address,
    /// Hash of the Wasm that will be installed.
    pub new_wasm_hash: BytesN<32>,
    /// Ledger timestamp before which execution is rejected.
    pub execute_after: u64,
}

/// Emitted when the admin cancels a pending Wasm upgrade.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeCancelled {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Admin that cancelled the upgrade.
    pub admin: Address,
    /// Hash of the Wasm that will no longer be installed.
    pub new_wasm_hash: BytesN<32>,
}

/// Emitted when the admin executes a proposed upgrade after the delay.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Admin that authorized the change.
    pub admin: Address,
    /// Hash of the new Wasm installed by the Soroban deployer.
    pub new_wasm_hash: BytesN<32>,
    /// Ledger timestamp that had to be reached before execution was allowed.
    pub execute_after: u64,
}

/// Emitted when the admin proposes handing the registry to `new_admin`.
///
/// Nothing changes until `new_admin` accepts. A later proposal supersedes this
/// one, so an indexer should treat the most recent `admin_transfer_proposed`
/// as the only live proposal.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferProposed {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Current admin that authorized the proposal.
    pub admin: Address,
    /// Address that must accept to become admin.
    pub new_admin: Address,
    /// Ledger timestamp from which the proposal can no longer be accepted.
    pub expires_at: u64,
}

/// Emitted when the admin withdraws a pending transfer before it is accepted.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferCanceled {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Current admin that authorized the cancellation.
    pub admin: Address,
    /// Address whose proposal was withdrawn.
    pub new_admin: Address,
}

/// Emitted when the proposed admin accepts and the registry changes hands.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferAccepted {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Admin that proposed the transfer and no longer holds the role.
    pub previous_admin: Address,
    /// Address that authorized the acceptance and is now the admin.
    pub new_admin: Address,
}
