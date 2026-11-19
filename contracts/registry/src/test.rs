use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{
        storage::Instance as _, Address as _, Events as _, Ledger as _, MockAuth, MockAuthInvoke,
    },
    token, xdr, Address, BytesN, Env, Event, IntoVal, Map, Symbol, TryFromVal, Val, Vec,
};

use keylease_mock_token::{MockToken, MockTokenClient};

use crate::events::{
    EscrowTokenSet, LeaseCreated, LeaseDelegated, LeaseRevoked, LeaseSettled, ServiceRegistered,
    ServiceStatusChanged, EVENT_SCHEMA_VERSION,
};
use crate::storage::{self, LEDGER_BUMP_EXTEND, LEDGER_BUMP_THRESHOLD, MAX_ENTRY_TTL_LEDGERS};
use crate::{
    Error, KeyLeaseRegistry, KeyLeaseRegistryClient, Lease, LeaseStatus, ServiceStats,
    MAX_LEASE_DEPOSIT, MAX_LEASE_DURATION_SECS, MAX_RATE_PER_CALL,
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
fn test_set_token_first_time_and_change_are_recorded() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(&env, &registry_id);
    registry.init(&admin);

    // First configuration: stored and announced. Assert the event before any
    // other contract call, because `env.events()` reflects the last invocation.
    let first = env.register(MockToken, ());
    registry.set_token(&admin, &first);
    assert_single_event(
        &env,
        &registry_id,
        &EscrowTokenSet {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: admin.clone(),
            token: first.clone(),
        },
    );
    assert_eq!(registry.get_token(), first);

    // A genuine migration to a different token is still recorded and announced.
    let second = env.register(MockToken, ());
    registry.set_token(&admin, &second);
    assert_single_event(
        &env,
        &registry_id,
        &EscrowTokenSet {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: admin.clone(),
            token: second.clone(),
        },
    );
    assert_eq!(registry.get_token(), second);
}

#[test]
fn test_set_token_is_a_noop_when_unchanged() {
    let env = Env::default();
    let ctx = setup(&env);

    let registry_id = ctx.registry_id();
    let ttl_before = env.as_contract(&registry_id, || env.storage().instance().get_ttl());

    // `setup` already configured this exact token; repeating it must not write
    // storage, bump the TTL, or publish a misleading `escrow_token_set` event.
    ctx.registry.set_token(&ctx.admin, &ctx.token.address);

    // Check the event first: another contract call would replace this buffer.
    assert_eq!(env.events().all(), Vec::new(&env));
    let ttl_after = env.as_contract(&registry_id, || env.storage().instance().get_ttl());
    assert_eq!(ttl_after, ttl_before);
    assert_eq!(ctx.registry.get_token(), ctx.token.address);
}

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
// Delegated lease creation
// ---------------------------------------------------------------------------

#[test]
fn test_delegate_can_create_lease_for_consumer() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let delegate = Address::generate(&env);

    let lease_id =
        ctx.registry
            .create_lease_for(&delegate, &ctx.consumer, &service_id, &10, &3_600);

    // `env.events().all()` only covers the *last* invocation, so snapshot the
    // creation's events before any read call overwrites them.
    let events = env.events().all();

    // The lease is the consumer's, and the consumer alone was debited.
    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(lease.consumer, ctx.consumer);
    assert_eq!(lease.allocated_calls, 10);
    assert!(!lease.settled);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 10);
    assert_eq!(ctx.token.balance(&delegate), 0);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);

    // `lease_created` is unchanged; `lease_delegated` records the courier.
    // Both payloads carry the schema version, so an indexer that filters on the
    // delegated event decodes the same fields as the core event.
    let created = LeaseCreated {
        lease_id,
        schema_version: EVENT_SCHEMA_VERSION,
        service_id,
        consumer: ctx.consumer.clone(),
        allocated_calls: 10,
        expires_at: 4_600,
        locked_deposit: RATE * 10,
    };
    let delegated = LeaseDelegated {
        lease_id,
        schema_version: EVENT_SCHEMA_VERSION,
        delegate: delegate.clone(),
        consumer: ctx.consumer.clone(),
    };
    let expected = Vec::from_array(
        &env,
        [
            (ctx.registry_id(), created.topics(&env), created.data(&env)),
            (
                ctx.registry_id(),
                delegated.topics(&env),
                delegated.data(&env),
            ),
        ],
    );
    assert_eq!(events, expected);
}

