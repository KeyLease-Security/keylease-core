use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{
        storage::{Instance as _, Persistent as _},
        Address as _, Events as _, Ledger as _,
    },
    token, xdr, Address, BytesN, Env, Event, IntoVal, Map, Symbol, TryFromVal, Val, Vec,
};

use keylease_mock_token::{MockToken, MockTokenClient};

use crate::events::{
    ContractUpgraded, EscrowTokenSet, LeaseCancellationRejected, LeaseCancellationRequested,
    LeaseCancellationWithdrawn, LeaseCancelled, LeaseCreated, LeaseRevoked, LeaseSettled,
    RegistryInitialized, ServiceRateUpdated, ServiceRegistered, ServiceStatusChanged,
    UpgradeCancelled, UpgradeProposed, EVENT_SCHEMA_VERSION,
};
use crate::storage::{
    self, DataKey, LEDGER_BUMP_EXTEND, LEDGER_BUMP_THRESHOLD, MAX_ENTRY_TTL_LEDGERS,
};
use crate::test_wasm::UPGRADE_TEST_WASM;
use crate::types::Service;
use crate::{
    Error, KeyLeaseRegistry, KeyLeaseRegistryClient, Lease, LeaseStatus, MAX_LEASE_DEPOSIT,
    MAX_LEASE_DURATION_SECS, MAX_PAGE_SIZE, UPGRADE_DELAY_SECS,
};

/// Per-call rate used across the tests.
const RATE: i128 = 100;
/// Escrow funded to the consumer at setup.
const FUNDING: i128 = 1_000_000;
/// The 24h provider settlement grace period, mirrored from the contract.
const GRACE: u64 = 24 * 60 * 60;
/// Ledgers to jump so an entry extended to the bump target falls below the bump
/// threshold while keeping enough life left to stay readable.
const LEDGERS_PAST_BUMP_THRESHOLD: u32 = LEDGER_BUMP_EXTEND - 5_000;

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

/// Ready-to-use registry: initialized, token configured, consumer funded and
/// one active service registered.
///
/// Ledger time is deliberately *not* touched here. Every test sets its own
/// timestamp so timing assumptions stay visible at the call site.
fn setup_with_service(env: &Env) -> (Ctx<'_>, u64) {
    let ctx = setup(env);
    let service_id = ctx
        .registry
        .register_service(&ctx.provider, &RATE, &endpoint_hash(env));
    (ctx, service_id)
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

#[test]
fn test_update_service_is_provider_only() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let impostor = Address::generate(&env);

    assert_eq!(
        ctx.registry
            .try_update_service(&impostor, &service_id, &100, &endpoint_hash(&env)),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_update_service_rejects_non_positive_rate() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    assert_eq!(
        ctx.registry
            .try_update_service(&ctx.provider, &service_id, &0, &endpoint_hash(&env)),
        Err(Ok(Error::InvalidRate))
    );
    assert_eq!(
        ctx.registry
            .try_update_service(&ctx.provider, &service_id, &-1, &endpoint_hash(&env)),
        Err(Ok(Error::InvalidRate))
    );
    assert_eq!(ctx.registry.get_service(&service_id).rate_per_call, RATE);
}

#[test]
fn test_update_service_preserves_existing_lease_rate() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    // Update the rate to a new value.
    ctx.registry
        .update_service(&ctx.provider, &service_id, &200, &endpoint_hash(&env));

    // The existing lease still uses the old rate.
    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(
        lease.locked_deposit,
        RATE * 10,
        "existing lease keeps old rate"
    );

    // A new lease uses the new rate.
    let new_lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let new_lease = ctx.registry.get_lease(&new_lease_id);
    assert_eq!(
        new_lease.locked_deposit,
        200 * 10,
        "new lease uses new rate"
    );
}

#[test]
fn test_update_service_updates_rate_and_endpoint() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // Update both rate and endpoint hash.
    ctx.registry
        .update_service(&ctx.provider, &service_id, &150, &endpoint_hash(&env));

    let service = ctx.registry.get_service(&service_id);
    assert_eq!(service.rate_per_call, 150, "rate was updated");
    assert_eq!(
        service.endpoint_hash,
        endpoint_hash(&env),
        "endpoint_hash was updated"
    );
}

#[test]
fn test_update_service_emits_event() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    ctx.registry
        .update_service(&ctx.provider, &service_id, &200, &endpoint_hash(&env));

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &ServiceRateUpdated {
            schema_version: EVENT_SCHEMA_VERSION,
            service_id,
            provider: ctx.provider.clone(),
            rate_per_call: 200,
            endpoint_hash: endpoint_hash(&env),
        },
    );
}

// ---------------------------------------------------------------------------
// Lease creation
// ---------------------------------------------------------------------------

#[test]
fn test_create_lease_locks_deposit_and_records_terms() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, service_id) = setup_with_service(&env);

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
// Quote view
// ---------------------------------------------------------------------------

