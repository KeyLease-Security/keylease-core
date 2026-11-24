use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events as _, Ledger as _},
    token, Address, BytesN, Env, Event, Vec,
};

use keylease_mock_token::{MockToken, MockTokenClient};

use crate::events::{
    Claimed, EscrowTokenSet, LeaseCreated, LeaseRevoked, LeaseSettled, LeaseToppedUp,
    ServiceReceiptKeySet, ServiceRegistered, ServiceStatusChanged,
};
use crate::{
    Error, KeyLeaseRegistry, KeyLeaseRegistryClient, LeaseStatus, MAX_BATCH_SETTLE,
    MAX_LEASE_DURATION_SECS,
};

/// Per-call rate used across the tests.
const RATE: i128 = 100;
/// Escrow funded to the consumer at setup.
const FUNDING: i128 = 1_000_000;
/// The 24h provider settlement grace period, mirrored from the contract.
const GRACE: u64 = 24 * 60 * 60;

struct Ctx<'a> {
    registry: KeyLeaseRegistryClient<'a>,
    token: MockTokenClient<'a>,
    admin: Address,
    provider: Address,
    consumer: Address,
}

impl Ctx<'_> {
    fn registry_id(&self) -> Address {
        self.registry.address.clone()
    }
}

/// Deploy a mock token and registry, wire them together, fund the consumer.
fn setup(env: &Env) -> Ctx<'_> {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let provider = Address::generate(env);
    let consumer = Address::generate(env);

    let token_id = env.register(MockToken, ());
    let token = MockTokenClient::new(env, &token_id);
    token.init(&admin);
    token.mint(&consumer, &FUNDING);

    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(env, &registry_id);
    registry.init(&admin);
    registry.set_token(&admin, &token_id);

    Ctx {
        registry,
        token,
        admin,
        provider,
        consumer,
    }
}

fn endpoint_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

fn register_default_service(env: &Env, ctx: &Ctx) -> u64 {
    ctx.registry
        .register_service(&ctx.provider, &RATE, &endpoint_hash(env))
}

// ---------------------------------------------------------------------------
// Usage-receipt signing (mirrors `usage_receipt_payload` in lib.rs)
// ---------------------------------------------------------------------------

const RECEIPT_SEED: [u8; 32] = [42u8; 32];

fn receipt_signing_key() -> SigningKey {
    SigningKey::from_bytes(&RECEIPT_SEED)
}

fn receipt_public_key(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &receipt_signing_key().verifying_key().to_bytes())
}

fn receipt_signature(
    env: &Env,
    registry_id: &Address,
    lease_id: u64,
    service_id: u64,
    verified_calls: u32,
) -> BytesN<64> {
    const DOMAIN: &[u8] = b"KeyLease/usage-receipt/v1";
    let address = registry_id.to_string().to_bytes();
    assert_eq!(address.len(), 56);

    let mut payload = [0u8; 101];
    let mut offset = DOMAIN.len();
    payload[..offset].copy_from_slice(DOMAIN);
    for i in 0..address.len() {
        payload[offset + i as usize] = address.get(i).unwrap();
    }
    offset += address.len() as usize;
    payload[offset..offset + 8].copy_from_slice(&lease_id.to_be_bytes());
    offset += 8;
    payload[offset..offset + 8].copy_from_slice(&service_id.to_be_bytes());
    offset += 8;
    payload[offset..offset + 4].copy_from_slice(&verified_calls.to_be_bytes());

    let signature = receipt_signing_key().sign(&payload).to_bytes();
    BytesN::from_array(env, &signature)
}

// ---------------------------------------------------------------------------
// Event helpers
// ---------------------------------------------------------------------------

/// Assert that the last contract invocation emitted exactly `event`, published by
/// `contract`, with the topics and payload `#[contractevent]` generates.
fn assert_single_event<E: Event>(env: &Env, contract: &Address, event: &E) {
    let expected = Vec::from_array(
        env,
        [(contract.clone(), event.topics(env), event.data(env))],
    );
    assert_eq!(env.events().all(), expected);
}

// ---------------------------------------------------------------------------
// Initialization & configuration
// ---------------------------------------------------------------------------

#[test]
fn test_init_configures_admin_and_token() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(ctx.registry.get_admin(), ctx.admin);
    assert_eq!(ctx.registry.get_token(), ctx.token.address);
    assert_eq!(ctx.registry.service_count(), 0);
    assert_eq!(ctx.registry.lease_count(), 0);
}

#[test]
fn test_double_init_is_rejected() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry.try_init(&ctx.admin),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn test_set_token_is_admin_only() {
    let env = Env::default();
    let ctx = setup(&env);
    let impostor = Address::generate(&env);

    assert_eq!(
        ctx.registry.try_set_token(&impostor, &ctx.token.address),
        Err(Ok(Error::Unauthorized))
    );
}