#[test]
fn test_delegate_cannot_create_lease_without_consumer_authorization() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let delegate = Address::generate(&env);
    let registry_id = ctx.registry_id();

    // `setup` mocked every auth; this hands the host exactly one signature —
    // the delegate's — so anything requiring the consumer must now fail.
    env.mock_auths(&[MockAuth {
        address: &delegate,
        invoke: &MockAuthInvoke {
            contract: &registry_id,
            fn_name: "create_lease_for",
            args: (&delegate, &ctx.consumer, &service_id, &10u32, &3_600u64).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let result =
        ctx.registry
            .try_create_lease_for(&delegate, &ctx.consumer, &service_id, &10, &3_600);

    assert!(
        result.is_err(),
        "a delegate must not lock a consumer's deposit without the consumer's signature"
    );
    assert_eq!(ctx.registry.lease_count(), 0);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
}

#[test]
fn test_consumer_alone_cannot_create_a_delegated_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let delegate = Address::generate(&env);
    let registry_id = ctx.registry_id();

    // The mirror case: the consumer signed, but the named delegate never did,
    // so the transaction that pays the fee cannot be attributed to it.
    env.mock_auths(&[MockAuth {
        address: &ctx.consumer,
        invoke: &MockAuthInvoke {
            contract: &registry_id,
            fn_name: "create_lease_for",
            args: (&delegate, &ctx.consumer, &service_id, &10u32, &3_600u64).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let result =
        ctx.registry
            .try_create_lease_for(&delegate, &ctx.consumer, &service_id, &10, &3_600);

    assert!(
        result.is_err(),
        "create_lease_for must authenticate the delegate too"
    );
    assert_eq!(ctx.registry.lease_count(), 0);
}

#[test]
fn test_delegated_lease_refund_belongs_to_the_consumer() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let delegate = Address::generate(&env);

    let lease_id = ctx
        .registry
        .create_lease_for(&delegate, &ctx.consumer, &service_id, &10, &100);

    // The delegate is not on the lease and cannot reclaim it.
    env.ledger().set_timestamp(1_100 + GRACE);
    assert_eq!(
        ctx.registry.try_revoke_expired_lease(&delegate, &lease_id),
        Err(Ok(Error::Unauthorized))
    );

    ctx.registry.revoke_expired_lease(&ctx.consumer, &lease_id);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.token.balance(&delegate), 0);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Revoked);
}

#[test]
fn test_delegated_creation_enforces_the_same_preconditions() {
    let env = Env::default();
    let ctx = setup(&env);
    let delegate = Address::generate(&env);

    // The delegate path shares the creation checks, it does not bypass them.
    assert_eq!(
        ctx.registry
            .try_create_lease_for(&delegate, &ctx.consumer, &999, &10, &3_600),
        Err(Ok(Error::ServiceNotFound))
    );
    let service_id = register_default_service(&env, &ctx);
    assert_eq!(
        ctx.registry
            .try_create_lease_for(&delegate, &ctx.consumer, &service_id, &0, &3_600),
        Err(Ok(Error::InvalidCalls))
    );
    assert_eq!(
        ctx.registry
            .try_create_lease_for(&delegate, &ctx.consumer, &service_id, &10, &0),
        Err(Ok(Error::InvalidDuration))
    );
    assert_eq!(ctx.registry.lease_count(), 0);
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

    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);

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

    // Provider paid for 6 calls; consumer refunded the remaining 4.
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
    // Provider was paid exactly once.
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

    // Full deposit returned; provider earns nothing.
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.token.balance(&ctx.provider), 0);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);

    let lease = ctx.registry.get_lease(&lease_id);
    assert!(lease.settled);
    // Revocation is its own terminal status, not a generic settlement.
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Revoked);
    // The provider earned nothing, and `used_calls` reads that back.
    assert_eq!(lease.used_calls, 0);
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
// Service settlement stats
// ---------------------------------------------------------------------------

#[test]
fn test_service_stats_start_at_zero_and_reject_unknown_service() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // A service nobody has settled yet reads as zero, not as missing.
    assert_eq!(
        ctx.registry.service_stats(&service_id),
        ServiceStats::zero()
    );
    assert_eq!(
        ctx.registry.try_service_stats(&999),
        Err(Ok(Error::ServiceNotFound))
    );
}

#[test]
fn test_service_stats_accumulate_settled_leases_calls_and_payouts() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // Fully consumed on the first lease...
    let first = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    ctx.registry.settle_lease(&ctx.provider, &first, &10);

    // ...and a partial settlement after expiry on the second, so the counter
    // must record the calls the provider was paid for, not the allotment.
    let second = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    env.ledger().set_timestamp(4_601);
    ctx.registry.settle_lease(&ctx.provider, &second, &4);

    assert_eq!(
        ctx.registry.service_stats(&service_id),
        ServiceStats {
            settled_leases: 2,
            settled_calls: 14,
            total_payout: RATE * 14,
        }
    );
    // The aggregate equals what actually reached the provider.
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 14);
}

#[test]
fn test_service_stats_record_a_zero_call_settlement() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    // Past expiry, so a zero-call settlement is allowed: the provider refunds
    // the whole deposit and still counts as having settled the lease.
    env.ledger().set_timestamp(4_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &0);

    assert_eq!(
        ctx.registry.service_stats(&service_id),
        ServiceStats {
            settled_leases: 1,
            settled_calls: 0,
            total_payout: 0,
        }
    );
}

#[test]
fn test_revoked_lease_does_not_count_toward_service_stats() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let settled = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    ctx.registry.settle_lease(&ctx.provider, &settled, &10);

    // A second lease the provider never settles: the consumer reclaims it in
    // full after expiry plus the grace window.
    let revoked = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &7, &100);
    env.ledger().set_timestamp(1_100 + GRACE);
    ctx.registry.revoke_expired_lease(&ctx.consumer, &revoked);

    assert_eq!(
        ctx.registry.service_stats(&service_id),
        ServiceStats {
            settled_leases: 1,
            settled_calls: 10,
            total_payout: RATE * 10,
        }
    );
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
}

