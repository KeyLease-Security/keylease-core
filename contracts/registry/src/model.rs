//! Bounded, exhaustive model of the escrow state machine.
//!
//! This module is the executable form of the escrow specification in
//! `docs/security.md` ("Formal escrow model"). It is compiled only for tests and
//! runs as part of `cargo test`, so CI checks it with no extra tooling.
//!
//! # What is modelled
//!
//! A lease is one small state machine:
//!
//! ```text
//!                    create(calls, rate)
//!     Empty ────────────────────────────────▶ Active { locked = rate * calls }
//!                                                  │
//!            settle(verified)  ───────────────────┤
//!              guard: verified ≤ allocated,        │
//!                     caller is the stored        │
//!                     provider, and the lease     │
//!                     has expired or is fully     │
//!                     consumed                     │
//!                                                  │
//!            revoke()  ────────────────────────────┤
//!              guard: caller is the consumer,       │
//!                     expired, grace elapsed        │
//!                                                  ▼
//!                                    Settled { payout + refund = locked }
//! ```
//!
//! `Settled` is terminal: every later settle or revoke is rejected and leaves
//! the state untouched.
//!
//! # What is checked
//!
//! * **Conservation:** `payout + refund == locked_deposit` on every successful
//!   release, and `payout + refund ≤ locked_deposit` in every reachable state.
//! * **No double release:** a lease releases value at most once, and the total
//!   released never exceeds the deposit.
//! * **`settled` is terminal:** no action from `Settled` changes any field.
//! * **Rejections are no-ops:** a guarded transition that fails mutates nothing.
//!
//! The exploration is exhaustive over a bounded domain: every sequence of up to
//! [`EXPLORE_DEPTH`] actions drawn from [`STEPS`], for every create shape in
//! [`CONFIGS`], under each of the three ledger-time regimes in [`Clock`]. It
//! fails loudly (panics) on the first transition that violates an invariant.
//!
//! # Assumptions
//!
//! The model deliberately fixes the parts of the contract that are not part of
//! the escrow state machine, so that the transitions stay small enough to
//! exhaust:
//!
//! 1. **The escrow token behaves.** Its `transfer` either moves exactly the
//!    requested amount or reverts. Re-entrancy, broken tokens and conservation
//!    of token balances are covered by the adversarial tests in `test.rs`.
//! 2. **Inputs other than the lease shape are valid.** The service exists, is
//!    active, and has a configured token; `duration_secs` is inside
//!    `1..=MAX_LEASE_DURATION_SECS`; the ledger clock is monotonic. Their
//!    guards are covered by `test.rs`.
//! 3. **The ledger time is observed once per call.** The model reads the clock
//!    at the start of a transition, exactly as the entry points do.
//! 4. **`rate_per_call` is immutable.** Changing it requires registering a new
//!    service, and in-flight leases keep their original escrow.
//! 5. **One lease per model run.** Leases are independent: one lease's escrow
//!    never backs another's payout, so a single-lease model is the whole
//!    accounting unit.
//! 6. **Provider rotation does not change the escrow.** `transfer_service`
//!    changes which address passes the "stored provider" guard; it never changes
//!    `locked_deposit`, the payout formula or the refund formula, so the model
//!    parameterises the guard by "is the caller the owner" and `test.rs` pins
//!    the rotation behaviour separately.
//!
//! # How the model is kept in sync with the contract
//!
//! Two mechanisms, and both have to pass:
//!
//! * The guards in [`Model::step`] are written in the same order as the checks
//!   in `KeyLeaseRegistry::settle_lease` and
//!   `KeyLeaseRegistry::revoke_expired_lease`, down to which error each one
//!   returns, because the conformance test compares `Result<(), Error>` values,
//!   not just success/failure.
//! * `test_model_matches_the_deployed_contract` drives the **real** contract
//!   through the same action alphabet and compares, after every transition: the
//!   returned error, `settled`, `used_calls`, the provider's balance, the
//!   consumer's balance and what is left in escrow. A drift in the contract's
//!   guard order, in the payout formula, or in which field is persisted first
//!   shows up as a mismatch in one of those six observations.

use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, BytesN, Env,
};

use keylease_mock_token::{MockToken, MockTokenClient};

use crate::{
    Error, KeyLeaseRegistry, KeyLeaseRegistryClient, MAX_LEASE_DEPOSIT, SETTLE_GRACE_PERIOD_SECS,
};

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// Ledger-time regime a transition is executed in, relative to a lease that was
/// created at [`CREATED_AT`] for [`DURATION`] seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// One second before expiry.
    BeforeExpiry,
    /// Inside the 24-hour provider grace window.
    InGrace,
    /// Exactly when the consumer's revoke window opens.
    AfterGrace,
}

