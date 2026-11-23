use soroban_sdk::{contracttype, Address, BytesN, Vec};

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

/// A proposed Wasm upgrade waiting out its mandatory delay.
///
/// Stored in instance storage so it survives as long as the registry's
/// configuration. The delay is 24 hours, far inside the instance TTL horizon.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgrade {
    /// Hash of the Wasm that will be installed on execution.
    pub new_wasm_hash: BytesN<32>,
    /// Ledger timestamp at which execution becomes legal.
    pub execute_after: u64,
}

/// What [`crate::KeyLeaseRegistry::create_lease`] would lock, computed without
/// authorization and without moving funds.
///
/// The field names match [`Lease`] so a quote can be compared field by field
/// with the lease it predicted.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseQuote {
    /// Deposit that would be transferred: `rate_per_call * requested_calls`.
    pub locked_deposit: i128,
    /// `ledger.timestamp() + duration_secs` at the moment the quote was read.
    pub expires_at: u64,
}

/// One page of a provider's service list, as returned by
/// [`crate::KeyLeaseRegistry::services_for_provider`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderServicePage {
    /// Ids the provider owns right now, oldest first.
    pub service_ids: Vec<u64>,
    /// How many index entries this call consumed. Pass it back as `after`.
    pub next: u32,
    /// `true` once the provider's index is exhausted; `next == 0` means no index.
    pub done: bool,
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

/// A signed statement from an optional attestor that a lease consumed a given
/// number of calls.
///
/// This is the payload a service opts into with
/// [`crate::KeyLeaseRegistry::set_service_attestor`]. The attestor signs
/// [`crate::usage_attestation_message`] for exactly the fields below, so a
/// signature only settles the lease, service and call count it was issued for:
///
/// * `lease_id` — replaying an attestation against another lease fails with
///   `AttestationMismatch`.
/// * `service_id` — an attestation issued for a service cannot settle a lease of
///   a different service.
/// * `verified_calls` — signed, so the provider cannot report a different count
///   than the attestor saw. `settle_lease_attested` rejects a payload whose
///   `verified_calls` differs from the call argument.
/// * `valid_until` — the last ledger timestamp at which the attestation may be
///   used (inclusive). A settlement attempt after it fails with
///   `AttestationStale`.
///
/// The attestation is an *additional* check, never a replacement for the
/// existing ones: expiry, allotment, ownership and single-settlement rules all
/// still apply.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageAttestation {
    /// The lease this attestation was issued for.
    pub lease_id: u64,
    /// The service that lease belongs to.
    pub service_id: u64,
    /// Calls the attestor observed (and signed) for that lease.
    pub verified_calls: u32,
    /// Last ledger timestamp at which this attestation is accepted, inclusive.
    pub valid_until: u64,
    /// Ed25519 signature over [`crate::usage_attestation_message`].
    pub signature: BytesN<64>,
}

/// A consumer's request to end a lease early with the provider's consent.
///
/// The request carries no refund amount: the provider approves it by reporting
/// the calls actually served, and the contract derives the refund from the
/// remaining calls at the stored rate.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelRequest {
    /// Lease the consumer wants to end early.
    pub lease_id: u64,
    /// Ledger timestamp at which the consumer requested cancellation.
    pub requested_at: u64,
}