#[test]
fn test_service_stats_are_scoped_per_service() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let first_service = register_default_service(&env, &ctx);
    let second_service =
        ctx.registry
            .register_service(&ctx.provider, &(RATE * 2), &endpoint_hash(&env));

    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &first_service, &10, &3_600);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);

    assert_eq!(ctx.registry.service_stats(&first_service).settled_leases, 1);
    assert_eq!(
        ctx.registry.service_stats(&second_service),
        ServiceStats::zero()
    );
}

// ---------------------------------------------------------------------------
// Rate bound and deposit arithmetic
// ---------------------------------------------------------------------------

#[test]
fn test_rate_above_max_is_rejected_at_registration() {
    let env = Env::default();
    let ctx = setup(&env);

    // One over the bound, and the old unbounded extreme, both fail here.
    assert_eq!(
        ctx.registry.try_register_service(
            &ctx.provider,
            &(MAX_RATE_PER_CALL + 1),
            &endpoint_hash(&env)
        ),
        Err(Ok(Error::RateTooHigh))
    );
    assert_eq!(
        ctx.registry
            .try_register_service(&ctx.provider, &i128::MAX, &endpoint_hash(&env)),
        Err(Ok(Error::RateTooHigh))
    );
    assert_eq!(ctx.registry.service_count(), 0);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

#[test]
fn test_max_rate_is_accepted_and_leasable() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);

    let service_id =
        ctx.registry
            .register_service(&ctx.provider, &MAX_RATE_PER_CALL, &endpoint_hash(&env));
    assert_eq!(
        ctx.registry.get_service(&service_id).rate_per_call,
        MAX_RATE_PER_CALL
    );

    // The largest rate is still a service somebody can actually buy, which is
    // the property `MAX_RATE_PER_CALL = MAX_LEASE_DEPOSIT` exists to guarantee:
    // one call at the ceiling is exactly the largest fundable deposit.
    ctx.token.mint(&ctx.consumer, &MAX_RATE_PER_CALL);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &1, &3_600);

    assert_eq!(
        ctx.registry.get_lease(&lease_id).locked_deposit,
        MAX_RATE_PER_CALL
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), MAX_RATE_PER_CALL);

    // Settling the whole allotment pays the same figure back out.
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &1);
    assert_eq!(ctx.token.balance(&ctx.provider), MAX_RATE_PER_CALL);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
}

#[test]
fn test_rate_bound_keeps_deposit_math_inside_i128() {
    // The bound is defined as the deposit cap, so the worst product is the cap
    // times the largest `u32`: 10^12 * 4.3 x 10^9 < 2^72, against an `i128`
    // ceiling of 2^127. The lease-time multiplication cannot overflow.
    let worst_deposit = MAX_RATE_PER_CALL.checked_mul(u32::MAX as i128);
    assert!(
        worst_deposit.is_some(),
        "MAX_RATE_PER_CALL must keep every allowed deposit inside i128"
    );
    assert!(worst_deposit.unwrap() < i128::MAX);
}

#[test]
fn test_large_but_valid_deposit_is_accepted() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);

    // This test used `(1i128 << 62) * 2`, which fits `i128` with room to spare.
    // `MAX_LEASE_DEPOSIT` now sits far below that, so "large but valid" means
    // "large and still inside the bound": one unit under the cap, which is the
    // last amount the protocol accepts. The cap itself and `cap + 1` are covered
    // by the `MAX_LEASE_DEPOSIT` tests below.
    let rate: i128 = MAX_LEASE_DEPOSIT - 1;
    let service_id = ctx
        .registry
        .register_service(&ctx.provider, &rate, &endpoint_hash(&env));

    // Fund the consumer for the full deposit (rate * 1).
    ctx.token.mint(&ctx.consumer, &rate);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &1, &3_600);

    assert_eq!(ctx.registry.get_lease(&lease_id).locked_deposit, rate);
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

#[test]
fn test_error_codes_are_appended_not_renumbered() {
    // The `#[contracterror]` numbers are what clients switch on, so the
    // append-only rule needs a test rather than a comment: everything that
    // predates this PR keeps its number and `RateTooHigh` is the new tail.
    assert_eq!(Error::InvalidCalls as u32, 11);
    assert_eq!(Error::TokenNotSet as u32, 14);
    assert_eq!(Error::Overflow as u32, 15);
    assert_eq!(Error::DurationTooLong as u32, 16);
    assert_eq!(Error::DepositTooLarge as u32, 17);
    assert_eq!(Error::RateTooHigh as u32, 18);
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

    // The ABI promises Active = 0, Expired = 1, Settled = 2, and `Revoked` is
    // appended as 3 — nothing renumbered, so old clients still decode history.
    let active: Val = LeaseStatus::Active.into_val(&env);
    let expired: Val = LeaseStatus::Expired.into_val(&env);
    let settled: Val = LeaseStatus::Settled.into_val(&env);
    let revoked: Val = LeaseStatus::Revoked.into_val(&env);

    assert_eq!(u32::try_from_val(&env, &active).unwrap(), 0u32);
    assert_eq!(u32::try_from_val(&env, &expired).unwrap(), 1u32);
    assert_eq!(u32::try_from_val(&env, &settled).unwrap(), 2u32);
    assert_eq!(u32::try_from_val(&env, &revoked).unwrap(), 3u32);
}

