# Design note — Two-step admin transfer (#121)

> **Status:** Proposed — **awaiting maintainer approval**. Per
> [CONTRIBUTING](https://github.com/KeyLease-Security/keylease-core/blob/main/CONTRIBUTING.md),
> a maintainer must approve this design; the accompanying PR implements it so
> the review can happen against working code and tests.
>
> **Issue:** [#121 — Make admin transfer a two-step, accept-based operation](https://github.com/KeyLease-Security/keylease-core/issues/121)
> **Touches:** `lib.rs`, `storage.rs`, `types.rs`, `events.rs`, `errors.rs`

## 1. Problem

`init` stores the admin once and nothing can change it. The admin is the only
account that can call `set_token` and the upgrade entry points, so a key that has to be
rotated (a departing operator, a suspected leak, a move from a single key to a
multisig) cannot be rotated: the deployment must keep trusting the old key
forever, or be abandoned along with its contract id and in-flight escrow.

A naive `set_admin(admin, new_admin)` would fix rotation but introduce a worse
failure: one typo, or an address nobody holds the key for, permanently strands
upgrade and token control.

## 2. Invariants

After this change the following must hold:

1. The stored admin changes **only** in `accept_admin`, and only to the address
   that the sitting admin proposed.
2. A hand-over needs authorization from **both** the outgoing admin
   (`propose_admin`) and the incoming admin (`accept_admin`). No other address
   can complete, start or cancel one.
3. At most one proposal exists at any time, and it can be accepted only before
   its `expires_at`.
4. A proposal made by an admin who is no longer in office is never acceptable.
5. On acceptance the previous admin loses every admin right in the same
   transaction; there is no window with two admins.
6. Escrow, services and leases are untouched by any admin-rotation call.
7. Existing storage keys, event payloads and error codes are unchanged.

## 3. Approach

### Entry points

| Entry point | Auth | Effect |
| --- | --- | --- |
| `propose_admin(admin, new_admin) -> u64` | current admin | Store `PendingAdmin`, replacing any earlier one; return `expires_at`. |
| `cancel_admin_transfer(admin)` | current admin | Remove `PendingAdmin`, expired or not. |
| `accept_admin(new_admin)` | proposed admin | Check proposal, set `Admin`, remove `PendingAdmin`. |
| `get_pending_admin() -> Option<PendingAdminTransfer>` | — | Pure read. |

### State

```rust
pub struct PendingAdminTransfer {
    pub proposed_by: Address, // admin at proposal time
    pub new_admin: Address,
    pub proposed_at: u64,     // ledger timestamp
    pub expires_at: u64,      // proposed_at + ADMIN_TRANSFER_WINDOW_SECS
}
```

Stored under a new `DataKey::PendingAdmin` in **instance** storage. It is a
single bounded value tied to the configuration it would change, every rotation
call extends the instance TTL, and so it needs no TTL policy of its own. The new
variant is additive, so existing keys keep decoding across the upgrade.

### Expiry

`ADMIN_TRANSFER_WINDOW_SECS = 7 days`, a compile-time constant. That is long
enough for a multisig successor to gather signatures, and short enough that a
forgotten proposal does not stay live as a hand-over to an address whose key
may since have leaked. Acceptance requires `now < expires_at`. An expired
proposal stays in storage (and visible through `get_pending_admin`) until the
admin cancels or replaces it, because an erroring `accept_admin` reverts and
cannot clean up after itself.

### Events and errors

New events (no topics, matching the other admin events; payloads carry
`schema_version`): `admin_transfer_proposed { admin, new_admin, expires_at }`,
`admin_transfer_canceled { admin, new_admin }`,
`admin_transfer_accepted { previous_admin, new_admin }`. Adding events does not
change any existing payload, so `EVENT_SCHEMA_VERSION` stays at `1`.

New errors are appended: `NoPendingAdminTransfer = 40`,
`AdminTransferExpired = 41`, `InvalidAdmin = 42` (proposing the current admin).

### Rejected alternatives

- **Single-step `set_admin`.** One call, but a typo or unreachable address
  permanently loses admin control. This is the failure the issue is about.
- **Requiring both signatures on `accept_admin`.** That would make acceptance a
  coordinated multi-party transaction and add nothing: the outgoing admin
  already authorized the exact successor in `propose_admin`, and can cancel
  until acceptance.
- **Caller-chosen expiry.** More flexible, but a long or unbounded window brings
  back the stale-proposal risk. It can be added later if operators need it.
- **Persistent storage with its own TTL.** More rent policy for a value that is
  only ever one entry and naturally belongs with the configuration.
- **Lost-key recovery (guardian, timelocked override).** Out of scope. Two-step
  rotation needs the current key. Recovery from a key that is *already* lost is
  what a multisig admin account is for, since Stellar lets the account replace
  its own signers.

## 4. Threat model

| Adversarial case | Handling |
| --- | --- |
| A third party calls `accept_admin` for itself | Not the proposed address → `Unauthorized`. |
| A third party calls `accept_admin(new_admin)` with its own signature | `new_admin.require_auth()` fails in the host. |
| The outgoing admin accepts on the successor's behalf | Same: only the successor's signature satisfies `require_auth`. |
| A third party proposes or cancels | `require_admin` → `Unauthorized`; with real auth, `require_auth` on the admin fails first. |
| The proposed admin cancels or re-proposes | Not the stored admin → `Unauthorized`. |
| A superseded successor accepts after a new proposal | Only the latest proposal exists → `Unauthorized`. |
| A proposal is replayed after the role moved on | Acceptance clears it → `NoPendingAdminTransfer`. |
| A proposal survives an upgrade or admin change | `proposed_by` must equal the sitting admin → `Unauthorized`. |
| A months-old proposal is accepted after the successor's key leaked | Expired → `AdminTransferExpired`. |
| Admin rotation is used to reach escrow | Rotation calls never call the token or touch lease state. |

## 5. Test plan

All in `contracts/registry/src/test.rs`:

- Happy path: propose → accept, with an exact event for each step, the single
  authorizer for each step, and `get_pending_admin` before and after.
- Every admin right moves: the old admin is rejected by `set_token`,
  `propose_upgrade`, `execute_upgrade`, `cancel_upgrade` and `propose_admin`;
  the new admin can set the token and rotate again.
- Third-party, outgoing-admin and no-proposal `accept_admin` calls are rejected.
- **Real-auth adversarial test** (`mock_auths`, not `mock_all_auths`): an
  attacker's signature cannot propose for the admin, and neither the attacker's
  nor the outgoing admin's signature can accept for the successor. The
  successor's own signature succeeds.
- Cancel: event, proposal gone, later accept fails; cancel is admin-only (third
  party and proposed admin both rejected); cancel with nothing pending fails.
- Expiry: accept at `expires_at` fails, at `expires_at - 1` succeeds; an expired
  proposal can still be cancelled.
- A new proposal replaces the old one; an accepted proposal cannot be replayed;
  a proposal recorded under a previous admin is rejected.
- Rotation does not move escrow, and a lease still settles afterwards.
- Propose and accept extend the instance TTL.