// ---------------------------------------------------------------------------
// Service registration
// ---------------------------------------------------------------------------

#[test]
fn test_register_service_assigns_incrementing_ids() {
    let env = Env::default();
    let ctx = setup(&env);

    let first = register_default_service(&env, &ctx);
    let second = register_default_service(&env, &ctx);

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(ctx.registry.service_count(), 2);

    let service = ctx.registry.get_service(&first);
    assert_eq!(service.service_id, first);
    assert_eq!(service.provider, ctx.provider);
    assert_eq!(service.rate_per_call, RATE);
    assert!(service.is_active);
    assert_eq!(service.endpoint_hash, endpoint_hash(&env));
}

#[test]
fn test_register_service_rejects_non_positive_rate() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry
            .try_register_service(&ctx.provider, &0, &endpoint_hash(&env)),
        Err(Ok(Error::InvalidRate))
    );
    assert_eq!(
        ctx.registry
            .try_register_service(&ctx.provider, &-1, &endpoint_hash(&env)),
        Err(Ok(Error::InvalidRate))
    );
    assert_eq!(ctx.registry.service_count(), 0);
}

#[test]
fn test_deactivated_service_rejects_new_leases() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &false);

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &10, &3_600),
        Err(Ok(Error::ServiceInactive))
    );

    // Reactivating restores leasing.
    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &true);
    assert_eq!(
        ctx.registry
            .create_lease(&ctx.consumer, &service_id, &10, &3_600),
        1
    );
}

#[test]
fn test_set_service_active_is_provider_only() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let impostor = Address::generate(&env);

    assert_eq!(
        ctx.registry
            .try_set_service_active(&impostor, &service_id, &false),
        Err(Ok(Error::Unauthorized))
    );
}

// ---------------------------------------------------------------------------
// Lease creation
// ---------------------------------------------------------------------------

#[test]
fn test_create_lease_locks_deposit_and_records_terms() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    assert_eq!(lease_id, 1);
    assert_eq!(ctx.registry.lease_count(), 1);

    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(lease.lease_id, lease_id);
    assert_eq!(lease.service_id, service_id);
    assert_eq!(lease.consumer, ctx.consumer);
    assert_eq!(lease.allocated_calls, 10);
    assert_eq!(lease.used_calls, 0);
    assert_eq!(lease.expires_at, 1_000 + 3_600);
    assert_eq!(lease.locked_deposit, RATE * 10);
    assert!(!lease.settled);

    // Deposit moved from consumer into escrow.
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);
    assert_eq!(ctx.registry.remaining_calls(&lease_id), 10);
    assert_eq!(ctx.registry.locked_balance(&lease_id), RATE * 10);
}

#[test]
fn test_create_lease_rejects_zero_calls() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &0, &3_600),
        Err(Ok(Error::InvalidCalls))
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

#[test]
fn test_create_lease_rejects_zero_duration() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &10, &0),
        Err(Ok(Error::InvalidDuration))
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

#[test]
fn test_create_lease_rejects_unknown_service() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &999, &10, &3_600),
        Err(Ok(Error::ServiceNotFound))
    );
}

#[test]
fn test_create_lease_requires_configured_token() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let consumer = Address::generate(&env);

    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(&env, &registry_id);
    registry.init(&admin);
    // Note: no set_token call.

    let service_id = registry.register_service(&provider, &RATE, &endpoint_hash(&env));

    assert_eq!(
        registry.try_create_lease(&consumer, &service_id, &10, &3_600),
        Err(Ok(Error::TokenNotSet))
    );
}

// ---------------------------------------------------------------------------
// Settlement
// ---------------------------------------------------------------------------

#[test]
fn test_full_usage_can_settle_before_expiry() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    // Not expired, but fully consumed -> settlement is allowed.
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);

    // Settlement records a pull-payment claim; tokens only move on `claim`.
    assert_eq!(ctx.token.balance(&ctx.provider), 0);
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 10);
    assert_eq!(ctx.registry.claimable_balance(&ctx.consumer), 0);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);

    ctx.registry.claim(&ctx.provider);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), 0);

    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(lease.used_calls, 10);
    assert!(lease.settled);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
}