#[test]
fn test_lease_status_of_separates_revocation_from_settlement() {
    let env = Env::default();
    let consumer = Address::generate(&env);

    // Pure view over the stored lease plus the registry's revocation marker.
    let lease = |settled: bool| Lease {
        lease_id: 1,
        service_id: 1,
        consumer: consumer.clone(),
        allocated_calls: 10,
        used_calls: 0,
        expires_at: 4_600,
        locked_deposit: RATE * 10,
        settled,
    };

    assert_eq!(
        LeaseStatus::of(&lease(false), 1_000, false),
        LeaseStatus::Active
    );
    assert_eq!(
        LeaseStatus::of(&lease(false), 4_600, false),
        LeaseStatus::Expired
    );
    assert_eq!(
        LeaseStatus::of(&lease(true), 1_000, false),
        LeaseStatus::Settled
    );
    assert_eq!(
        LeaseStatus::of(&lease(true), 4_600, false),
        LeaseStatus::Settled
    );
    // A reclaimed lease is `Revoked` regardless of the timestamp it is read at:
    // the funding path, not the clock, decides the terminal status.
    assert_eq!(
        LeaseStatus::of(&lease(true), 1_000, true),
        LeaseStatus::Revoked
    );
    assert_eq!(
        LeaseStatus::of(&lease(true), 9_999, true),
        LeaseStatus::Revoked
    );
}

#[test]
fn test_revoked_lease_is_not_reported_as_settled() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // Same service, same terms; only who closes the lease differs.
    let settled_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let revoked_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry
        .settle_lease(&ctx.provider, &settled_lease, &10);

    env.ledger().set_timestamp(4_600 + GRACE);
    ctx.registry
        .revoke_expired_lease(&ctx.consumer, &revoked_lease);

    assert_eq!(
        ctx.registry.lease_status(&settled_lease),
        LeaseStatus::Settled
    );
    assert_eq!(
        ctx.registry.lease_status(&revoked_lease),
        LeaseStatus::Revoked
    );
    assert_ne!(
        ctx.registry.lease_status(&settled_lease),
        ctx.registry.lease_status(&revoked_lease),
        "a reclaimed deposit must not read as provider revenue"
    );

    // The stored `settled` flag stays true for both: the lease struct layout is
    // frozen, so `Revoked` is carried by the marker, not by a new field.
    assert!(ctx.registry.get_lease(&settled_lease).settled);
    assert!(ctx.registry.get_lease(&revoked_lease).settled);
}

#[test]
fn test_settle_after_revoke_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_100 + GRACE);
    ctx.registry.revoke_expired_lease(&ctx.consumer, &lease_id);

    // A provider that arrives after the consumer reclaimed the escrow gets
    // nothing, and the lease stays `Revoked`.
    assert_eq!(
        ctx.registry.try_settle_lease(&ctx.provider, &lease_id, &10),
        Err(Ok(Error::LeaseAlreadySettled))
    );
    assert_eq!(ctx.token.balance(&ctx.provider), 0);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Revoked);
    assert_eq!(
        ctx.registry.service_stats(&service_id),
        ServiceStats::zero()
    );
}

#[test]
fn test_revoke_after_settle_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    // Provider settles the full usage before expiry, then the grace window
    // opens: the consumer must not be able to convert a paid lease into a
    // refund, and the status must remain `Settled`.
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);
    env.ledger().set_timestamp(1_100 + GRACE);

    assert_eq!(
        ctx.registry
            .try_revoke_expired_lease(&ctx.consumer, &lease_id),
        Err(Ok(Error::LeaseAlreadySettled))
    );
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
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
            schema_version: EVENT_SCHEMA_VERSION,
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
            schema_version: EVENT_SCHEMA_VERSION,
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
            schema_version: EVENT_SCHEMA_VERSION,
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
            schema_version: EVENT_SCHEMA_VERSION,
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
            schema_version: EVENT_SCHEMA_VERSION,
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
            schema_version: EVENT_SCHEMA_VERSION,
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
// Maximum deposit bound (MAX_LEASE_DEPOSIT)
// ---------------------------------------------------------------------------

/// Register a service at an explicit rate and return its id. The deposit bound
/// lives in the `rate × requested_calls` product, so these tests need rates far
/// outside the `RATE` the rest of the suite uses.
fn register_service_at(env: &Env, ctx: &Ctx, rate: i128) -> u64 {
    ctx.registry
        .register_service(&ctx.provider, &rate, &endpoint_hash(env))
}