#[test]
fn test_quote_lease_predicts_the_lease_it_mints() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let quote = ctx.registry.quote_lease(&service_id, &500, &3_600);
    assert_eq!(quote.locked_deposit, RATE * 500);
    assert_eq!(quote.expires_at, 1_000 + 3_600);

    // The quote must not drift from what the lease actually records.
    let lease = ctx.registry.get_lease(&ctx.registry.create_lease(
        &ctx.consumer,
        &service_id,
        &500,
        &3_600,
    ));
    assert_eq!(quote.locked_deposit, lease.locked_deposit);
    assert_eq!(quote.expires_at, lease.expires_at);
}

#[test]
fn test_quote_lease_changes_nothing() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let consumer_before = ctx.token.balance(&ctx.consumer);

    ctx.registry.quote_lease(&service_id, &500, &3_600);

    assert_eq!(ctx.token.balance(&ctx.consumer), consumer_before);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
    assert_eq!(ctx.registry.lease_count(), 0);
    // `events().all()` reports the invocation just completed, which is the
    // quote itself: a view must emit no Soroban event.
    assert!(env.events().all().events().is_empty());
}

#[test]
fn test_quote_lease_rejects_a_paused_service() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &false);

    assert_eq!(
        ctx.registry.try_quote_lease(&service_id, &10, &3_600),
        Err(Ok(Error::ServiceInactive))
    );
}

#[test]
fn test_quote_lease_reports_the_same_errors_as_create_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &false);

    // (service, calls, duration, expected) — every rejection that
    // `create_lease` reaches before it touches a token balance.
    let cases = [
        (service_id, 0, 3_600, Error::InvalidCalls),
        (service_id, 10, 0, Error::InvalidDuration),
        (
            service_id,
            10,
            MAX_LEASE_DURATION_SECS + 1,
            Error::DurationTooLong,
        ),
        (service_id, 10, 3_600, Error::ServiceInactive),
        (999, 10, 3_600, Error::ServiceNotFound),
    ];

    for (id, calls, duration, expected) in cases {
        assert_eq!(
            ctx.registry.try_quote_lease(&id, &calls, &duration),
            Err(Ok(expected)),
            "quote_lease(service {id}, {calls} calls, {duration}s)"
        );
        assert_eq!(
            ctx.registry
                .try_create_lease(&ctx.consumer, &id, &calls, &duration),
            Err(Ok(expected)),
            "create_lease(service {id}, {calls} calls, {duration}s)"
        );
    }
}

#[test]
fn test_quote_lease_rejects_an_overflowing_deposit() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    // A rate that overflows i128 for any call count above 1.
    let service_id = ctx
        .registry
        .register_service(&ctx.provider, &i128::MAX, &endpoint_hash(&env));

    assert_eq!(
        ctx.registry.try_quote_lease(&service_id, &2, &3_600),
        Err(Ok(Error::Overflow))
    );
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &2, &3_600),
        Err(Ok(Error::Overflow))
    );
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
}

#[test]
fn test_quote_lease_rejects_a_deposit_over_the_cap() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    // Two calls at this rate fit `i128` and still clear `MAX_LEASE_DEPOSIT`, so
    // the rejection must be `DepositTooLarge` and never `Overflow`.
    let rate = MAX_LEASE_DEPOSIT / 2 + 1;
    let service_id = ctx
        .registry
        .register_service(&ctx.provider, &rate, &endpoint_hash(&env));

    assert_eq!(
        ctx.registry.try_quote_lease(&service_id, &2, &3_600),
        Err(Ok(Error::DepositTooLarge))
    );
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &2, &3_600),
        Err(Ok(Error::DepositTooLarge))
    );

    // The cap is the only money check a view can make: `cap - 1` is quotable
    // even though this consumer could not fund it, because the registry never
    // reads the caller's balance.
    let boundary = ctx.registry.register_service(
        &ctx.provider,
        &(MAX_LEASE_DEPOSIT - 1),
        &endpoint_hash(&env),
    );
    let quote = ctx.registry.quote_lease(&boundary, &1, &3_600);
    assert_eq!(quote.locked_deposit, MAX_LEASE_DEPOSIT - 1);
}

#[test]
fn test_quote_lease_rejects_an_overflowing_expiry() {
    let env = Env::default();
    env.ledger().set_timestamp(u64::MAX);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    assert_eq!(
        ctx.registry.try_quote_lease(&service_id, &10, &1),
        Err(Ok(Error::Overflow))
    );
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &10, &1),
        Err(Ok(Error::Overflow))
    );
}

#[test]
fn test_quote_lease_requires_configured_token() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let provider = Address::generate(&env);

    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(&env, &registry_id);
    registry.init(&admin);
    // Note: no set_token call, same as test_create_lease_requires_configured_token.

    let service_id = registry.register_service(&provider, &RATE, &endpoint_hash(&env));

    assert_eq!(
        registry.try_quote_lease(&service_id, &10, &3_600),
        Err(Ok(Error::TokenNotSet))
    );
}

// ---------------------------------------------------------------------------
// Provider service index
// ---------------------------------------------------------------------------

/// Read the TTL of one of the registry's persistent entries.
fn persistent_ttl(env: &Env, registry: &Address, key: &DataKey) -> u32 {
    env.as_contract(registry, || env.storage().persistent().get_ttl(key))
}