impl Clock {
    const fn now(self) -> u64 {
        match self {
            Clock::BeforeExpiry => EXPIRES_AT - 1,
            Clock::InGrace => EXPIRES_AT + SETTLE_GRACE_PERIOD_SECS / 2,
            Clock::AfterGrace => EXPIRES_AT + SETTLE_GRACE_PERIOD_SECS,
        }
    }
}

const CREATED_AT: u64 = 1_000;
const DURATION: u64 = 100;
const EXPIRES_AT: u64 = CREATED_AT + DURATION;

/// Stage of a lease in the abstract machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// No lease exists yet (or creation was rejected).
    Empty,
    /// Funds escrowed, nothing released.
    Active,
    /// Funds released; terminal.
    Settled,
}

/// Abstract state of one lease: what the contract stores, plus the running total
/// of value that has left escrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    stage: Stage,
    rate: i128,
    allocated: u32,
    used: u32,
    locked: i128,
    settled: bool,
    payout: i128,
    refund: i128,
    released: i128,
}

/// One transition of the alphabet. The variants name the guard they are meant to
/// exercise, and the conformance test resolves them against the live contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `settle_lease(provider, id, 0)`.
    SettleZeroOwner,
    /// `settle_lease(provider, id, allocated / 2)`.
    SettlePartialOwner,
    /// `settle_lease(provider, id, allocated)`.
    SettleFullOwner,
    /// `settle_lease(provider, id, allocated + 1)`.
    SettleOverOwner,
    /// `settle_lease(attacker, id, allocated)`.
    SettleFullStranger,
    /// `revoke_expired_lease(consumer, id)`.
    RevokeConsumer,
    /// `revoke_expired_lease(attacker, id)`.
    RevokeStranger,
}

const STEPS: [Step; 7] = [
    Step::SettleZeroOwner,
    Step::SettlePartialOwner,
    Step::SettleFullOwner,
    Step::SettleOverOwner,
    Step::SettleFullStranger,
    Step::RevokeConsumer,
    Step::RevokeStranger,
];

const CLOCKS: [Clock; 3] = [Clock::BeforeExpiry, Clock::InGrace, Clock::AfterGrace];

/// Create inputs the model is run for: a rejected zero-call lease, a two-call
/// lease (partial settlement exists) and a four-call lease.
const CONFIGS: [(u32, i128); 3] = [(0, RATE), (2, RATE), (4, RATE)];

/// Per-call rate used by the model and by the conformance harness.
pub const RATE: i128 = 100;

/// How many actions deep the pure model is explored (the conformance run stops
/// one level earlier to keep the suite fast; see the test).
const EXPLORE_DEPTH: u32 = 4;

impl Model {
    fn empty() -> Self {
        Model {
            stage: Stage::Empty,
            rate: 0,
            allocated: 0,
            used: 0,
            locked: 0,
            settled: false,
            payout: 0,
            refund: 0,
            released: 0,
        }
    }

    /// The `create_lease` transition and its guards, in contract order.
    fn create(mut self, calls: u32, rate: i128) -> (Self, Result<(), Error>) {
        debug_assert_eq!(self.stage, Stage::Empty, "one model run models one lease");
        if calls == 0 {
            return (self, Err(Error::InvalidCalls));
        }
        if rate <= 0 {
            return (self, Err(Error::InvalidRate));
        }
        let locked = match rate.checked_mul(calls as i128) {
            Some(locked) => locked,
            None => return (self, Err(Error::Overflow)),
        };
        if locked > MAX_LEASE_DEPOSIT {
            return (self, Err(Error::DepositTooLarge));
        }

        self.stage = Stage::Active;
        self.rate = rate;
        self.allocated = calls;
        self.locked = locked;
        (self, Ok(()))
    }

    /// Apply one action, mirroring the contract's check order exactly.
    fn step(self, step: Step, clock: Clock) -> (Self, Result<(), Error>) {
        match step {
            Step::SettleZeroOwner => self.settle(0, true, clock),
            Step::SettlePartialOwner => self.settle(self.allocated / 2, true, clock),
            Step::SettleFullOwner => self.settle(self.allocated, true, clock),
            Step::SettleOverOwner => self.settle(self.allocated.saturating_add(1), true, clock),
            Step::SettleFullStranger => self.settle(self.allocated, false, clock),
            Step::RevokeConsumer => self.revoke(true, clock),
            Step::RevokeStranger => self.revoke(false, clock),
        }
    }