#[test]
fn test_deposit_exactly_at_max_is_accepted() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_service_at(&env, &ctx, MAX_LEASE_DEPOSIT);
    ctx.token.mint(&ctx.consumer, &MAX_LEASE_DEPOSIT);

    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &1, &3_600);

    assert_eq!(
        ctx.registry.get_lease(&lease_id).locked_deposit,
        MAX_LEASE_DEPOSIT
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

#[test]
fn test_deposit_one_above_max_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    // A rate of `MAX_LEASE_DEPOSIT + 1` can no longer be listed at all —
    // `MAX_RATE_PER_CALL` equals the deposit cap — so the product is composed
    // instead: 1_000_000_000_001 = 10_001 x 99_990_001, one unit over the cap
    // from two prices the protocol both accepts.
    let service_id = register_service_at(&env, &ctx, 10_001);
    let calls: u32 = 99_990_001;
    // Fund the lease the bound is meant to stop: the consumer could otherwise
    // afford it, and a passing assertion would only be proving poverty.
    ctx.token.mint(&ctx.consumer, &(MAX_LEASE_DEPOSIT + 1));
    let funded = ctx.token.balance(&ctx.consumer);
    let leases_before = ctx.registry.lease_count();

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &calls, &3_600),
        Err(Ok(Error::DepositTooLarge))
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), funded);
    assert_eq!(
        ctx.registry.lease_count(),
        leases_before,
        "the bound must reject before the lease id is allocated"
    );
}

#[test]
fn test_deposit_bound_applies_to_the_product_not_the_rate() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    // Every input is well under the bound; only their product crosses it, which
    // is the case a rate-only check would let through.
    let service_id = register_service_at(&env, &ctx, 1_000_000);
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &1_000_001, &3_600),
        Err(Ok(Error::DepositTooLarge))
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

#[test]
fn test_the_two_bounds_fire_at_different_entry_points() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    // `MAX_RATE_PER_CALL` guards the listing and `MAX_LEASE_DEPOSIT` guards the
    // lease, so the extreme rate this test used to reach through
    // `create_lease` is now stopped one call earlier and for a different
    // reason. The two errors stay distinguishable for clients, and the
    // multiplication itself can no longer overflow: the worst legal product is
    // `MAX_RATE_PER_CALL * u32::MAX < 2^72`, inside `i128` by ~10^17.
    assert_eq!(
        ctx.registry
            .try_register_service(&ctx.provider, &(i128::MAX / 2), &endpoint_hash(&env)),
        Err(Ok(Error::RateTooHigh))
    );
    assert_eq!(ctx.registry.service_count(), 0);

    // A product that crosses the deposit cap while both inputs stay legal is
    // still `DepositTooLarge`, not an arithmetic failure.
    let service_id = register_service_at(&env, &ctx, MAX_RATE_PER_CALL);
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &2, &3_600),
        Err(Ok(Error::DepositTooLarge))
    );
    // Neither rejection moved escrow: this is the claim `#160`'s
    // `test_rate_math_overflow_reverts_deposit` made through `Error::Overflow`,
    // which the price bound now makes unreachable.
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
    assert_eq!(ctx.registry.lease_count(), 0);
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
    Provider,
    LeaseId,
    Armed,
    Blocked,
}

/// A hostile SEP-41-shaped token that re-enters `settle_lease` from `transfer`.
///
/// It never moves value; it only records whether the re-entrant settlement was
/// rejected. With checks-effects-interactions the second attempt must fail with
/// `LeaseAlreadySettled` before any payout is attempted.
#[contract]
pub struct ReentrantToken;