/// Rewrite a service's owner through `save_service`, the path a future
/// `transfer_service` entry point would have to use.
fn reassign_service(env: &Env, registry: &Address, service_id: u64, to: &Address) {
    env.as_contract(registry, || {
        let mut service: Service = env
            .storage()
            .persistent()
            .get(&DataKey::Service(service_id))
            .unwrap();
        service.provider = to.clone();
        crate::storage::save_service(env, &service);
    });
}

#[test]
fn test_services_for_provider_lists_in_registration_order() {
    let env = Env::default();
    let ctx = setup(&env);

    let first = register_default_service(&env, &ctx);
    let second = register_default_service(&env, &ctx);
    let third = register_default_service(&env, &ctx);

    let page = ctx.registry.services_for_provider(&ctx.provider, &0, &10);
    assert_eq!(
        page.service_ids,
        Vec::from_slice(&env, &[first, second, third])
    );
    assert_eq!(page.next, 3);
    assert!(page.done);
}

#[test]
fn test_services_for_provider_pages_without_gaps_or_repeats() {
    let env = Env::default();
    let ctx = setup(&env);

    let mut all = [0u64; 5];
    for id in all.iter_mut() {
        *id = register_default_service(&env, &ctx);
    }

    let mut seen = Vec::new(&env);
    let mut after = 0u32;
    for _ in 0..5 {
        let page = ctx
            .registry
            .services_for_provider(&ctx.provider, &after, &2);
        for service_id in page.service_ids.iter() {
            seen.push_back(service_id);
        }
        after = page.next;
        if page.done {
            break;
        }
    }

    assert_eq!(seen, Vec::from_slice(&env, &all));
}

#[test]
fn test_services_for_provider_reports_page_bounds() {
    let env = Env::default();
    let ctx = setup(&env);
    for _ in 0..5 {
        register_default_service(&env, &ctx);
    }

    let first = ctx.registry.services_for_provider(&ctx.provider, &0, &2);
    assert_eq!(first.service_ids.len(), 2);
    assert_eq!(first.next, 2);
    assert!(!first.done);

    let middle = ctx
        .registry
        .services_for_provider(&ctx.provider, &first.next, &2);
    assert_eq!(middle.service_ids.len(), 2);
    assert_eq!(middle.next, 4);
    assert!(!middle.done);

    let last = ctx
        .registry
        .services_for_provider(&ctx.provider, &middle.next, &2);
    assert_eq!(last.service_ids.len(), 1);
    assert_eq!(last.next, 5);
    assert!(last.done);
}

#[test]
fn test_services_for_provider_cursors_past_the_end_are_done() {
    let env = Env::default();
    let ctx = setup(&env);
    register_default_service(&env, &ctx);

    // A cursor past the index yields nothing and is terminal, so a client that
    // re-reads with a stale cursor cannot loop forever.
    let page = ctx
        .registry
        .services_for_provider(&ctx.provider, &1_000, &10);
    assert!(page.service_ids.is_empty());
    assert_eq!(page.next, 1);
    assert!(page.done);
}

#[test]
fn test_services_for_provider_of_an_unknown_provider_is_an_empty_page() {
    let env = Env::default();
    let ctx = setup(&env);
    register_default_service(&env, &ctx);

    let stranger = Address::generate(&env);
    let page = ctx.registry.services_for_provider(&stranger, &0, &10);

    assert!(page.service_ids.is_empty());
    assert_eq!(page.next, 0);
    assert!(page.done);
}

#[test]
fn test_services_for_provider_never_leaks_another_providers_services() {
    let env = Env::default();
    let ctx = setup(&env);
    let other = Address::generate(&env);

    let mine_a = register_default_service(&env, &ctx);
    let their_id = ctx
        .registry
        .register_service(&other, &RATE, &endpoint_hash(&env));
    let mine_b = register_default_service(&env, &ctx);

    let mine = ctx.registry.services_for_provider(&ctx.provider, &0, &10);
    assert_eq!(mine.service_ids, Vec::from_slice(&env, &[mine_a, mine_b]));

    let theirs = ctx.registry.services_for_provider(&other, &0, &10);
    assert_eq!(theirs.service_ids, Vec::from_slice(&env, &[their_id]));
}

#[test]
fn test_services_for_provider_bounds_the_page_size() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // Zero would be a silent no-op and unbounded would read the whole index in
    // one transaction; both have to be explicit errors.
    assert_eq!(
        ctx.registry
            .try_services_for_provider(&ctx.provider, &0, &0),
        Err(Ok(Error::InvalidLimit))
    );
    assert_eq!(
        ctx.registry
            .try_services_for_provider(&ctx.provider, &0, &(MAX_PAGE_SIZE + 1)),
        Err(Ok(Error::InvalidLimit))
    );

    let max = ctx
        .registry
        .services_for_provider(&ctx.provider, &0, &MAX_PAGE_SIZE);
    assert_eq!(max.service_ids, Vec::from_slice(&env, &[service_id]));
}