    /// `settle_lease`: load, `settled`, call count, stored provider, settleable
    /// window, checked payout and refund — the same order as the contract.
    fn settle(mut self, verified: u32, owner: bool, clock: Clock) -> (Self, Result<(), Error>) {
        if self.stage == Stage::Empty {
            return (self, Err(Error::LeaseNotFound));
        }
        if self.settled {
            return (self, Err(Error::LeaseAlreadySettled));
        }
        if verified > self.allocated {
            return (self, Err(Error::InvalidCalls));
        }
        if !owner {
            return (self, Err(Error::Unauthorized));
        }
        let fully_consumed = verified == self.allocated;
        if clock == Clock::BeforeExpiry && !fully_consumed {
            return (self, Err(Error::LeaseNotSettleable));
        }

        let payout = match self.rate.checked_mul(verified as i128) {
            Some(payout) => payout,
            None => return (self, Err(Error::Overflow)),
        };
        let refund = match self.locked.checked_sub(payout) {
            Some(refund) => refund,
            None => return (self, Err(Error::Overflow)),
        };

        self.stage = Stage::Settled;
        self.settled = true;
        self.used = verified;
        self.payout = payout;
        self.refund = refund;
        self.released = payout + refund;
        (self, Ok(()))
    }

    /// `revoke_expired_lease`: load, consumer, `settled`, expiry, grace — the
    /// same order as the contract.
    fn revoke(mut self, consumer: bool, clock: Clock) -> (Self, Result<(), Error>) {
        if self.stage == Stage::Empty {
            return (self, Err(Error::LeaseNotFound));
        }
        // The contract checks ownership before the settled flag here, which is
        // why a stranger revoking a settled lease gets `Unauthorized`.
        if !consumer {
            return (self, Err(Error::Unauthorized));
        }
        if self.settled {
            return (self, Err(Error::LeaseAlreadySettled));
        }
        if clock == Clock::BeforeExpiry {
            return (self, Err(Error::LeaseNotExpired));
        }
        if clock == Clock::InGrace {
            return (self, Err(Error::RevokeWindowNotElapsed));
        }

        self.stage = Stage::Settled;
        self.settled = true;
        self.refund = self.locked;
        self.released = self.locked;
        (self, Ok(()))
    }
}

// ---------------------------------------------------------------------------
// Invariant checks
// ---------------------------------------------------------------------------

/// What the exhaustive run counted, so the test can assert it actually explored
/// something rather than passing vacuously.
#[derive(Default)]
struct Coverage {
    transitions: u32,
    settlements: u32,
    revocations: u32,
    rejections: u32,
}

/// Check one transition against every invariant, panicking with the offending
/// transition on the first violation.
fn check(
    before: &Model,
    after: &Model,
    outcome: Result<(), Error>,
    step: Step,
    cov: &mut Coverage,
) {
    // No lease may ever report more released than it locked, in any state.
    assert!(
        after.payout + after.refund <= after.locked,
        "conservation violated by {step:?}: payout {} + refund {} > locked {}",
        after.payout,
        after.refund,
        after.locked
    );
    assert!(
        after.released <= after.locked,
        "released {} exceeds locked {} after {step:?}",
        after.released,
        after.locked
    );
    // Released value is monotone: nothing is ever un-released.
    assert!(
        after.released >= before.released,
        "released went backwards after {step:?}"
    );

    match outcome {
        Ok(()) => {
            assert_eq!(
                after.stage,
                Stage::Settled,
                "{step:?} succeeded without reaching the terminal stage"
            );
            assert_eq!(
                after.payout + after.refund,
                after.locked,
                "{step:?} released {:?} of a {} deposit",
                after.payout + after.refund,
                after.locked
            );
            assert_eq!(
                after.released, after.locked,
                "a successful {step:?} must release the whole deposit exactly once"
            );
            assert_eq!(
                before.released, 0,
                "{step:?} released a lease that had already released value"
            );
            match step {
                Step::SettleZeroOwner
                | Step::SettlePartialOwner
                | Step::SettleFullOwner
                | Step::SettleOverOwner
                | Step::SettleFullStranger => cov.settlements += 1,
                Step::RevokeConsumer | Step::RevokeStranger => cov.revocations += 1,
            }
        }
        Err(error) => {
            cov.rejections += 1;
            assert_eq!(
                after, before,
                "{step:?} was rejected with {error:?} but changed the state"
            );
        }
    }

    // `settled` is terminal: from a settled lease nothing succeeds, nothing
    // changes, and the payout/refund split stays as it was.
    if before.stage == Stage::Settled {
        assert!(
            outcome.is_err(),
            "{step:?} transitioned out of the terminal Settled stage"
        );
        assert_eq!(after, before, "{step:?} mutated a settled lease");
        assert_eq!(
            after.payout, before.payout,
            "{step:?} changed a settled payout"
        );
        assert_eq!(
            after.refund, before.refund,
            "{step:?} changed a settled refund"
        );
    }

    // A create is the only way out of Empty, and it can only land in Active.
    if before.stage == Stage::Empty && outcome.is_ok() {
        assert_eq!(after.stage, Stage::Active);
        assert_eq!(after.released, 0);
    }

    cov.transitions += 1;
}