#[test]
fn test_partial_settle_after_expiry_refunds_remainder() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    // Move past expiry (4_600) with a small buffer.
    env.ledger().set_timestamp(4_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &6);

    // Provider credited for 6 calls; consumer credited the remaining 4.
    assert_eq!(ctx.token.balance(&ctx.provider), 0);
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 6);
    assert_eq!(ctx.registry.claimable_balance(&ctx.consumer), RATE * 4);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);

    ctx.registry.claim(&ctx.provider);
    ctx.registry.claim(&ctx.consumer);

    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 6);
    assert_eq!(
        ctx.token.balance(&ctx.consumer),
        FUNDING - RATE * 10 + RATE * 4
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);

    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(lease.used_calls, 6);
    assert!(lease.settled);
}

#[test]
fn test_partial_settle_before_expiry_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    assert_eq!(
        ctx.registry.try_settle_lease(&ctx.provider, &lease_id, &4),
        Err(Ok(Error::LeaseNotSettleable))
    );
    // Escrow is untouched.
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);
}

#[test]
fn test_lease_cannot_be_settled_twice() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);

    assert_eq!(
        ctx.registry.try_settle_lease(&ctx.provider, &lease_id, &10),
        Err(Ok(Error::LeaseAlreadySettled))
    );
    // Provider was credited exactly once.
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 10);
    ctx.registry.claim(&ctx.provider);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
}

#[test]
fn test_only_owning_provider_can_settle() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let impostor = Address::generate(&env);
    assert_eq!(
        ctx.registry.try_settle_lease(&impostor, &lease_id, &10),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(ctx.token.balance(&impostor), 0);
}

#[test]
fn test_provider_cannot_over_claim_calls() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    assert_eq!(
        ctx.registry.try_settle_lease(&ctx.provider, &lease_id, &11),
        Err(Ok(Error::InvalidCalls))
    );
}

// ---------------------------------------------------------------------------
// Expired, unsettled reclamation
// ---------------------------------------------------------------------------

#[test]
fn test_expired_unsettled_lease_is_fully_reclaimed() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    // Expiry is at 1_100; exactly at the end of the 24h grace window.
    env.ledger().set_timestamp(1_100 + GRACE);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Expired);

    ctx.registry.revoke_expired_lease(&ctx.consumer, &lease_id);

    // Full deposit credited to the consumer; tokens move on claim.
    assert_eq!(ctx.registry.claimable_balance(&ctx.consumer), RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);

    ctx.registry.claim(&ctx.consumer);

    // Full deposit returned; provider earns nothing.
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.token.balance(&ctx.provider), 0);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);

    let lease = ctx.registry.get_lease(&lease_id);
    assert!(lease.settled);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
    assert_eq!(ctx.registry.remaining_calls(&lease_id), 0);
    assert_eq!(ctx.registry.locked_balance(&lease_id), 0);
}

#[test]
fn test_revoke_before_expiry_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_050);
    assert_eq!(
        ctx.registry
            .try_revoke_expired_lease(&ctx.consumer, &lease_id),
        Err(Ok(Error::LeaseNotExpired))
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);
}

#[test]
fn test_revoke_during_grace_window_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    // One second short of the grace window.
    env.ledger().set_timestamp(1_100 + GRACE - 1);
    assert_eq!(
        ctx.registry
            .try_revoke_expired_lease(&ctx.consumer, &lease_id),
        Err(Ok(Error::RevokeWindowNotElapsed))
    );
}

#[test]
fn test_only_consumer_can_revoke() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_100 + GRACE);
    let impostor = Address::generate(&env);
    assert_eq!(
        ctx.registry.try_revoke_expired_lease(&impostor, &lease_id),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_provider_can_settle_within_grace_window() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    // Provider settles late-but-in-time: expiry + 12h.
    env.ledger().set_timestamp(1_100 + GRACE / 2);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &3);

    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 3);
    assert_eq!(ctx.registry.claimable_balance(&ctx.consumer), RATE * 7);

    ctx.registry.claim(&ctx.provider);
    ctx.registry.claim(&ctx.consumer);

    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 3);
    assert_eq!(
        ctx.token.balance(&ctx.consumer),
        FUNDING - RATE * 10 + RATE * 7
    );

    // Consumer can no longer reclaim what was already settled.
    env.ledger().set_timestamp(1_100 + GRACE + 1);
    assert_eq!(
        ctx.registry
            .try_revoke_expired_lease(&ctx.consumer, &lease_id),
        Err(Ok(Error::LeaseAlreadySettled))
    );
}

// ---------------------------------------------------------------------------
// Rate math & overflow
// ---------------------------------------------------------------------------

#[test]
fn test_rate_math_overflow_reverts_deposit() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);

    // A rate that overflows for any call count > 1.
    let service_id = ctx
        .registry
        .register_service(&ctx.provider, &i128::MAX, &endpoint_hash(&env));

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &2, &3_600),
        Err(Ok(Error::Overflow))
    );

    // The consumer's funds were never moved into escrow.
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
    assert_eq!(ctx.registry.lease_count(), 0);
}