#[test]
fn test_services_for_provider_drops_a_transferred_service_from_the_old_owner() {
    let env = Env::default();
    let ctx = setup(&env);
    let new_owner = Address::generate(&env);

    let kept = register_default_service(&env, &ctx);
    let transferred = register_default_service(&env, &ctx);
    let last = register_default_service(&env, &ctx);

    reassign_service(&env, &ctx.registry_id(), transferred, &new_owner);

    // The stale id is still in the old owner's index; the read path is what
    // hides it, and `next` still counts the entry it consumed.
    let old = ctx.registry.services_for_provider(&ctx.provider, &0, &10);
    assert_eq!(old.service_ids, Vec::from_slice(&env, &[kept, last]));
    assert_eq!(old.next, 3);
    assert!(old.done);

    let new = ctx.registry.services_for_provider(&new_owner, &0, &10);
    assert_eq!(new.service_ids, Vec::from_slice(&env, &[transferred]));
    assert_eq!(new.next, 1);
    assert!(new.done);
}

#[test]
fn test_transferred_service_can_be_leased_by_its_new_owner() {
    let env = Env::default();
    let ctx = setup(&env);
    let new_owner = Address::generate(&env);

    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    reassign_service(&env, &ctx.registry_id(), service_id, &new_owner);

    // Ownership lives on the record, so settlement follows the transfer even
    // though the old owner's index entry is now dead weight.
    assert_eq!(
        ctx.registry.try_settle_lease(&ctx.provider, &lease_id, &10),
        Err(Ok(Error::Unauthorized))
    );
    ctx.registry.settle_lease(&new_owner, &lease_id, &10);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
}

#[test]
fn test_reregistering_a_service_does_not_duplicate_its_index_entry() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // Any entry point that re-saves an owned service must stay idempotent.
    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &false);
    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &true);

    let page = ctx.registry.services_for_provider(&ctx.provider, &0, &10);
    assert_eq!(page.service_ids, Vec::from_slice(&env, &[service_id]));
    assert_eq!(page.next, 1);
}

#[test]
fn test_provider_index_entry_shares_the_service_rent_policy() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let service_key = DataKey::Service(service_id);
    let index_key = DataKey::ProviderServices(ctx.provider.clone());
    let at_write = persistent_ttl(&env, &ctx.registry_id(), &index_key);

    // Same constants as the record it points at, so neither can be evicted
    // while the other is still readable.
    assert_eq!(
        at_write,
        persistent_ttl(&env, &ctx.registry_id(), &service_key)
    );
    assert_eq!(at_write, LEDGER_BUMP_EXTEND);

    // Walk past the bump threshold without touching the registry: nothing on the
    // read-only path is allowed to revive the entry, so it has to arrive decayed.
    let now = env.ledger().sequence();
    env.ledger()
        .set_sequence_number(now + LEDGERS_PAST_BUMP_THRESHOLD);
    let decayed = persistent_ttl(&env, &ctx.registry_id(), &index_key);
    assert!(decayed < LEDGER_BUMP_THRESHOLD);

    // Listing is the read-only surface the module doc bills at zero rent, and that
    // includes the reader's own index entry: a page must not pay for anything.
    ctx.registry
        .services_for_provider(&ctx.provider, &0, &MAX_PAGE_SIZE);
    assert_eq!(
        persistent_ttl(&env, &ctx.registry_id(), &index_key),
        decayed
    );
}

#[test]
fn test_a_catalogue_write_revives_the_provider_index() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let index_key = DataKey::ProviderServices(ctx.provider.clone());

    let now = env.ledger().sequence();
    env.ledger()
        .set_sequence_number(now + LEDGERS_PAST_BUMP_THRESHOLD);
    assert!(persistent_ttl(&env, &ctx.registry_id(), &index_key) < LEDGER_BUMP_THRESHOLD);

    // Pausing appends no id — the service is already indexed — so this is the call
    // that fails if the extension lives inside the append guard. Without it, a
    // provider could only refresh its index by registering something new.
    ctx.registry
        .set_service_active(&ctx.provider, &service_id, &false);
    assert_eq!(
        persistent_ttl(&env, &ctx.registry_id(), &index_key),
        LEDGER_BUMP_EXTEND
    );
}

#[test]
fn test_services_for_provider_does_not_pay_the_listed_services_rent() {
    let env = Env::default();
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let service_key = DataKey::Service(service_id);
    let now = env.ledger().sequence();
    env.ledger()
        .set_sequence_number(now + LEDGERS_PAST_BUMP_THRESHOLD);
    let before = persistent_ttl(&env, &ctx.registry_id(), &service_key);
    assert!(before < LEDGER_BUMP_THRESHOLD);

    ctx.registry
        .services_for_provider(&ctx.provider, &0, &MAX_PAGE_SIZE);

    // Paging is a discovery read over somebody else's entries: the owner pays to
    // keep them alive, not whoever happens to list them.
    assert_eq!(
        persistent_ttl(&env, &ctx.registry_id(), &service_key),
        before
    );
}

#[test]
fn test_services_for_provider_is_a_view() {
    let env = Env::default();
    let ctx = setup(&env);
    register_default_service(&env, &ctx);

    let counter_before = ctx.registry.service_count();
    ctx.registry
        .services_for_provider(&ctx.provider, &0, &MAX_PAGE_SIZE);

    assert_eq!(ctx.registry.service_count(), counter_before);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
    assert!(env.events().all().events().is_empty());
}

// ---------------------------------------------------------------------------
// Settlement
// ---------------------------------------------------------------------------

