#![no_std]
// No unsafe code in contract crates (CONTRIBUTING.md#unsafe-code-and-std).
// `forbid` on the deployed Wasm. On the host, soroban-sdk's `testutils`
// feature registers contracts through `ctor`, whose expansion carries its own
// `#[allow(unsafe_code)]`, which `forbid` rejects; `deny` still rejects any
// unsafe written in this crate. scripts/check-no-std.sh checks these lines.
#![cfg_attr(target_family = "wasm", forbid(unsafe_code))]
#![cfg_attr(not(target_family = "wasm"), deny(unsafe_code))]

//! # KeyLease Mock Token
//!
//! A deliberately small, audit-friendly SEP-41 style fungible token used by the
//! registry's integration tests and local sandboxes. It implements exactly the
//! surface the registry depends on (`transfer`, `balance`) plus the standard
//! allowance/inspection methods, so it can be used anywhere a `token::Client`
//! is expected.
//!
//! **Not for production.** The admin can mint without limit.
//!
//! Every balance and allowance movement uses checked arithmetic and returns
//! [`TokenError::Overflow`] instead of trapping: the workspace release profile
//! sets `overflow-checks = true`, so a plain `+` that overflowed would fail the
//! transaction with a host error rather than a token error.

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, String};

/// Storage keys for the mock token.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Admin,
    Decimals,
    Name,
    Symbol,
    Balance(Address),
    Allowance(Address, Address),
    AllowanceExpiry(Address, Address),
}

/// Error codes returned by the mock token.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TokenError {
    /// An amount was zero or negative where a positive amount is required.
    InvalidAmount = 1,
    /// The source account does not hold enough tokens.
    InsufficientBalance = 2,
    /// The spender's allowance is too small.
    InsufficientAllowance = 3,
    /// The token has already been initialized.
    AlreadyInitialized = 4,
    /// The token has not been initialized.
    NotInitialized = 5,
    /// Only the admin may perform this operation.
    Unauthorized = 6,
    /// A balance or allowance would not fit in `i128` after the operation.
    ///
    /// Codes are append-only: a caller maps the number, never the name.
    Overflow = 7,
}

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    /// Initialize the token and appoint `admin` as the sole minter.
    pub fn init(env: Env, admin: Address) -> Result<(), TokenError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(TokenError::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Decimals, &7u32);
        env.storage().instance().set(
            &DataKey::Name,
            &String::from_str(&env, "KeyLease Mock Token"),
        );
        env.storage()
            .instance()
            .set(&DataKey::Symbol, &String::from_str(&env, "KLMOCK"));
        Ok(())
    }

    /// Mint `amount` new tokens to `to`. Admin only.
    pub fn mint(env: Env, to: Address, amount: i128) -> Result<(), TokenError> {
        let admin = read_admin(&env)?;
        admin.require_auth();

        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }

        let key = DataKey::Balance(to.clone());
        let balance = read_balance(&env, &to);
        let minted = balance.checked_add(amount).ok_or(TokenError::Overflow)?;
        env.storage().persistent().set(&key, &minted);
        Ok(())
    }

    /// Transfer `amount` from `from` to `to`.
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) -> Result<(), TokenError> {
        from.require_auth();
        move_balance(&env, &from, &to, amount)
    }

    /// Transfer `amount` from `from` to `to` using `spender`'s allowance.
    pub fn transfer_from(
        env: Env,
        spender: Address,
        from: Address,
        to: Address,
        amount: i128,
    ) -> Result<(), TokenError> {
        spender.require_auth();

        // Validate before drawing anything down. A negative `amount` passes the
        // `allowance < amount` test below and would otherwise widen the spender's
        // allowance before `move_balance` rejected the transfer.
        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }
        let allowance = read_allowance(&env, &from, &spender);
        if allowance < amount {
            return Err(TokenError::InsufficientAllowance);
        }

        move_balance(&env, &from, &to, amount)?;

        // Reached only after the transfer succeeded, so `0 <= amount <=
        // allowance` and this subtraction cannot underflow or overflow.
        let key = DataKey::Allowance(from, spender);
        env.storage().persistent().set(&key, &(allowance - amount));
        Ok(())
    }

    /// Approve `spender` to spend up to `amount` of `from`'s tokens.
    ///
    /// `live_until_ledger` of `0` means the allowance never expires.
    pub fn approve(
        env: Env,
        from: Address,
        spender: Address,
        amount: i128,
        live_until_ledger: u32,
    ) -> Result<(), TokenError> {
        from.require_auth();

        if amount < 0 {
            return Err(TokenError::InvalidAmount);
        }

        let key = DataKey::Allowance(from.clone(), spender.clone());
        env.storage().persistent().set(&key, &amount);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND);

        let expiry_key = DataKey::AllowanceExpiry(from, spender);
        env.storage()
            .persistent()
            .set(&expiry_key, &live_until_ledger);
        env.storage()
            .persistent()
            .extend_ttl(&expiry_key, TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Remaining allowance `spender` may draw from `from`.
    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        read_allowance(&env, &from, &spender)
    }

    /// Token balance of `id`.
    pub fn balance(env: Env, id: Address) -> i128 {
        read_balance(&env, &id)
    }

    /// Number of decimals (always 7).
    pub fn decimals(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Decimals)
            .unwrap_or(7)
    }

    /// Human-readable token name.
    pub fn name(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Name)
            .unwrap_or_else(|| String::from_str(&env, "KeyLease Mock Token"))
    }

    /// Token ticker symbol.
    pub fn symbol(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Symbol)
            .unwrap_or_else(|| String::from_str(&env, "KLMOCK"))
    }

    /// The configured admin/minter.
    pub fn admin(env: Env) -> Result<Address, TokenError> {
        read_admin(&env)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

const TTL_THRESHOLD: u32 = 17_280;
const TTL_EXTEND: u32 = 518_400;

fn read_admin(env: &Env) -> Result<Address, TokenError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(TokenError::NotInitialized)
}