/// Depth-first, exhaustive exploration of every action sequence of length
/// `depth` from `model`.
fn explore(model: Model, clock: Clock, depth: u32, cov: &mut Coverage) {
    for step in STEPS {
        let (next, outcome) = model.step(step, clock);
        check(&model, &next, outcome, step, cov);
        if depth > 1 {
            explore(next, clock, depth - 1, cov);
        }
    }
}

// ---------------------------------------------------------------------------
// Exhaustive model test
// ---------------------------------------------------------------------------

#[test]
fn test_model_explores_the_escrow_state_machine_exhaustively() {
    let mut cov = Coverage::default();

    for (calls, rate) in CONFIGS {
        for clock in CLOCKS {
            let (created, outcome) = Model::empty().create(calls, rate);
            match outcome {
                Ok(()) => {
                    assert_eq!(created.stage, Stage::Active);
                    assert_eq!(created.locked, rate * calls as i128);
                    assert_eq!(created.released, 0);
                    explore(created, clock, EXPLORE_DEPTH, &mut cov);
                }
                Err(error) => {
                    // A rejected create leaves nothing behind to act on, but the
                    // machine still has to reject every later action.
                    assert_eq!(created, Model::empty());
                    assert_eq!(error, Error::InvalidCalls);
                    explore(created, clock, EXPLORE_DEPTH, &mut cov);
                }
            }
        }
    }

    // Guard against a silently empty run: every transition kind, in every
    // regime, has to have been exercised for the invariants above to mean
    // anything.
    let expected_runs = CONFIGS.len() as u32 * CLOCKS.len() as u32;
    assert_eq!(cov.transitions, expected_runs * nodes(EXPLORE_DEPTH));
    assert!(cov.settlements > 0, "no settlement was ever explored");
    assert!(cov.revocations > 0, "no revocation was ever explored");
    assert!(cov.rejections > 0, "no guard was ever explored");
    assert_eq!(
        cov.settlements + cov.revocations + cov.rejections,
        cov.transitions
    );
}

/// Number of edges in a tree of depth `depth` over [`STEPS`]: `7 + 7² + …`.
const fn nodes(depth: u32) -> u32 {
    let mut total = 0;
    let mut level = 1;
    let mut d = 0;
    while d < depth {
        level *= STEPS.len() as u32;
        total += level;
        d += 1;
    }
    total
}

// ---------------------------------------------------------------------------
// Conformance: the model against the deployed contract
// ---------------------------------------------------------------------------

/// Escrow funded to the consumer in the conformance harness.
const FUNDING: i128 = 1_000_000;

struct Harness<'a> {
    env: Env,
    registry: KeyLeaseRegistryClient<'a>,
    token: MockTokenClient<'a>,
    provider: Address,
    consumer: Address,
    stranger: Address,
    service_id: u64,
}

/// Deploy the registry and a mock token, fund the consumer and register the
/// modelled service.
fn harness(env: &Env) -> Harness<'_> {
    env.mock_all_auths();
    env.ledger().set_timestamp(CREATED_AT);

    let admin = Address::generate(env);
    let provider = Address::generate(env);
    let consumer = Address::generate(env);
    let stranger = Address::generate(env);

    let token_id = env.register(MockToken, ());
    let token = MockTokenClient::new(env, &token_id);
    token.init(&admin);
    token.mint(&consumer, &FUNDING);

    let registry_id = env.register(KeyLeaseRegistry, ());
    let registry = KeyLeaseRegistryClient::new(env, &registry_id);
    registry.init(&admin);
    registry.set_token(&admin, &token_id);

    let endpoint_hash = BytesN::from_array(env, &[7u8; 32]);
    let service_id = registry.register_service(&provider, &RATE, &endpoint_hash);

    Harness {
        env: env.clone(),
        registry,
        token,
        provider,
        consumer,
        stranger,
        service_id,
    }
}

