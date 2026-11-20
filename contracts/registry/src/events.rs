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
}

/// Emitted when a lease is transferred to a new consumer.
///
/// The access right and the refund path move together, so an indexer can follow
/// who is entitled to use the lease and reclaim its deposit.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseTransferred {
    /// Lease that changed hands.
    #[topic]
    pub lease_id: u64,
    /// Previous consumer, which authorized the transfer.
    pub from: Address,
    /// New consumer and owner of the refund path.
    pub to: Address,
}

/// Emitted when a provider settles a lease from escrow.
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

/// Emitted when the admin upgrades the registry's Wasm.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    /// Payload schema version; see the module docs.
    pub schema_version: u32,
    /// Admin that authorized the change.
    pub admin: Address,
    /// Hash of the new Wasm installed by the Soroban deployer.
    pub new_wasm_hash: BytesN<32>,
}
