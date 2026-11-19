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

/// Lifetime settlement totals for one service, keyed by `service_id`.
///
/// This is a convenience aggregate over the `lease_settled` event stream, so a
/// provider or indexer can report revenue without replaying history. Only
/// provider settlements are counted: a revoked lease paid the provider nothing
/// and contributes nothing to these totals.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceStats {
    /// Leases of this service the provider has settled.
    pub settled_leases: u64,
    /// Sum of `verified_calls` across those settlements.
    pub settled_calls: u64,
    /// Sum of tokens paid to the provider across those settlements, in the
    /// escrow token's smallest unit.
    pub total_payout: i128,
}

impl ServiceStats {
    /// The totals of a service that has never been settled.
    pub const fn zero() -> Self {
        ServiceStats {
            settled_leases: 0,
            settled_calls: 0,
            total_payout: 0,
        }
    }
}

/// Derived lifecycle state of a [`Lease`]. Discriminants are stable on the wire.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseStatus {
    /// Not yet settled and not yet past `expires_at`.
    Active = 0,
    /// Past `expires_at` but not yet settled or revoked.
    Expired = 1,
    /// Funds have been released; no further state transitions are possible.
    Settled = 2,
    /// Funds were reclaimed by the consumer after the provider failed to
    /// settle. Final, like [`LeaseStatus::Settled`], but economically the
    /// opposite: the provider earned nothing.
    Revoked = 3,
}

impl LeaseStatus {
    /// Derive the current status of `lease` at ledger time `now`.
    ///
    /// `revoked` is the registry's revocation marker for this lease, not a
    /// field of [`Lease`]: the lease struct's on-chain layout is frozen, so the
    /// distinction between "settled by the provider" and "reclaimed by the
    /// consumer" is stored beside the lease instead of inside it (see
    /// [`crate::storage`]). A revoked lease is always also `settled`, which is
    /// what keeps its escrow from being released twice; therefore a settled lease
    /// must be classified by the marker before it can be reported as
    /// [`LeaseStatus::Settled`].
    pub fn of(lease: &Lease, now: u64, revoked: bool) -> Self {
        if lease.settled {
            if revoked {
                LeaseStatus::Revoked
            } else {
                LeaseStatus::Settled
            }
        } else if now >= lease.expires_at {
            LeaseStatus::Expired
        } else {
            LeaseStatus::Active
        }
    }
}