#[test]
fn test_full_usage_can_settle_before_expiry() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, service_id) = setup_with_service(&env);
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
    let (ctx, service_id) = setup_with_service(&env);
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
    let (ctx, service_id) = setup_with_service(&env);
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
// Derived views across every lease state
//
// `remaining_calls` and `locked_balance` encode protocol semantics (what a
// consumer may still use, what is still owed), so they are pinned here for each
// state instead of only being read incidentally by the lifecycle tests.
// ---------------------------------------------------------------------------

#[test]
fn test_remaining_calls_equals_allocated_calls_immediately_after_create_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // Two leases on the same service: the view is per lease, not per service.
    let first = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &7, &3_600);
    let second = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &25, &3_600);

    assert_eq!(ctx.registry.remaining_calls(&first), 7);
    assert_eq!(ctx.registry.remaining_calls(&second), 25);
    assert_eq!(ctx.registry.lease_status(&first), LeaseStatus::Active);
    assert_eq!(ctx.registry.lease_status(&second), LeaseStatus::Active);

    // Nothing has been consumed or settled, so the full deposit is still locked.
    let lease = ctx.registry.get_lease(&first);
    assert_eq!(lease.used_calls, 0);
    assert_eq!(ctx.registry.locked_balance(&first), lease.locked_deposit);
}

#[test]
fn test_remaining_calls_is_zero_at_and_after_expiry() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    // One second before expiry the full allotment is still usable.
    env.ledger().set_timestamp(1_099);
    assert_eq!(ctx.registry.remaining_calls(&lease_id), 10);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Active);

    // Expiry is inclusive: at `expires_at` nothing remains.
    env.ledger().set_timestamp(1_100);
    assert_eq!(ctx.registry.remaining_calls(&lease_id), 0);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Expired);

    // And it stays zero afterwards.
    env.ledger().set_timestamp(1_100 + GRACE);
    assert_eq!(ctx.registry.remaining_calls(&lease_id), 0);

    // Expiry does not release escrow: an unsettled lease still owes its deposit
    // until the provider settles or the consumer revokes.
    assert_eq!(
        ctx.registry.locked_balance(&lease_id),
        RATE * 10,
        "expired but unsettled leases still hold the deposit"
    );
}

#[test]
fn test_remaining_calls_and_locked_balance_are_zero_after_settle_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    // Expired, partially consumed settlement: 6 paid, 4 refunded.
    env.ledger().set_timestamp(1_100);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &6);

    assert_eq!(ctx.registry.remaining_calls(&lease_id), 0);
    assert_eq!(ctx.registry.locked_balance(&lease_id), 0);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
}

#[test]
fn test_remaining_calls_and_locked_balance_are_zero_after_revoke_expired_lease() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);

    env.ledger().set_timestamp(1_100 + GRACE);
    ctx.registry.revoke_expired_lease(&ctx.consumer, &lease_id);

    assert_eq!(ctx.registry.remaining_calls(&lease_id), 0);
    assert_eq!(ctx.registry.locked_balance(&lease_id), 0);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
}

#[test]
fn test_derived_lease_views_report_unknown_lease() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(
        ctx.registry.try_remaining_calls(&42),
        Err(Ok(Error::LeaseNotFound))
    );
    assert_eq!(
        ctx.registry.try_locked_balance(&42),
        Err(Ok(Error::LeaseNotFound))
    );
}

#[test]
fn test_service_count_after_each_kind_of_mutation() {
    let env = Env::default();
    let ctx = setup(&env);

    assert_eq!(ctx.registry.service_count(), 0);

    let first = register_default_service(&env, &ctx);
    assert_eq!(ctx.registry.service_count(), 1);

    let second = register_default_service(&env, &ctx);
    assert_eq!(ctx.registry.service_count(), 2);

    // Mutating an existing service is not a registration.
    ctx.registry
        .set_service_active(&ctx.provider, &first, &false);
    assert_eq!(ctx.registry.service_count(), 2);

    ctx.registry
        .update_service(&ctx.provider, &first, &150, &endpoint_hash(&env));
    assert_eq!(ctx.registry.service_count(), 2);

    ctx.registry
        .set_service_active(&ctx.provider, &first, &true);
    assert_eq!(ctx.registry.service_count(), 2);

    // A rejected registration allocates no id.
    assert_eq!(
        ctx.registry
            .try_register_service(&ctx.provider, &0, &endpoint_hash(&env)),
        Err(Ok(Error::InvalidRate))
    );
    assert_eq!(ctx.registry.service_count(), 2);
    assert_eq!(ctx.registry.get_service(&second).service_id, second);
}

#[test]
fn test_lease_count_after_each_kind_of_mutation() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    assert_eq!(ctx.registry.lease_count(), 0);

    let first = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    assert_eq!(ctx.registry.lease_count(), 1);

    let second = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &5, &100);
    assert_eq!(ctx.registry.lease_count(), 2);

    // Settlement is a lease mutation, not a creation.
    ctx.registry.settle_lease(&ctx.provider, &first, &10);
    assert_eq!(ctx.registry.lease_count(), 2);

    // Neither is a revoke.
    env.ledger().set_timestamp(1_100 + GRACE);
    ctx.registry.revoke_expired_lease(&ctx.consumer, &second);
    assert_eq!(ctx.registry.lease_count(), 2);

    // A rejected create allocates no id either.
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &0, &3_600),
        Err(Ok(Error::InvalidCalls))
    );
    assert_eq!(ctx.registry.lease_count(), 2);
}