#[contractimpl]
impl ReentrantToken {
    pub fn arm(env: Env, registry: Address, provider: Address, lease_id: u64) {
        env.storage()
            .instance()
            .set(&ReentryKey::Registry, &registry);
        env.storage()
            .instance()
            .set(&ReentryKey::Provider, &provider);
        env.storage()
            .instance()
            .set(&ReentryKey::LeaseId, &lease_id);
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

        // Disarm so the attack is attempted exactly once per settlement.
        env.storage().instance().set(&ReentryKey::Armed, &false);

        let registry: Address = env.storage().instance().get(&ReentryKey::Registry).unwrap();
        let provider: Address = env.storage().instance().get(&ReentryKey::Provider).unwrap();
        let lease_id: u64 = env.storage().instance().get(&ReentryKey::LeaseId).unwrap();

        let client = KeyLeaseRegistryClient::new(&env, &registry);
        let blocked = client.try_settle_lease(&provider, &lease_id, &0).is_err();
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
fn test_reentrant_token_cannot_settle_a_lease_twice() {
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

    token_client.arm(&registry_id, &provider, &lease_id);
    env.ledger().set_timestamp(4_601);
    registry.settle_lease(&provider, &lease_id, &10);

    assert!(
        token_client.blocked(),
        "re-entrant settle_lease should have been rejected"
    );
    assert!(registry.get_lease(&lease_id).settled);
}

// ---------------------------------------------------------------------------
// Event schema versioning
// ---------------------------------------------------------------------------

/// The `schema_version` an indexer actually sees: decoded out of the event's
/// XDR payload map rather than from the Rust struct that produced it.
fn wire_schema_version(env: &Env, event: &xdr::ContractEvent) -> Option<u32> {
    let xdr::ContractEventBody::V0(body) = &event.body;
    let data = Val::try_from_val(env, &body.data).ok()?;
    let payload = Map::<Symbol, Val>::try_from_val(env, &data).ok()?;
    let raw = payload.get(Symbol::new(env, "schema_version"))?;
    u32::try_from_val(env, &raw).ok()
}

/// Assert every event the registry published in the last invocation carries the
/// current schema version in its payload. Panics when the registry published
/// nothing, so this can never pass by accident.
///
/// The expected value is a literal, not `EVENT_SCHEMA_VERSION`: a test that read
/// the constant back would pass even if the constant were raised on its own,
/// which is exactly the drift this has to catch. `contractmeta!` takes no path
/// expression, so the metadata literal in `lib.rs` is the third copy of this
/// number and has to move with it.
fn assert_events_are_versioned(env: &Env, registry: &Address) {
    let all = env.events().all();
    let mut published = 0u32;
    for event in all.filter_by_contract(registry).events() {
        assert_eq!(
            wire_schema_version(env, event),
            Some(1),
            "registry event without the published payload version: {:?}",
            event.body
        );
        published += 1;
    }
    assert!(published > 0, "expected the registry to publish an event");
}

#[test]
fn test_init_event_is_versioned_on_the_wire() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let registry_id = env.register(KeyLeaseRegistry, ());

    KeyLeaseRegistryClient::new(&env, &registry_id).init(&admin);

    assert_events_are_versioned(&env, &registry_id);
}

#[test]
fn test_service_and_lease_events_are_versioned() {
    let env = Env::default();
    let ctx = setup(&env);
    let registry_id = ctx.registry_id();

    // `set_token` is repeatable, so EscrowTokenSet is reachable without a
    // second registry deployment.
    let placeholder = Address::generate(&env);
    ctx.registry.set_token(&ctx.admin, &placeholder);
    assert_events_are_versioned(&env, &registry_id);
    ctx.registry.set_token(&ctx.admin, &ctx.token.address);

    let service_id = register_default_service(&env, &ctx);
    assert_events_are_versioned(&env, &registry_id);

    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &false);
    assert_events_are_versioned(&env, &registry_id);
    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &true);

    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    assert_events_are_versioned(&env, &registry_id);

    env.ledger().set_timestamp(3_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &4);
    assert_events_are_versioned(&env, &registry_id);
}

#[test]
fn test_revoke_event_is_versioned() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    env.ledger().set_timestamp(3_601 + GRACE);
    ctx.registry.revoke_expired_lease(&ctx.consumer, &lease_id);

    assert_events_are_versioned(&env, &ctx.registry_id());
}

#[test]
fn test_schema_version_is_payload_only_topics_stay_put() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    // Topics are positional: an indexer reads `lease_created` at index 0 and the
    // `lease_id` at index 1. Versioning had to go in the payload map, where a
    // new key is additive, so pin the topic layout against a future "just add a
    // topic" change.
    let all = env.events().all();
    let events = all.filter_by_contract(&ctx.registry_id());
    let xdr::ContractEventBody::V0(body) = &events.events()[0].body;
    assert_eq!(body.topics.len(), 2);
    let name: Val = Symbol::new(&env, "lease_created").into_val(&env);
    let id: Val = lease_id.into_val(&env);
    assert_eq!(
        body.topics.first(),
        Some(&xdr::ScVal::try_from_val(&env, &name).unwrap())
    );
    assert_eq!(
        body.topics.get(1),
        Some(&xdr::ScVal::try_from_val(&env, &id).unwrap())
    );
}

// ---------------------------------------------------------------------------
// TTL maintenance: pure reads and explicit bumps
// ---------------------------------------------------------------------------

/// Advance the ledger to the first moment an entry created `horizon` ledgers ago
/// is due for an extension, i.e. one ledger inside the bump threshold.
///
/// Every "reads must not bump" assertion has to be taken here: at any earlier
/// point there is nothing to bump, and the assertion would hold vacuously.
fn advance_until_bump_is_due(env: &Env, horizon: u32) {
    let seq = env.ledger().sequence();
    env.ledger()
        .set_sequence_number(seq + horizon - LEDGER_BUMP_THRESHOLD + 1);
}

/// Ledger effects of the invocation that just ran, as
/// `(persistent rent bumps, ledger writes, rent fee in stroops)`.
fn last_invocation(env: &Env) -> (u32, u32, i64) {
    let resources = env.cost_estimate().resources();
    let fee = env.cost_estimate().fee();
    (
        resources.persistent_entry_rent_bumps,
        resources.write_entries,
        fee.persistent_entry_rent,
    )
}