fn read_balance(env: &Env, id: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::Balance(id.clone()))
        .unwrap_or(0)
}

fn move_balance(env: &Env, from: &Address, to: &Address, amount: i128) -> Result<(), TokenError> {
    if amount <= 0 {
        return Err(TokenError::InvalidAmount);
    }

    let from_balance = read_balance(env, from);
    if from_balance < amount {
        return Err(TokenError::InsufficientBalance);
    }

    let from_key = DataKey::Balance(from.clone());
    let to_key = DataKey::Balance(to.clone());

    // The credit is the side that can overflow: the check above guarantees
    // `from_balance >= amount`, so the debit cannot underflow, while a recipient
    // already holding `i128::MAX` would overflow a plain addition. Computing it
    // first also means a rejected transfer writes no debit.
    let to_balance = read_balance(env, to)
        .checked_add(amount)
        .ok_or(TokenError::Overflow)?;

    env.storage()
        .persistent()
        .set(&from_key, &(from_balance - amount));
    env.storage().persistent().set(&to_key, &to_balance);

    env.storage()
        .persistent()
        .extend_ttl(&from_key, TTL_THRESHOLD, TTL_EXTEND);
    env.storage()
        .persistent()
        .extend_ttl(&to_key, TTL_THRESHOLD, TTL_EXTEND);
    Ok(())
}

fn read_allowance(env: &Env, from: &Address, spender: &Address) -> i128 {
    let live_until: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::AllowanceExpiry(from.clone(), spender.clone()))
        .unwrap_or(0);
    if live_until != 0 && env.ledger().sequence() > live_until {
        return 0;
    }
    env.storage()
        .persistent()
        .get(&DataKey::Allowance(from.clone(), spender.clone()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{MockToken, MockTokenClient, TokenError};
    use soroban_sdk::{testutils::Address as _, Address, Env};

    struct Ctx<'a> {
        token: MockTokenClient<'a>,
        alice: Address,
        bob: Address,
    }

    fn setup(env: &Env) -> Ctx<'_> {
        env.mock_all_auths();

        let admin = Address::generate(env);
        let alice = Address::generate(env);
        let bob = Address::generate(env);

        let token_id = env.register(MockToken, ());
        let token = MockTokenClient::new(env, &token_id);
        token.init(&admin);

        Ctx { token, alice, bob }
    }

    #[test]
    fn mint_past_the_ceiling_reports_overflow() {
        let env = Env::default();
        let ctx = setup(&env);

        ctx.token.mint(&ctx.alice, &i128::MAX);
        assert_eq!(ctx.token.balance(&ctx.alice), i128::MAX);

        assert_eq!(
            ctx.token.try_mint(&ctx.alice, &1),
            Err(Ok(TokenError::Overflow))
        );
        assert_eq!(ctx.token.balance(&ctx.alice), i128::MAX);
    }

    #[test]
    fn transfer_into_a_saturated_account_reports_overflow() {
        let env = Env::default();
        let ctx = setup(&env);

        ctx.token.mint(&ctx.alice, &i128::MAX);
        ctx.token.mint(&ctx.bob, &i128::MAX);

        assert_eq!(
            ctx.token.try_transfer(&ctx.alice, &ctx.bob, &1),
            Err(Ok(TokenError::Overflow))
        );
        assert_eq!(ctx.token.balance(&ctx.alice), i128::MAX);
        assert_eq!(ctx.token.balance(&ctx.bob), i128::MAX);
    }

    #[test]
    fn transfer_from_checks_the_amount_before_drawing_the_allowance() {
        let env = Env::default();
        let ctx = setup(&env);

        ctx.token.mint(&ctx.alice, &1_000);
        ctx.token.approve(&ctx.alice, &ctx.bob, &i128::MAX, &0);

        // `i128::MIN` is smaller than every allowance, so a spender could once
        // turn it into a negative debit and widen its own allowance.
        assert_eq!(
            ctx.token
                .try_transfer_from(&ctx.bob, &ctx.alice, &ctx.bob, &i128::MIN),
            Err(Ok(TokenError::InvalidAmount))
        );
        assert_eq!(ctx.token.allowance(&ctx.alice, &ctx.bob), i128::MAX);
        assert_eq!(ctx.token.balance(&ctx.bob), 0);
    }

    #[test]
    fn transfer_from_spends_exactly_the_allowance() {
        let env = Env::default();
        let ctx = setup(&env);

        ctx.token.mint(&ctx.alice, &500);
        ctx.token.approve(&ctx.alice, &ctx.bob, &500, &0);
        ctx.token
            .transfer_from(&ctx.bob, &ctx.alice, &ctx.bob, &500);

        assert_eq!(ctx.token.allowance(&ctx.alice, &ctx.bob), 0);
        assert_eq!(ctx.token.balance(&ctx.alice), 0);
        assert_eq!(ctx.token.balance(&ctx.bob), 500);
    }

    #[test]
    fn error_codes_keep_their_on_chain_numbers() {
        assert_eq!(TokenError::InvalidAmount as u32, 1);
        assert_eq!(TokenError::InsufficientBalance as u32, 2);
        assert_eq!(TokenError::InsufficientAllowance as u32, 3);
        assert_eq!(TokenError::AlreadyInitialized as u32, 4);
        assert_eq!(TokenError::NotInitialized as u32, 5);
        assert_eq!(TokenError::Unauthorized as u32, 6);
        assert_eq!(TokenError::Overflow as u32, 7);
    }
}