// ---------------------------------------------------------------------------
// Soroban contract events
// ---------------------------------------------------------------------------

#[test]
fn test_init_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(&env, &registry_id);

    registry.init(&admin);

    assert_single_event(
        &env,
        &registry_id,
        &RegistryInitialized {
            schema_version: EVENT_SCHEMA_VERSION,
            admin,
        },
    );
}

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
    let (ctx, service_id) = setup_with_service(&env);

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
            rate_per_call: RATE,
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
        soroban_sdk::String::from_str(&env, "0.2.1")
    );
}

#[test]
fn test_propose_upgrade_records_hash_and_execution_ledger() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, _service_id) = setup_with_service(&env);
    let new_wasm_hash = BytesN::from_array(&env, &[9u8; 32]);

    ctx.registry.propose_upgrade(&ctx.admin, &new_wasm_hash);

    // Read events before any further invocation: the host resets them per call.
    assert_single_event(
        &env,
        &ctx.registry_id(),
        &UpgradeProposed {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: ctx.admin.clone(),
            new_wasm_hash: new_wasm_hash.clone(),
            execute_after: 1_000 + UPGRADE_DELAY_SECS,
        },
    );
    let pending = ctx.registry.get_pending_upgrade().expect("pending upgrade");
    assert_eq!(pending.new_wasm_hash, new_wasm_hash);
    assert_eq!(pending.execute_after, 1_000 + UPGRADE_DELAY_SECS);
}

#[test]
fn test_execute_upgrade_before_delay_fails() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, _service_id) = setup_with_service(&env);
    let new_wasm_hash = BytesN::from_array(&env, &[9u8; 32]);
    ctx.registry.propose_upgrade(&ctx.admin, &new_wasm_hash);

    // One second short of the recorded execution ledger.
    env.ledger().set_timestamp(1_000 + UPGRADE_DELAY_SECS - 1);
    assert_eq!(
        ctx.registry.try_execute_upgrade(&ctx.admin),
        Err(Ok(Error::UpgradeDelayNotElapsed))
    );
    // A failed execution must not consume the proposal.
    let pending = ctx.registry.get_pending_upgrade().expect("pending upgrade");
    assert_eq!(pending.new_wasm_hash, new_wasm_hash);
    assert_eq!(pending.execute_after, 1_000 + UPGRADE_DELAY_SECS);
}

#[test]
fn test_execute_upgrade_after_delay_reports_hash_and_ledger() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, _service_id) = setup_with_service(&env);
    let wasm_hash = env.deployer().upload_contract_wasm(UPGRADE_TEST_WASM);
    ctx.registry.propose_upgrade(&ctx.admin, &wasm_hash);

    // At the recorded ledger, execution becomes legal.
    env.ledger().set_timestamp(1_000 + UPGRADE_DELAY_SECS);
    ctx.registry.execute_upgrade(&ctx.admin);

    // The install swaps the contract code, so all registry reads happen before
    // execution. Host-level events remain readable afterwards.
    assert_single_event(
        &env,
        &ctx.registry_id(),
        &ContractUpgraded {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: ctx.admin.clone(),
            new_wasm_hash: wasm_hash,
            execute_after: 1_000 + UPGRADE_DELAY_SECS,
        },
    );
}

#[test]
fn test_cancel_upgrade_prevents_execution() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, _service_id) = setup_with_service(&env);
    let new_wasm_hash = BytesN::from_array(&env, &[9u8; 32]);
    ctx.registry.propose_upgrade(&ctx.admin, &new_wasm_hash);

    ctx.registry.cancel_upgrade(&ctx.admin);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &UpgradeCancelled {
            schema_version: EVENT_SCHEMA_VERSION,
            admin: ctx.admin.clone(),
            new_wasm_hash: new_wasm_hash.clone(),
        },
    );
    assert!(ctx.registry.get_pending_upgrade().is_none());
    env.ledger().set_timestamp(1_000 + UPGRADE_DELAY_SECS);
    assert_eq!(
        ctx.registry.try_execute_upgrade(&ctx.admin),
        Err(Ok(Error::NoPendingUpgrade))
    );
}

#[test]
fn test_upgrade_entry_points_are_admin_only() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, _service_id) = setup_with_service(&env);
    let impostor = Address::generate(&env);
    let new_wasm_hash = BytesN::from_array(&env, &[9u8; 32]);

    assert_eq!(
        ctx.registry.try_propose_upgrade(&impostor, &new_wasm_hash),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        ctx.registry.try_cancel_upgrade(&impostor),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        ctx.registry.try_execute_upgrade(&impostor),
        Err(Ok(Error::Unauthorized))
    );
    assert!(ctx.registry.get_pending_upgrade().is_none());
}