#[test]
fn test_large_but_valid_deposit_is_accepted() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);

    // (2^62) * 2 fits comfortably in i128, so the lease must be accepted.
    let rate: i128 = 1i128 << 62;
    let service_id = ctx
        .registry
        .register_service(&ctx.provider, &rate, &endpoint_hash(&env));

    // Fund the consumer for the full deposit (rate * 2).
    ctx.token.mint(&ctx.consumer, &(rate * 2));
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &2, &3_600);

    assert_eq!(ctx.registry.get_lease(&lease_id).locked_deposit, rate * 2);
}

#[test]
fn test_expiry_timestamp_overflow_reverts() {
    let env = Env::default();
    env.ledger().set_timestamp(u64::MAX);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &10, &1),
        Err(Ok(Error::Overflow))
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

// ---------------------------------------------------------------------------
// Status transitions
// ---------------------------------------------------------------------------

#[test]
fn test_lease_status_transitions() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Active);

    env.ledger().set_timestamp(4_600);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Expired);

    ctx.registry.settle_lease(&ctx.provider, &lease_id, &0);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
}

#[test]
fn test_lease_status_discriminants_are_stable() {
    let env = Env::default();
    use soroban_sdk::{IntoVal, TryFromVal, Val};

    // The ABI promises Active = 0, Expired = 1, Settled = 2.
    let active: Val = LeaseStatus::Active.into_val(&env);
    let expired: Val = LeaseStatus::Expired.into_val(&env);
    let settled: Val = LeaseStatus::Settled.into_val(&env);

    assert_eq!(u32::try_from_val(&env, &active).unwrap(), 0u32);
    assert_eq!(u32::try_from_val(&env, &expired).unwrap(), 1u32);
    assert_eq!(u32::try_from_val(&env, &settled).unwrap(), 2u32);
}

#[test]
fn test_lease_status_unknown_lease() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry.try_lease_status(&42),
        Err(Ok(Error::LeaseNotFound))
    );
}

// ---------------------------------------------------------------------------
// Soroban contract events
// ---------------------------------------------------------------------------

#[test]
fn test_set_token_emits_event() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &EscrowTokenSet {
            admin: ctx.admin.clone(),
            token: ctx.token.address.clone(),
        },
    );
}

#[test]
fn test_service_registration_emits_event() {
    let env = Env::default();
    let ctx = setup(&env);

    let service_id = register_default_service(&env, &ctx);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &ServiceRegistered {
            service_id,
            provider: ctx.provider.clone(),
            rate_per_call: RATE,
            endpoint_hash: endpoint_hash(&env),
        },
    );
}

#[test]
fn test_service_status_change_emits_event() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &false);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &ServiceStatusChanged {
            service_id,
            provider: ctx.provider.clone(),
            is_active: false,
        },
    );
}

#[test]
fn test_lease_lifecycle_emits_events() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseCreated {
            lease_id,
            service_id,
            consumer: ctx.consumer.clone(),
            allocated_calls: 10,
            expires_at: 4_600,
            locked_deposit: RATE * 10,
        },
    );

    env.ledger().set_timestamp(4_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &6);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseSettled {
            lease_id,
            provider: ctx.provider.clone(),
            verified_calls: 6,
            payout: RATE * 6,
            refund: RATE * 4,
        },
    );
}

#[test]
fn test_revoke_emits_event() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_100 + GRACE);
    ctx.registry.revoke_expired_lease(&ctx.consumer, &lease_id);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseRevoked {
            lease_id,
            consumer: ctx.consumer.clone(),
            refund: RATE * 10,
        },
    );
}

// ---------------------------------------------------------------------------
// Soroban lifecycle: metadata, upgrades, storage bounds
// ---------------------------------------------------------------------------

#[test]
fn test_version_matches_contract_metadata() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry.version(),
        soroban_sdk::String::from_str(&env, "0.3.0")
    );
}

#[test]
fn test_upgrade_is_admin_only() {
    let env = Env::default();
    let ctx = setup(&env);
    let impostor = Address::generate(&env);
    let new_wasm_hash = BytesN::from_array(&env, &[9u8; 32]);

    assert_eq!(
        ctx.registry.try_upgrade(&impostor, &new_wasm_hash),
        Err(Ok(Error::Unauthorized))
    );
}

// A successful upgrade installs a real Wasm uploaded with
// `stellar contract upload`, which the unit-test host cannot fabricate (it
// requires a valid `contractspecv0` metadata section). The success path is
// exercised end to end by `scripts/deploy-testnet.sh --upgrade` against
// Testnet; here we pin the authorization guard.

