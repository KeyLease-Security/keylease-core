# Design Note: Admin Emergency Pause

- **Issue:** #120
- **Status:** Pending Maintainer Approval
- **Author:** Antigravity Agent
- **Target Contract:** `KeyLeaseRegistry` (`contracts/registry`)

---

## 1. Context and Problem

KeyLease escrow holds funds on behalf of consumers and providers. In the event
of an identified vulnerability, token anomaly, or external emergency, the
protocol administrator needs a mechanism to halt inbound risks immediately.

However, a naive pause mechanism that freezes all contract executions would
trap consumer funds locked in escrow and prevent providers from claiming earned
settlements. This violates the core trust model of KeyLease.

---

## 2. Invariants

1. **Non-blocking settlement and refunds (Core Invariant):** `settle_lease` and
   `revoke_expired_lease` are **NEVER blocked** by an emergency pause.
   Escrowed consumer deposits and provider payouts must never be held hostage
   by a contract pause.
2. **Safe default for upgrades:** Existing contract deployments upgraded to a
   Wasm containing the emergency pause logic must default to unpaused without
   requiring data migrations. An absent pause storage key evaluates to `false`
   (unpaused).
3. **Admin exclusivity:** Only the configured contract `admin` can pause or
   unpause the contract. Unauthorized callers fail with `Error::Unauthorized`.
4. **Transparent observability:** State transitions between paused and unpaused
   must emit typed Soroban contract events (`EmergencyPaused` and
   `EmergencyUnpaused`).

---

## 3. Entry Point Classification

### Blocked Entry Points (Fail with `Error::ContractPaused`)

The following state-mutating entry points accept new commitments or modify
service definitions, and are therefore blocked during an emergency pause:

| Function | Reason for Blocking |
| --- | --- |
| `register_service` | Halts registration of new services during an incident. |
| `set_service_active` | Prevents modifying service active/inactive status during an incident. |
| `create_lease` | Prevents consumers from depositing funds or minting new leases. |
| `create_lease_for` | Shares `create_lease`'s mint path, so a delegate cannot bypass the pause. |

### Available Entry Points (Never Blocked)

The following entry points remain fully accessible while the contract is
paused:

| Function | Reason for Remaining Available |
| --- | --- |
| `settle_lease` | **Invariant:** Providers must always be able to claim earned revenue and disburse remaining refunds to consumers. |
| `revoke_expired_lease` | **Invariant:** Consumers must always be able to reclaim full escrow for abandoned or unserved leases after the 24h grace period. |
| `pause` | Admin can invoke to enter or re-affirm the paused state. |
| `unpause` | Admin can invoke to resume normal operations. |
| `upgrade` | Admin must be able to deploy bug fixes or patches during an incident. |
| `set_token` | Admin configuration function. |
| `init` | Initialization (one-time). |
| All read views | All `get_*`, `lease_status`, `remaining_calls`, `locked_balance`, `version`, `is_paused` read-only views remain accessible. |

---

## 4. Storage Design

- **Storage Key:** `DataKey::Paused` placed in **instance storage**.
- **Data Type:** `bool`.
- **Default Value:** `false` (unpaused).
- **Upgrade Compatibility:** an absent key reads as unpaused, so no migration
  is needed.

```rust
pub fn is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
}
```

Because `unwrap_or(false)` is used, any contract upgraded from an earlier Wasm
version where `DataKey::Paused` was not set immediately evaluates to unpaused
without data migration or re-initialization.

---

## 5. Event Schema

Typed events emitted on state transitions (in `events.rs`):

```rust
/// Emitted when the admin triggers an emergency pause.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPaused {
    /// Payload schema version; see the events module docs.
    pub schema_version: u32,
    /// Admin that authorized the pause.
    pub admin: Address,
}

/// Emitted when the admin unpauses the registry.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyUnpaused {
    /// Payload schema version; see the events module docs.
    pub schema_version: u32,
    /// Admin that authorized the unpause.
    pub admin: Address,
}
```

---

## 6. Error Handling

A new error discriminant is added to `errors.rs`:

```rust
/// The contract is currently paused by admin emergency action.
ContractPaused = 44,
```

`44` is the next free code after `InvalidEndpointHash = 43` on `main`; no
existing discriminant is renumbered.

---

## 7. Public ABI Additions

```rust
impl KeyLeaseRegistry {
    /// Pause new service registrations and lease creations. Admin only.
    pub fn pause(env: Env, admin: Address) -> Result<(), Error>;

    /// Resume normal operations. Admin only.
    pub fn unpause(env: Env, admin: Address) -> Result<(), Error>;

    /// Query whether the contract is currently paused.
    pub fn is_paused(env: Env) -> bool;
}
```