#[test]
fn test_reproposal_restarts_the_delay() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let (ctx, _service_id) = setup_with_service(&env);
    let first_hash = BytesN::from_array(&env, &[1u8; 32]);
    let second_hash = BytesN::from_array(&env, &[2u8; 32]);
    ctx.registry.propose_upgrade(&ctx.admin, &first_hash);

    env.ledger().set_timestamp(1_000 + UPGRADE_DELAY_SECS - 1);
    ctx.registry.propose_upgrade(&ctx.admin, &second_hash);

    let pending = ctx.registry.get_pending_upgrade().expect("pending upgrade");
    assert_eq!(pending.new_wasm_hash, second_hash);
    assert_eq!(
        pending.execute_after,
        1_000 + UPGRADE_DELAY_SECS - 1 + UPGRADE_DELAY_SECS
    );
    assert_eq!(
        ctx.registry.try_execute_upgrade(&ctx.admin),
        Err(Ok(Error::UpgradeDelayNotElapsed))
    );
}

// A successful upgrade installs a real Wasm uploaded with
// `stellar contract upload`, which the unit-test host cannot fabricate (it
// requires a valid `contractspecv0` metadata section). The success path is
// exercised end to end by `scripts/upgrade.sh` against
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
    let service_id = register_service_at(&env, &ctx, MAX_LEASE_DEPOSIT + 1);
    // Fund the lease the bound is meant to stop: the consumer could otherwise
    // afford it, and a passing assertion would only be proving poverty.
    ctx.token.mint(&ctx.consumer, &(MAX_LEASE_DEPOSIT + 1));
    let funded = ctx.token.balance(&ctx.consumer);
    let leases_before = ctx.registry.lease_count();

    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &1, &3_600),
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
fn test_deposit_bound_rejects_before_the_overflow_path() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_service_at(&env, &ctx, i128::MAX / 2);
    // `2 × (i128::MAX / 2)` fits in `i128`, so this is not an arithmetic
    // failure — it is the bound doing its job, and the two errors stay
    // distinguishable for clients.
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &2, &3_600),
        Err(Ok(Error::DepositTooLarge))
    );
    assert_eq!(
        ctx.registry
            .try_create_lease(&ctx.consumer, &service_id, &3, &3_600),
        Err(Ok(Error::Overflow))
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

// ---------------------------------------------------------------------------
// Early consumer cancellation
// ---------------------------------------------------------------------------

#[test]
fn test_early_cancel_with_provider_consent() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);

    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseCancellationRequested {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            consumer: ctx.consumer.clone(),
        },
    );

    // The request alone moves nothing.
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);

    // The provider consent reports 4 served calls; the other 6 are refunded.
    env.ledger().set_timestamp(1_500);
    ctx.registry
        .approve_early_cancel(&ctx.provider, &lease_id, &4);

    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 4);
    assert_eq!(
        ctx.token.balance(&ctx.consumer),
        FUNDING - RATE * 10 + RATE * 6
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);

    let lease = ctx.registry.get_lease(&lease_id);
    assert_eq!(lease.used_calls, 4);
    assert!(lease.settled);
    assert_eq!(ctx.registry.lease_status(&lease_id), LeaseStatus::Settled);
    assert_eq!(ctx.registry.remaining_calls(&lease_id), 0);
    assert_eq!(ctx.registry.locked_balance(&lease_id), 0);

    // The lease is finished: it cannot be approved, settled or revoked again.
    assert_eq!(
        ctx.registry
            .try_approve_early_cancel(&ctx.provider, &lease_id, &4),
        Err(Ok(Error::LeaseAlreadySettled))
    );
    assert_eq!(
        ctx.registry.try_settle_lease(&ctx.provider, &lease_id, &4),
        Err(Ok(Error::LeaseAlreadySettled))
    );
}

#[test]
fn test_early_cancel_conservation() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    // Zero calls served: the entire deposit comes back.
    let unused_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    ctx.registry
        .request_early_cancel(&ctx.consumer, &unused_lease);
    ctx.registry
        .approve_early_cancel(&ctx.provider, &unused_lease, &0);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING);
    assert_eq!(ctx.token.balance(&ctx.provider), 0);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);

    // Every call served: no refund, and the provider receives the full deposit.
    let used_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    ctx.registry
        .request_early_cancel(&ctx.consumer, &used_lease);
    ctx.registry
        .approve_early_cancel(&ctx.provider, &used_lease, &10);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.consumer), FUNDING - RATE * 10);
    assert_eq!(ctx.token.balance(&ctx.registry_id()), 0);
}

#[test]
fn test_provider_can_refuse_early_cancel() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);
    ctx.registry.reject_early_cancel(&ctx.provider, &lease_id);

    // The request is gone, so consent can no longer release funds.
    assert_eq!(
        ctx.registry
            .try_approve_early_cancel(&ctx.provider, &lease_id, &2),
        Err(Ok(Error::CancellationNotRequested))
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);

    // The lease is untouched and still settles normally at expiry.
    env.ledger().set_timestamp(4_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &3);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 3);
    assert_eq!(
        ctx.token.balance(&ctx.consumer),
        FUNDING - RATE * 10 + RATE * 7
    );
}