#[test]
fn test_duration_above_max_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    assert_eq!(
        ctx.registry.try_create_lease(
            &ctx.consumer,
            &service_id,
            &10,
            &(MAX_LEASE_DURATION_SECS + 1)
        ),
        Err(Ok(Error::DurationTooLong))
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

#[test]
fn test_max_duration_is_accepted() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let lease_id =
        ctx.registry
            .create_lease(&ctx.consumer, &service_id, &10, &MAX_LEASE_DURATION_SECS);
    assert_eq!(
        ctx.registry.get_lease(&lease_id).expires_at,
        1_000 + MAX_LEASE_DURATION_SECS
    );
}

// ---------------------------------------------------------------------------
// Stellar Asset Contract (SAC) integration
// ---------------------------------------------------------------------------

#[test]
fn test_escrow_runs_on_the_stellar_asset_contract() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let consumer = Address::generate(&env);

    // The same contract Stellar deploys for a wrapped native asset (XLM).
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let sac_id = sac.address();
    token::StellarAssetClient::new(&env, &sac_id).mint(&consumer, &FUNDING);

    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(&env, &registry_id);
    registry.init(&admin);
    registry.set_token(&admin, &sac_id);

    let service_id = registry.register_service(&provider, &RATE, &endpoint_hash(&env));
    let lease_id = registry.create_lease(&consumer, &service_id, &10, &3_600);

    let sac_client = token::Client::new(&env, &sac_id);
    assert_eq!(sac_client.balance(&consumer), FUNDING - RATE * 10);
    assert_eq!(sac_client.balance(&registry_id), RATE * 10);

    env.ledger().set_timestamp(4_601);
    registry.settle_lease(&provider, &lease_id, &4);

    // Settlement credits claims; the real SAC pays out on claim.
    assert_eq!(sac_client.balance(&provider), 0);
    assert_eq!(registry.claimable_balance(&provider), RATE * 4);
    assert_eq!(registry.claimable_balance(&consumer), RATE * 6);
    assert_eq!(sac_client.balance(&registry_id), RATE * 10);

    registry.claim(&provider);
    registry.claim(&consumer);

    assert_eq!(sac_client.balance(&provider), RATE * 4);
    assert_eq!(
        sac_client.balance(&consumer),
        FUNDING - RATE * 10 + RATE * 6
    );
    assert_eq!(sac_client.balance(&registry_id), 0);
}

// ---------------------------------------------------------------------------
// Re-entrancy hardening (checks-effects-interactions)
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
enum ReentryKey {
    Registry,
    Beneficiary,
    Armed,
    Blocked,
}

/// A hostile SEP-41-shaped token that re-enters `claim` from `transfer`.
///
/// It never moves value; it only records whether the re-entrant claim was
/// rejected. With checks-effects-interactions the balance entry is removed
/// before the transfer, so the second attempt must fail with `NothingToClaim`.
#[contract]
pub struct ReentrantToken;

#[contractimpl]
impl ReentrantToken {
    pub fn arm(env: Env, registry: Address, beneficiary: Address) {
        env.storage()
            .instance()
            .set(&ReentryKey::Registry, &registry);
        env.storage()
            .instance()
            .set(&ReentryKey::Beneficiary, &beneficiary);
        env.storage().instance().set(&ReentryKey::Armed, &true);
        env.storage().instance().set(&ReentryKey::Blocked, &false);
    }

    pub fn transfer(env: Env, _from: Address, _to: Address, _amount: i128) {
        let armed: bool = env
            .storage()
            .instance()
            .get(&ReentryKey::Armed)
            .unwrap_or(false);
        if !armed {
            return;
        }

        // Disarm so the attack is attempted exactly once per claim.
        env.storage().instance().set(&ReentryKey::Armed, &false);

        let registry: Address = env.storage().instance().get(&ReentryKey::Registry).unwrap();
        let beneficiary: Address = env
            .storage()
            .instance()
            .get(&ReentryKey::Beneficiary)
            .unwrap();

        let client = KeyLeaseRegistryClient::new(&env, &registry);
        let blocked = client.try_claim(&beneficiary).is_err();
        env.storage().instance().set(&ReentryKey::Blocked, &blocked);
    }

    pub fn blocked(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&ReentryKey::Blocked)
            .unwrap_or(false)
    }
}