#[test]
fn test_lease_reads_never_extend_ttl() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // One lease per view, each read at the exact moment its own entry is due for
    // an extension. `lease_ttl_extend` is the policy that sized the entry, so
    // recomputing it here is what tells us when the bump would have fired.
    let views: [&str; 4] = [
        "get_lease",
        "lease_status",
        "remaining_calls",
        "locked_balance",
    ];
    for view in views {
        let lease_id = ctx
            .registry
            .create_lease(&ctx.consumer, &service_id, &10, &3_600);
        let horizon = storage::lease_ttl_extend(&env, &ctx.registry.get_lease(&lease_id));
        advance_until_bump_is_due(&env, horizon);

        match view {
            "get_lease" => {
                ctx.registry.get_lease(&lease_id);
            }
            "lease_status" => {
                ctx.registry.lease_status(&lease_id);
            }
            "remaining_calls" => {
                ctx.registry.remaining_calls(&lease_id);
            }
            _ => {
                ctx.registry.locked_balance(&lease_id);
            }
        }

        let (bumps, writes, rent) = last_invocation(&env);
        assert_eq!(bumps, 0, "{view} extended a storage TTL");
        assert_eq!(writes, 0, "{view} wrote the ledger");
        assert_eq!(rent, 0, "{view} charged the caller rent");
    }
}

#[test]
fn test_revoked_lease_status_read_never_extends_the_marker() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_100 + GRACE);
    ctx.registry.revoke_expired_lease(&ctx.consumer, &lease_id);

    // The revoke gave the marker the same horizon it gave the lease entry, so
    // the two age together and a read has nothing to add. Advance to the moment
    // both are due for a bump — the only state in which an extending read costs
    // the caller anything — and read the view that consults the marker.
    let horizon = storage::lease_ttl_extend(&env, &ctx.registry.get_lease(&lease_id));
    advance_until_bump_is_due(&env, horizon);

    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Revoked);

    let (bumps, writes, rent) = last_invocation(&env);
    assert_eq!(bumps, 0, "lease_status extended the revocation marker");
    assert_eq!(writes, 0, "lease_status wrote the ledger");
    assert_eq!(rent, 0, "lease_status charged the caller rent");
}

#[test]
fn test_service_stats_read_never_extends_its_entry() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_100 + GRACE);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &4);

    // `record_settlement` is the only thing that sizes the stats entry's
    // horizon, so a read at the moment it is due for a bump is the case that
    // would show a read paying rent.
    advance_until_bump_is_due(&env, LEDGER_BUMP_EXTEND);
    let stats = ctx.registry.service_stats(&service_id);
    assert_eq!(stats.settled_calls, 4);

    let (bumps, writes, rent) = last_invocation(&env);
    assert_eq!(bumps, 0, "service_stats extended its own entry");
    assert_eq!(writes, 0, "service_stats wrote the ledger");
    assert_eq!(rent, 0, "service_stats charged the caller rent");
}

#[test]
fn test_service_read_never_extends_and_bump_service_does() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    advance_until_bump_is_due(&env, LEDGER_BUMP_EXTEND);

    ctx.registry.get_service(&service_id);
    let (bumps, writes, rent) = last_invocation(&env);
    assert_eq!(bumps, 0, "get_service extended a storage TTL");
    assert_eq!(writes, 0, "get_service wrote the ledger");
    assert_eq!(rent, 0, "get_service charged the caller rent");

    ctx.registry.bump_service(&service_id);
    let (bumps, _, _) = last_invocation(&env);
    assert_eq!(bumps, 1, "bump_service must pay for the extension");
}

#[test]
fn test_bump_service_rejects_an_unknown_service() {
    let env = Env::default();
    let ctx = setup(&env);

    // `extend_ttl` on a key that does not exist is a silent no-op, so the
    // caller would pay for a bump that keeps nothing alive.
    assert_eq!(
        ctx.registry.try_bump_service(&404),
        Err(Ok(Error::ServiceNotFound))
    );
}

#[test]
fn test_bump_lease_is_where_the_rent_lives() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    let horizon = storage::lease_ttl_extend(&env, &ctx.registry.get_lease(&lease_id));
    advance_until_bump_is_due(&env, horizon);

    ctx.registry.get_lease(&lease_id);
    let read = env.cost_estimate().fee();

    ctx.registry.bump_lease(&lease_id);
    let resources = env.cost_estimate().resources();
    let bump = env.cost_estimate().fee();

    // Precondition: an extension really was due at this ledger, otherwise the
    // comparison below compares two no-ops.
    assert_eq!(resources.persistent_entry_rent_bumps, 1);
    assert_eq!(read.persistent_entry_rent, 0);
    assert!(bump.persistent_entry_rent > 0);
    // Measured on this policy: the pure read is 113 stroops of instructions and
    // the bump is 48,798, of which 48,684 is the persistent rent for the horizon
    // it bought. The ratio is a floor, not a target: the rent half grows with the
    // lease term, so a tight bound here would only register a regression.
    assert!(
        bump.total > read.total * 50,
        "expected the bumping call to dominate a pure read, read={} bump={}",
        read.total,
        bump.total
    );
}

// ---------------------------------------------------------------------------
// Explicit lease bumping
// ---------------------------------------------------------------------------

#[test]
fn test_bump_lease_needs_no_authorization() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.bump_lease(&lease_id);
    assert!(
        env.auths().is_empty(),
        "bump_lease demanded authorization: {:?}",
        env.auths()
    );

    // Control: settlement is authorized, so an empty list above is not the
    // host failing to record anything.
    env.ledger().set_timestamp(3_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);
    assert!(!env.auths().is_empty());
}

