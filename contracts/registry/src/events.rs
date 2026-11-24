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
//! ("lease_settled", lease_id)  ->  { provider, verified_calls, payout, refund }
//! ```
//!
//! Events never carry secret material: `endpoint_hash` is already a commitment.

use soroban_sdk::{contractevent, Address, BytesN};

/// Emitted once, when the registry is initialized.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryInitialized {
    /// Address that will administer the registry.
    pub admin: Address,
}

/// Emitted when the admin points the registry at the SEP-41 escrow token.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowTokenSet {
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

/// Emitted when a provider settles a lease from escrow.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseSettled {
    /// Lease that was settled.
    #[topic]
    pub lease_id: u64,
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
    /// Consumer that received the refund.
    pub consumer: Address,
    /// Full deposit returned to the consumer.
    pub refund: i128,
}

/// Emitted when a consumer tops up a lease's calls and/or duration.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseToppedUp {
    /// Lease that was extended.
    #[topic]
    pub lease_id: u64,
    /// Consumer that funded the top-up.
    pub consumer: Address,
    /// Calls added by this top-up.
    pub additional_calls: u32,
    /// Seconds added to the lease term by this top-up.
    pub additional_duration_secs: u64,
    /// New expiry timestamp after the top-up.
    pub new_expires_at: u64,
    /// Additional deposit escrowed by this top-up.
    pub added_deposit: i128,
}

/// Emitted when a provider enables, replaces or clears a service's receipt key.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceReceiptKeySet {
    /// Service whose receipt policy changed.
    #[topic]
    pub service_id: u64,
    /// Provider that owns the service.
    pub provider: Address,
    /// Required ed25519 receipt key, or `None` to restore unsigned settlement.
    pub receipt_key: Option<BytesN<32>>,
}

/// Emitted when a beneficiary withdraws its pull-payment balance.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claimed {
    /// Address that received the tokens.
    #[topic]
    pub beneficiary: Address,
    /// Amount transferred out of the registry.
    pub amount: i128,
}

/// Emitted when the admin upgrades the registry's Wasm.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgraded {
    /// Admin that authorized the upgrade.
    pub admin: Address,
    /// Hash of the new Wasm installed by the Soroban deployer.
    pub new_wasm_hash: BytesN<32>,
}