#[test]
fn test_reentrant_token_cannot_claim_a_balance_twice() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);
    let consumer = Address::generate(&env);

    let token_id = env.register(ReentrantToken, ());
    let token_client = ReentrantTokenClient::new(&env, &token_id);

    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(&env, &registry_id);
    registry.init(&admin);
    registry.set_token(&admin, &token_id);

    let service_id = registry.register_service(&provider, &RATE, &endpoint_hash(&env));
    let lease_id = registry.create_lease(&consumer, &service_id, &10, &3_600);

    env.ledger().set_timestamp(4_601);
    registry.settle_lease(&provider, &lease_id, &10);

    token_client.arm(&registry_id, &provider);
    registry.claim(&provider);

    assert!(
        token_client.blocked(),
        "re-entrant claim should have been rejected"
    );
    assert_eq!(registry.claimable_balance(&provider), 0);
    assert!(registry.get_lease(&lease_id).settled);
}

// ---------------------------------------------------------------------------
// Lease top-up / renewal
// ---------------------------------------------------------------------------

#[test]
fn test_top_up_increases_calls_and_escrow() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry
        .top_up_lease(&ctx.consumer, &lease_id, &5, &600);

    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(lease.allocated_calls, 15);
    assert_eq!(lease.expires_at, 5_200);
    assert_eq!(lease.locked_deposit, RATE * 15);
    assert_eq!(lease.used_calls, 0);
    assert_eq!(ctx.registry.remaining_calls(&lease_id), 15);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 15);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 15);
}

#[test]
fn test_top_up_duration_only_keeps_deposit() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry
        .top_up_lease(&ctx.consumer, &lease_id, &0, &100);

    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(lease.allocated_calls, 10);
    assert_eq!(lease.expires_at, 4_700);
    assert_eq!(lease.locked_deposit, RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 10);
}

#[test]
fn test_top_up_emits_event() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry
        .top_up_lease(&ctx.consumer, &lease_id, &5, &600);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseToppedUp {
            lease_id,
            consumer: ctx.consumer.clone(),
            additional_calls: 5,
            additional_duration_secs: 600,
            new_expires_at: 5_200,
            added_deposit: RATE * 5,
        },
    );
}

#[test]
fn test_top_up_rejects_empty_parameters() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    assert_eq!(
        ctx.registry
            .try_top_up_lease(&ctx.consumer, &lease_id, &0, &0),
        Err(Ok(Error::InvalidTopUp))
    );
}

#[test]
fn test_top_up_requires_the_consumer() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let impostor = Address::generate(&env);

    assert_eq!(
        ctx.registry.try_top_up_lease(&impostor, &lease_id, &5, &0),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_top_up_rejects_settled_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);

    assert_eq!(
        ctx.registry
            .try_top_up_lease(&ctx.consumer, &lease_id, &5, &0),
        Err(Ok(Error::LeaseAlreadySettled))
    );
}

#[test]
fn test_top_up_rejects_expired_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_100);

    assert_eq!(
        ctx.registry
            .try_top_up_lease(&ctx.consumer, &lease_id, &5, &0),
        Err(Ok(Error::LeaseExpired))
    );
}

#[test]
fn test_top_up_cannot_exceed_max_duration() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id =
        ctx.registry
            .create_lease(&ctx.consumer, &service_id, &10, &MAX_LEASE_DURATION_SECS);

    assert_eq!(
        ctx.registry
            .try_top_up_lease(&ctx.consumer, &lease_id, &0, &1),
        Err(Ok(Error::DurationTooLong))
    );
}

#[test]
fn test_settlement_after_top_up_pays_the_original_rate() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry
        .top_up_lease(&ctx.consumer, &lease_id, &10, &3_600);
    assert_eq!(ctx.registry.get_lease(&lease_id).locked_deposit, RATE * 20);

    env.ledger().set_timestamp(8_201);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &6);

    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 6);
    assert_eq!(ctx.registry.claimable_balance(&ctx.consumer), RATE * 14);

    ctx.registry.claim(&ctx.provider);
    ctx.registry.claim(&ctx.consumer);

    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 6);
    assert_eq!(
        ctx.token.balance(&ctx.consumer),
        FUNDING - RATE * 20 + RATE * 14
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
}

// ---------------------------------------------------------------------------
// Bounded batch settlement
// ---------------------------------------------------------------------------

#[test]
fn test_batch_settlement_settles_multiple_leases() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let first = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let second = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &20, &3_600);
    let third = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &5, &3_600);

    let batch = Vec::from_array(&env, [(first, 10u32), (second, 20u32), (third, 5u32)]);
    ctx.registry.settle_leases(&ctx.provider, &batch);

    // Read events before any further contract call resets the event buffer.
    assert_eq!(env.events().all().events().len(), 3);

    for lease_id in [first, second, third] {
        assert!(ctx.registry.get_lease(&lease_id).settled);
    }
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 35);
    assert_eq!(ctx.registry.claimable_balance(&ctx.consumer), 0);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 35);

    ctx.registry.claim(&ctx.provider);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 35);
}