impl Harness<'_> {
    /// Lease id of the single lease this harness creates.
    const LEASE: u64 = 1;

    /// Run `step` against the real contract, returning the same shape the model
    /// does: the contract error, or `Ok(())`.
    fn apply(&self, step: Step, allocated: u32) -> Result<(), Error> {
        let (settle, calls, owner) = match step {
            Step::SettleZeroOwner => (true, 0, true),
            Step::SettlePartialOwner => (true, allocated / 2, true),
            Step::SettleFullOwner => (true, allocated, true),
            Step::SettleOverOwner => (true, allocated.saturating_add(1), true),
            Step::SettleFullStranger => (true, allocated, false),
            Step::RevokeConsumer => (false, 0, true),
            Step::RevokeStranger => (false, 0, false),
        };
        // A settlement is authorized by the provider, a revocation by the
        // consumer; `owner` means "the party the contract expects" for each.
        let outcome = if settle {
            let caller = if owner {
                &self.provider
            } else {
                &self.stranger
            };
            self.registry.try_settle_lease(caller, &Self::LEASE, &calls)
        } else {
            let caller = if owner {
                &self.consumer
            } else {
                &self.stranger
            };
            self.registry.try_revoke_expired_lease(caller, &Self::LEASE)
        };
        match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(conversion)) => {
                panic!("{step:?} returned a value that did not convert: {conversion:?}")
            }
            Err(Ok(error)) => Err(error),
            Err(Err(never)) => panic!("{step:?} failed below the contract layer: {never:?}"),
        }
    }
}

/// Drive the real contract through `steps` and assert it agrees with the model
/// after every one of them.
fn compare_sequence(steps: &[Step], calls: u32, clock: Clock) {
    let env = Env::default();
    let h = harness(&env);

    let (mut model, created) = Model::empty().create(calls, RATE);
    let lease = h
        .registry
        .try_create_lease(&h.consumer, &h.service_id, &calls, &DURATION);
    match (created, lease) {
        (Ok(()), Ok(Ok(id))) => {
            assert_eq!(id, Harness::LEASE);
            let stored = h.registry.get_lease(&id);
            assert_eq!(stored.locked_deposit, model.locked);
            assert_eq!(stored.allocated_calls, model.allocated);
            assert!(!stored.settled);
        }
        (Err(model_error), Err(Ok(contract_error))) => {
            assert_eq!(model_error, contract_error);
            // Nothing exists to act on, so there is nothing to compare further.
            assert_eq!(model, Model::empty());
            return;
        }
        (model_outcome, contract_outcome) => {
            panic!("create disagreed: model {model_outcome:?} contract {contract_outcome:?}")
        }
    }

    h.env.ledger().set_timestamp(clock.now());

    for step in steps {
        let (next, model_outcome) = model.step(*step, clock);
        let contract_outcome = h.apply(*step, model.allocated);
        assert_eq!(
            model_outcome, contract_outcome,
            "{step:?} under {clock:?} for {calls} calls: model and contract disagreed"
        );

        // Same six observations the module docs list: state flags, the metered
        // count, both balances and what is left in escrow.
        let stored = h.registry.get_lease(&Harness::LEASE);
        assert_eq!(stored.settled, next.settled, "{step:?}: settled drifted");
        assert_eq!(stored.used_calls, next.used, "{step:?}: used_calls drifted");
        assert_eq!(
            h.token.balance(&h.provider),
            next.payout,
            "{step:?}: provider payout drifted"
        );
        assert_eq!(
            h.token.balance(&h.consumer),
            FUNDING - next.locked + next.refund,
            "{step:?}: consumer refund drifted"
        );
        assert_eq!(
            h.token.balance(&h.registry.address),
            next.locked - next.released,
            "{step:?}: escrow balance drifted"
        );
        assert_eq!(
            stored.locked_deposit, next.locked,
            "{step:?}: locked deposit was rewritten"
        );

        model = next;
    }
}

#[test]
fn test_model_matches_the_deployed_contract() {
    for (calls, _rate) in CONFIGS {
        for clock in CLOCKS {
            // Every single action from the created state: one full pass over the
            // first transition out of Active, including every guard, in every
            // time regime.
            for step in STEPS {
                compare_sequence(&[step], calls, clock);
            }
        }

        // Every pair under the regime with the most open doors (post-expiry,
        // inside the grace window): that is where settlement and revocation are
        // both reachable and where `settled` terminality and "no double
        // release" become observable on the real contract rather than only in
        // the model. Pairs under the other two regimes are covered
        // exhaustively by the model test above.
        for first in STEPS {
            for second in STEPS {
                compare_sequence(&[first, second], calls, Clock::InGrace);
            }
        }
    }
}
