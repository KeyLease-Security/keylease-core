use soroban_sdk::contracterror;

/** Contract-level error codes returned by [`crate::KeyLeaseRegistry`].
 * Codes are stable and start at 1 (`0` is reserved by the host for success).
 * New codes must be appended; codes are part of the ABI.
 */
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// `init` was called on an already-initialized registry.
    AlreadyInitialized = 1,
    /// The registry has not been initialized.
    NotInitialized = 2,
    /// The caller is not authorized for this operation.
    Unauthorized = 3,
    /// No service exists for the given `service_id`.
    ServiceNotFound = 4,
    /// The service is deactivated and cannot be leased.
    ServiceInactive = 5,
    /// No lease exists for the given `lease_id`.
    LeaseNotFound = 6,
    /// The lease has already been settled or revoked.
    LeaseAlreadySettled = 7,
    /// The lease cannot be settled yet: it has not expired and is not fully used.
    LeaseNotSettleable = 8,
    /// The lease has not reached its expiry timestamp.
    LeaseNotExpired = 9,
    /// The 24-hour post-expiry grace window for provider settlement has not elapsed.
    RevokeWindowNotElapsed = 10,
    /// Invalid call count (zero, or greater than the allocated allotment).
    InvalidCalls = 11,
    /// Invalid lease duration (must be greater than zero).
    InvalidDuration = 12,
    /// Invalid per-call rate (must be greater than zero).
    InvalidRate = 13,
    /// The escrow token address has not been configured.
    TokenNotSet = 14,
    /// Arithmetic overflow during rate or deposit math.
    Overflow = 15,
    /// Lease duration exceeds `MAX_LEASE_DURATION_SECS`, so its storage entry
    /// could not be kept alive for the whole term.
    DurationTooLong = 16,
    /// Deposit exceeds `MAX_LEASE_DEPOSIT`. Distinct from `Overflow`: the
    /// multiplication fit in `i128`, the amount is simply too large to escrow
    /// in a single lease.
    DepositTooLarge = 17,
    /// The service has no attestor configured, so `settle_lease_attested` is the
    /// wrong entry point for it. Plain `settle_lease` is.
    AttestorNotConfigured = 18,
    /// The service requires an attestation, and plain `settle_lease` cannot
    /// supply one.
    AttestationRequired = 19,
    /// The attestation is for a different lease, service or call count than the
    /// settlement it is presented with.
    AttestationMismatch = 20,
    /// The attestation's `valid_until` has already passed.
    AttestationStale = 21,
    /// There is no pending upgrade to execute or cancel.
    NoPendingUpgrade = 22,
    /// The proposed upgrade is still inside its mandatory delay window.
    UpgradeDelayNotElapsed = 23,
    /// A page size outside `1..=MAX_PAGE_SIZE`, so serving it would read an
    /// unbounded number of ledger entries in one transaction.
    InvalidLimit = 24,
    /// No early-cancellation request exists for the lease.
    CancellationNotRequested = 25,
    /// The lease already has a pending early-cancellation request.
    CancellationAlreadyRequested = 26,
    /// Early cancellation is only available before the lease expires.
    LeaseExpired = 27,
    /// Published `rate_per_call` exceeds `MAX_RATE_PER_CALL`. Appended after the
    /// early-cancellation codes, so every number already published keeps its
    /// meaning.
    RateTooHigh = 28,
}