#[test]
fn test_batch_settlement_rejects_empty_batch() {
    let env = Env::default();
    let ctx = setup(&env);
    let batch: Vec<(u64, u32)> = Vec::new(&env);

    assert_eq!(
        ctx.registry.try_settle_leases(&ctx.provider, &batch),
        Err(Ok(Error::EmptyBatch))
    );
}

#[test]
fn test_batch_settlement_rejects_oversized_batch() {
    let env = Env::default();
    let ctx = setup(&env);

    let mut batch: Vec<(u64, u32)> = Vec::new(&env);
    for i in 0..=MAX_BATCH_SETTLE {
        batch.push_back((i as u64 + 1, 1u32));
    }
    assert_eq!(batch.len(), MAX_BATCH_SETTLE + 1);

    assert_eq!(
        ctx.registry.try_settle_leases(&ctx.provider, &batch),
        Err(Ok(Error::BatchTooLarge))
    );
}

#[test]
fn test_batch_settlement_rejects_duplicate_leases() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let batch = Vec::from_array(&env, [(lease_id, 10u32), (lease_id, 10u32)]);

    assert_eq!(
        ctx.registry.try_settle_leases(&ctx.provider, &batch),
        Err(Ok(Error::DuplicateLease))
    );
    assert!(!ctx.registry.get_lease(&lease_id).settled);
}

#[test]
fn test_batch_settlement_rejects_mixed_ownership_without_partial_state() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let own_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let other_provider = Address::generate(&env);
    let other_service = ctx
        .registry
        .register_service(&other_provider, &RATE, &endpoint_hash(&env));
    let other_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &other_service, &10, &3_600);

    let batch = Vec::from_array(&env, [(own_lease, 10u32), (other_lease, 10u32)]);

    assert_eq!(
        ctx.registry.try_settle_leases(&ctx.provider, &batch),
        Err(Ok(Error::Unauthorized))
    );
    assert!(!ctx.registry.get_lease(&own_lease).settled);
    assert!(!ctx.registry.get_lease(&other_lease).settled);
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), 0);
}

#[test]
fn test_batch_settlement_is_all_or_nothing() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let valid = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let batch = Vec::from_array(&env, [(valid, 10u32), (999u64, 1u32)]);

    assert_eq!(
        ctx.registry.try_settle_leases(&ctx.provider, &batch),
        Err(Ok(Error::LeaseNotFound))
    );
    assert!(!ctx.registry.get_lease(&valid).settled);
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), 0);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);
}

#[test]
fn test_batch_settlement_rejects_receipt_protected_services() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let batch = Vec::from_array(&env, [(lease_id, 10u32)]);

    assert_eq!(
        ctx.registry.try_settle_leases(&ctx.provider, &batch),
        Err(Ok(Error::ReceiptRequired))
    );
    assert!(!ctx.registry.get_lease(&lease_id).settled);
}

// ---------------------------------------------------------------------------
// Pull-payment claims
// ---------------------------------------------------------------------------

#[test]
fn test_claim_rejects_zero_balance() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry.try_claim(&ctx.consumer),
        Err(Ok(Error::NothingToClaim))
    );
}

#[test]
fn test_claim_is_single_use() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);
    ctx.registry.claim(&ctx.provider);

    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
    assert_eq!(
        ctx.registry.try_claim(&ctx.provider),
        Err(Ok(Error::NothingToClaim))
    );
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
}

#[test]
fn test_claims_accumulate_across_leases() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let first = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let second = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.settle_lease(&ctx.provider, &first, &10);
    ctx.registry.settle_lease(&ctx.provider, &second, &10);

    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 20);
    ctx.registry.claim(&ctx.provider);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 20);
}

#[test]
fn test_claims_conserve_value() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    env.ledger().set_timestamp(4_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &6);

    assert_eq!(
        ctx.registry.claimable_balance(&ctx.provider)
            + ctx.registry.claimable_balance(&ctx.consumer),
        RATE * 10
    );

    ctx.registry.claim(&ctx.provider);
    ctx.registry.claim(&ctx.consumer);

    assert_eq!(
        ctx.token.balance(&ctx.provider) + ctx.token.balance(&ctx.consumer),
        FUNDING
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
}

#[test]
fn test_claim_emits_event() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);
    ctx.registry.claim(&ctx.provider);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &Claimed {
            beneficiary: ctx.provider.clone(),
            amount: RATE * 10,
        },
    );
}

// ---------------------------------------------------------------------------
// Optional signed usage receipts
// ---------------------------------------------------------------------------