#[test]
fn test_bump_lease_rejects_an_unknown_lease() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry.try_bump_lease(&404),
        Err(Ok(Error::LeaseNotFound))
    );
}

#[test]
fn test_bump_lease_rejects_a_settled_lease() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    env.ledger().set_timestamp(3_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &10);

    // Its funds have moved; keeping the entry alive is pure state growth.
    assert_eq!(
        ctx.registry.try_bump_lease(&lease_id),
        Err(Ok(Error::LeaseAlreadySettled))
    );
}

#[test]
fn test_bump_lease_cannot_shorten_a_live_lease() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    // Twenty days rather than the maximum, so the entry's horizon stays inside
    // the instance entry's own thirty-day horizon: past that the host starts
    // reviving the instance too and the bump count stops meaning one thing.
    const TERM: u64 = 20 * 24 * 60 * 60;
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &TERM);

    let lease = ctx.registry.get_lease(&lease_id);
    let horizon = storage::lease_ttl_extend(&env, &lease);
    assert_eq!(horizon, 380_160);

    // Age the lease far past its grace window. The policy now asks for about two
    // days, which is much less than the horizon the entry already holds.
    env.ledger()
        .set_timestamp(lease.expires_at + 10 * 365 * 24 * 60 * 60);
    ctx.registry.bump_lease(&lease_id);
    let (bumps, writes, _) = last_invocation(&env);
    assert_eq!(bumps, 0);
    assert_eq!(writes, 0, "a bump that lowered the TTL would rewrite it");

    // And the long horizon survived the short ask: just inside it, an extension
    // is still due on an entry that is alive rather than resurrected. Had the
    // bump shortened the horizon, the entry would be past its lifetime here and
    // the host would pay to bring it back.
    advance_until_bump_is_due(&env, horizon);
    ctx.registry.bump_lease(&lease_id);
    let (bumps, writes, _) = last_invocation(&env);
    assert_eq!(bumps, 1, "the entry should still be live and due to extend");
    assert_eq!(writes, 0, "the entry should not have needed restoring");
}

// ---------------------------------------------------------------------------
// Lease TTL clamp
// ---------------------------------------------------------------------------

/// A lease whose only TTL-relevant field is `expires_at`.
fn lease_expiring_at(env: &Env, expires_at: u64) -> Lease {
    Lease {
        lease_id: 1,
        service_id: 1,
        consumer: Address::generate(env),
        allocated_calls: 1,
        used_calls: 0,
        expires_at,
        locked_deposit: RATE,
        settled: false,
    }
}

#[test]
fn test_ttl_ceiling_is_named_and_below_the_network_limit() {
    let env = Env::default();

    // ~90 days at five-second ledgers.
    assert_eq!(MAX_ENTRY_TTL_LEDGERS, 1_555_200);
    assert!(
        MAX_ENTRY_TTL_LEDGERS < env.storage().max_ttl(),
        "the contract ceiling must sit under the network's maxEntryTtl"
    );
    // That the ceiling also sits above the longest lease any caller can create
    // is pinned at runtime by `test_longest_lease_is_below_the_clamp`.
}

#[test]
fn test_longest_lease_is_below_the_clamp() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);

    let lease = lease_expiring_at(&env, 1_000 + MAX_LEASE_DURATION_SECS);
    let ask = storage::lease_ttl_extend(&env, &lease);

    // 30-day term + 24h grace + a one-day buffer, at five-second ledgers.
    assert_eq!(ask, 552_960);
    assert!(
        ask < MAX_ENTRY_TTL_LEDGERS,
        "the clamp must never truncate a lease the contract itself issued"
    );
}

#[test]
fn test_lease_ttl_ask_floors_once_the_grace_window_is_past() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);

    // A timestamp far past expiry: the remaining term saturates at zero, so the
    // ask is the grace window plus the buffer, and never goes negative.
    let lease = lease_expiring_at(&env, 1_000);
    assert_eq!(storage::lease_ttl_extend(&env, &lease), 34_560);

    let long_past = lease_expiring_at(&env, 0);
    assert_eq!(
        storage::lease_ttl_extend(&env, &long_past),
        storage::lease_ttl_extend(&env, &lease)
    );
}

#[test]
fn test_lease_ttl_ask_is_clamped_for_an_unreachable_horizon() {
    let env = Env::default();

    let lease = lease_expiring_at(&env, u64::MAX);
    assert_eq!(
        storage::lease_ttl_extend(&env, &lease),
        MAX_ENTRY_TTL_LEDGERS
    );
}

#[test]
fn test_lease_ttl_ask_follows_a_lowered_network_max_entry_ttl() {
    let env = Env::default();
    env.ledger().set_max_entry_ttl(100_000);

    let lease = lease_expiring_at(&env, u64::MAX);
    let ask = storage::lease_ttl_extend(&env, &lease);

    assert!(
        ask < MAX_ENTRY_TTL_LEDGERS,
        "the contract ceiling must yield to a lower network cap"
    );
    assert_eq!(ask, env.storage().max_ttl());
}
