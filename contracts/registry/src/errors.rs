use soroban_sdk::contracterror;

/// Contract-level error codes returned by [`crate::KeyLeaseRegistry`].
///
/// Codes are stable and start at 1 (`0` is reserved by the host for success).
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
}