#[test]
fn test_receipt_key_change_emits_event() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let key = receipt_public_key(&env);

    ctx.registry
        .set_service_receipt_key(&ctx.provider, &service_id, &Some(key.clone()));

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &ServiceReceiptKeySet {
            service_id,
            provider: ctx.provider.clone(),
            receipt_key: Some(key),
        },
    );
}

#[test]
fn test_receipt_key_is_provider_only() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let impostor = Address::generate(&env);

    assert_eq!(
        ctx.registry.try_set_service_receipt_key(
            &impostor,
            &service_id,
            &Some(receipt_public_key(&env)),
        ),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_receipt_settlement_accepts_valid_signature() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let receipt = receipt_signature(&env, &ctx.registry_id(), lease_id, service_id, 10);
    ctx.registry
        .settle_lease_with_receipt(&ctx.provider, &lease_id, &10, &receipt);

    let lease = ctx.registry.get_lease(&lease_id);
    assert!(lease.settled);
    assert_eq!(lease.used_calls, 10);
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 10);
}

#[test]
fn test_receipt_settlement_rejects_invalid_signature() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let forged = BytesN::from_array(&env, &[0u8; 64]);
    assert!(
        ctx.registry
            .try_settle_lease_with_receipt(&ctx.provider, &lease_id, &10, &forged)
            .is_err(),
        "a forged receipt must be rejected"
    );
    assert!(!ctx.registry.get_lease(&lease_id).settled);
}

#[test]
fn test_receipt_is_bound_to_its_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    let first = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let second = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let first_receipt = receipt_signature(&env, &ctx.registry_id(), first, service_id, 10);
    assert!(
        ctx.registry
            .try_settle_lease_with_receipt(&ctx.provider, &second, &10, &first_receipt)
            .is_err(),
        "a receipt for one lease must not settle another"
    );
}

#[test]
fn test_receipt_is_bound_to_the_reported_call_count() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let receipt = receipt_signature(&env, &ctx.registry_id(), lease_id, service_id, 10);

    env.ledger().set_timestamp(4_601);
    assert!(
        ctx.registry
            .try_settle_lease_with_receipt(&ctx.provider, &lease_id, &6, &receipt)
            .is_err(),
        "the receipt must cover the exact call count being settled"
    );
}

#[test]
fn test_receipt_cannot_be_replayed() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let receipt = receipt_signature(&env, &ctx.registry_id(), lease_id, service_id, 10);
    ctx.registry
        .settle_lease_with_receipt(&ctx.provider, &lease_id, &10, &receipt);

    assert_eq!(
        ctx.registry
            .try_settle_lease_with_receipt(&ctx.provider, &lease_id, &10, &receipt),
        Err(Ok(Error::LeaseAlreadySettled))
    );
    assert_eq!(ctx.registry.claimable_balance(&ctx.provider), RATE * 10);
}

#[test]
fn test_unsigned_settlement_is_rejected_for_receipt_services() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    assert_eq!(
        ctx.registry.try_settle_lease(&ctx.provider, &lease_id, &10),
        Err(Ok(Error::ReceiptRequired))
    );
}

#[test]
fn test_receipt_rejected_when_service_is_not_configured() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let receipt = receipt_signature(&env, &ctx.registry_id(), lease_id, service_id, 10);

    assert_eq!(
        ctx.registry
            .try_settle_lease_with_receipt(&ctx.provider, &lease_id, &10, &receipt),
        Err(Ok(Error::ReceiptNotConfigured))
    );
}

#[test]
fn test_clearing_receipt_key_restores_unsigned_settlement() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &service_id,
        &Some(receipt_public_key(&env)),
    );
    ctx.registry
        .set_service_receipt_key(&ctx.provider, &service_id, &None);

    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);

    assert!(ctx.registry.get_lease(&lease_id).settled);
}

#[test]
fn test_receipt_mode_is_per_service() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let receipt_service = register_default_service(&env, &ctx);
    let unsigned_service = register_default_service(&env, &ctx);

    ctx.registry.set_service_receipt_key(
        &ctx.provider,
        &receipt_service,
        &Some(receipt_public_key(&env)),
    );

    let unsigned_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &unsigned_service, &10, &3_600);
    ctx.registry
        .settle_lease(&ctx.provider, &unsigned_lease, &10);

    let receipt_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &receipt_service, &10, &3_600);
    let receipt = receipt_signature(&env, &ctx.registry_id(), receipt_lease, receipt_service, 10);
    ctx.registry
        .settle_lease_with_receipt(&ctx.provider, &receipt_lease, &10, &receipt);

    assert!(ctx.registry.get_lease(&unsigned_lease).settled);
    assert!(ctx.registry.get_lease(&receipt_lease).settled);
}