#[test]
fn test_early_cancel_requires_consent_and_ownership() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let impostor = Address::generate(&env);

    // Only the consumer may request cancellation.
    assert_eq!(
        ctx.registry.try_request_early_cancel(&impostor, &lease_id),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        ctx.registry
            .try_request_early_cancel(&ctx.provider, &lease_id),
        Err(Ok(Error::Unauthorized))
    );

    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);

    // One pending request at a time.
    assert_eq!(
        ctx.registry
            .try_request_early_cancel(&ctx.consumer, &lease_id),
        Err(Ok(Error::CancellationAlreadyRequested))
    );

    // Only the owning provider may approve or reject.
    assert_eq!(
        ctx.registry
            .try_approve_early_cancel(&impostor, &lease_id, &1),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        ctx.registry
            .try_approve_early_cancel(&ctx.consumer, &lease_id, &1),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        ctx.registry.try_reject_early_cancel(&impostor, &lease_id),
        Err(Ok(Error::Unauthorized))
    );

    // Approval without a request is rejected even for the real provider.
    let other_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    assert_eq!(
        ctx.registry
            .try_approve_early_cancel(&ctx.provider, &other_lease, &1),
        Err(Ok(Error::CancellationNotRequested))
    );
    assert_eq!(
        ctx.registry
            .try_reject_early_cancel(&ctx.provider, &other_lease),
        Err(Ok(Error::CancellationNotRequested))
    );
}

#[test]
fn test_consumer_can_withdraw_early_cancel() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);
    let impostor = Address::generate(&env);

    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);

    assert_eq!(
        ctx.registry.try_withdraw_early_cancel(&impostor, &lease_id),
        Err(Ok(Error::Unauthorized))
    );

    ctx.registry.withdraw_early_cancel(&ctx.consumer, &lease_id);

    assert_eq!(
        ctx.registry
            .try_withdraw_early_cancel(&ctx.consumer, &lease_id),
        Err(Ok(Error::CancellationNotRequested))
    );
    assert_eq!(
        ctx.registry
            .try_approve_early_cancel(&ctx.provider, &lease_id, &2),
        Err(Ok(Error::CancellationNotRequested))
    );
    assert_eq!(ctx.token.balance(&ctx.registry_id()), RATE * 10);
}

#[test]
fn test_early_cancel_rejected_after_settlement() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    // The consumer asked early, then the provider settled partially instead.
    env.ledger().set_timestamp(2_000);
    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);
    env.ledger().set_timestamp(4_601);
    ctx.registry.settle_lease(&ctx.provider, &lease_id, &4);

    assert_eq!(
        ctx.registry
            .try_request_early_cancel(&ctx.consumer, &lease_id),
        Err(Ok(Error::LeaseAlreadySettled))
    );
    assert_eq!(
        ctx.registry
            .try_approve_early_cancel(&ctx.provider, &lease_id, &4),
        Err(Ok(Error::LeaseAlreadySettled))
    );

    // Settlement cleared the pending request; nothing stale is left behind.
    assert_eq!(
        ctx.registry
            .try_withdraw_early_cancel(&ctx.consumer, &lease_id),
        Err(Ok(Error::CancellationNotRequested))
    );
}

#[test]
fn test_early_cancel_after_expiry_is_rejected() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);

    let expired_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);
    env.ledger().set_timestamp(1_100);
    assert_eq!(
        ctx.registry
            .try_request_early_cancel(&ctx.consumer, &expired_lease),
        Err(Ok(Error::LeaseExpired))
    );

    // A request made before expiry can still be approved after the lease expires.
    let active_lease = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &100);
    env.ledger().set_timestamp(1_199);
    ctx.registry
        .request_early_cancel(&ctx.consumer, &active_lease);
    env.ledger().set_timestamp(1_200);
    ctx.registry
        .approve_early_cancel(&ctx.provider, &active_lease, &5);
    assert_eq!(ctx.token.balance(&ctx.provider), RATE * 5);
}

#[test]
fn test_early_cancel_events() {
    let env = Env::default();
    env.ledger().set_timestamp(1_000);
    let ctx = setup(&env);
    let service_id = register_default_service(&env, &ctx);
    let lease_id = ctx
        .registry
        .create_lease(&ctx.consumer, &service_id, &10, &3_600);

    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);
    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseCancellationRequested {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            consumer: ctx.consumer.clone(),
        },
    );

    ctx.registry.reject_early_cancel(&ctx.provider, &lease_id);
    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseCancellationRejected {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            provider: ctx.provider.clone(),
        },
    );

    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);
    ctx.registry.withdraw_early_cancel(&ctx.consumer, &lease_id);
    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseCancellationWithdrawn {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            consumer: ctx.consumer.clone(),
        },
    );

    ctx.registry.request_early_cancel(&ctx.consumer, &lease_id);
    ctx.registry
        .approve_early_cancel(&ctx.provider, &lease_id, &6);
    assert_single_event(
        &env,
        &ctx.registry_id(),
        &LeaseCancelled {
            schema_version: EVENT_SCHEMA_VERSION,
            lease_id,
            provider: ctx.provider.clone(),
            consumer: ctx.consumer.clone(),
            verified_calls: 6,
            payout: RATE * 6,
            refund: RATE * 4,
        },
    );
}

// ---------------------------------------------------------------------------
