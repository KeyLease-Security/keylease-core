use soroban_sdk::{contracttype, Address, BytesN};

/// A provider-published API or RPC service that can be leased by consumers.
///
/// `endpoint_hash` commits to the off-chain endpoint metadata (e.g. a SHA-256
/// digest of the service descriptor) without publishing the static credential
/// on-chain. Leases are authorized against this commitment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Service {
    /// Monotonic, protocol-assigned identifier (starting at 1).
    pub service_id: u64,
    /// The provider that owns and operates the service.
    pub provider: Address,
    /// Price charged per metered call, denominated in the escrow token's stroops.
    pub rate_per_call: i128,
    /// Whether new leases may be created for this service.
    pub is_active: bool,
    /// Commitment to the off-chain endpoint descriptor.
    pub endpoint_hash: BytesN<32>,
}

/// A funded, time-bound authorization for a fixed allotment of metered calls.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lease {
    /// Monotonic, protocol-assigned identifier (starting at 1).
    pub lease_id: u64,
    /// The service this lease authorizes access to.
    pub service_id: u64,
    /// The party that locked the deposit and receives unused balance.
    pub consumer: Address,
    /// Number of calls purchased up front.
    pub allocated_calls: u32,
    /// Number of calls the provider has been paid for after settlement.
    pub used_calls: u32,
    /// Unix timestamp (ledger time) after which the lease can no longer be used.
    pub expires_at: u64,
    /// Total deposit locked in escrow: `rate_per_call * allocated_calls`.
    pub locked_deposit: i128,
    /// Whether funds have been released (settled by provider or reclaimed by consumer).
    pub settled: bool,
}

/// Lifecycle of a settlement inside the consumer challenge window.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementState {
    /// Proposed and waiting for `challengeable_until`; funds are still locked.
    Pending = 0,
    /// The consumer disputed the reported call count; the admin must resolve it.
    Challenged = 1,
    /// Funds have been released (paid out or refunded) and nothing can change.
    Finalized = 2,
}

/// A provider's proposed split of a lease's escrow, held until the challenge
/// window closes.
///
/// Kept in its own storage entry so the [`Lease`] layout is unchanged by the
/// challenge-window upgrade.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settlement {
    /// Lease being settled.
    pub lease_id: u64,
    /// Consumer that receives `refund` and may challenge the proposal.
    pub consumer: Address,
    /// Provider that receives `payout`.
    pub provider: Address,
    /// Calls the provider reported as consumed.
    pub verified_calls: u32,
    /// Amount the provider would receive when finalized.
    pub payout: i128,
    /// Amount the consumer would receive when finalized.
    pub refund: i128,
    /// Ledger timestamp at which the settlement was proposed.
    pub proposed_at: u64,
    /// First ledger timestamp at which the proposal may be finalized.
    pub challengeable_until: u64,
    /// Current state of the settlement.
    pub state: SettlementState,
    /// Commitment to the active challenge's evidence (all zeroes when none).
    pub challenge_evidence: BytesN<32>,
    /// Ledger timestamp at which the active challenge was filed (0 when none).
    pub challenged_at: u64,
}

/// Derived lifecycle state of a [`Lease`]. Discriminants are stable on the wire.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseStatus {
    /// Not yet settled and not yet past `expires_at`.
    Active = 0,
    /// Past `expires_at` but not yet settled or revoked.
    Expired = 1,
    /// No further lease transitions are possible: the lease was settled,
    /// revoked, or committed to a proposed settlement. Check `get_settlement`
    /// to see whether the escrow has actually been released.
    Settled = 2,
}

impl LeaseStatus {
    /// Derive the current status of `lease` at ledger time `now`.
    pub fn of(lease: &Lease, now: u64) -> Self {
        if lease.settled {
            LeaseStatus::Settled
        } else if now >= lease.expires_at {
            LeaseStatus::Expired
        } else {
            LeaseStatus::Active
        }
    }
}
