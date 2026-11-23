# Glossary

Every protocol term used across the guides, defined once. Each entry links to
the page that explains it in depth.

Admin
:   The address set by `init` that may configure the escrow token and propose,
    cancel or execute upgrades. It has no power over individual leases or
    escrowed funds. See [Contract reference → Administration](contract-reference.md#administration)
    and [Security model → Trust assumptions](security.md#trust-assumptions).

Attestor
:   An optional key a provider attaches to a service so `settle_lease_attested`
    can verify a signed usage report. See [Contract reference](contract-reference.md).

Base unit
:   The smallest indivisible amount of the configured escrow token. Every
    amount in the contract (`rate_per_call`, `locked_deposit`, payouts,
    refunds) is an `i128` of base units. For the native SAC a base unit is a
    [stroop](#stroop). See [Rate units and stroop conversion](contract-reference.md#rate-units-and-stroop-conversion).

Consumer
:   The account that escrows a deposit with `create_lease`, receives refunds and
    may revoke an unsettled lease after the grace window. See
    [Protocol → Actors](protocol.md#actors) and the [Consumer guide](consumer-guide.md).

Delegate
:   An account that submits `create_lease_for` on behalf of a consumer and pays
    the transaction fee. It gains no claim on the lease. See
    [Contract reference → `create_lease_for`](contract-reference.md#create_lease_for).

Deposit (locked deposit)
:   `rate_per_call × requested_calls`, transferred from the consumer into
    escrow when a lease is created and capped by `MAX_LEASE_DEPOSIT`. See
    [Protocol → Worked economics](protocol.md#worked-economics).

Early cancellation
:   A consumer request to end an active lease before expiry, which takes effect
    only if the provider approves it. See
    [Protocol → Early cancellation with provider consent](protocol.md#early-cancellation-with-provider-consent).

Endpoint hash
:   A 32-byte commitment to the provider's off-chain endpoint descriptor,
    stored on the service. Consumers check it before escrowing. See
    [Consumer guide → Pick a service](consumer-guide.md#1-pick-a-service).

Escrow
:   The registry contract's own token balance, holding every live lease's
    deposit until settlement or revocation. See
    [Architecture → The money paths in order](architecture.md#the-money-paths-in-order).

Escrow token
:   The single SEP-41 token configured with `set_token`; usually the native
    SAC. See [Networks → Native SAC vs. Custom SEP-41 Token](networks.md#native-sac-vs-custom-sep-41-token).

Event schema version
:   The `schema_version` field carried in every event payload, bumped when a
    payload changes shape. See [Integration → Event schema version](integration.md#event-schema-version).

Expiry (`expires_at`)
:   The ledger timestamp after which a lease no longer authorizes calls:
    creation time plus `duration_secs`. See [Protocol → Lifecycle](protocol.md#lifecycle).

Grace window
:   The 24 hours (`SETTLE_GRACE_PERIOD_SECS`) after expiry during which only
    the provider may settle; the consumer may revoke once it has elapsed. See
    [Protocol → The 24-hour grace window](protocol.md#the-24-hour-grace-window).

Lease
:   A time-bound, escrowed authorization for a fixed number of calls to one
    service, identified by a monotonic `lease_id`. See
    [Protocol → Lease](protocol.md#lease).

Lease status
:   The derived state of a lease: `Active`, `Expired`, `Settled`, `Revoked` or
    `Cancelled`. See [Contract reference → `LeaseStatus` and the `settled` flag](contract-reference.md#leasestatus-and-the-settled-flag).

Payout
:   `rate_per_call × verified_calls`, transferred from escrow to the provider at
    settlement. See [Architecture → `settle_lease`](architecture.md#2-settle_lease-two-transfers-out-of-escrow).

Provider
:   The account that registers a service, verifies leases off-chain, and settles
    them for a payout. See [Protocol → Actors](protocol.md#actors) and the
    [Provider guide](provider-guide.md).

Rate (`rate_per_call`)
:   The price of one metered call in [base units](#base-unit) of the escrow
    token, snapshotted onto each lease at creation. See
    [Rate units and stroop conversion](contract-reference.md#rate-units-and-stroop-conversion).

Refund
:   The part of a deposit returned to the consumer: the unused remainder at
    settlement, or the whole deposit on revocation. See
    [FAQ → refunds](faq.md#how-and-when-do-consumers-receive-refunds).

Registry
:   The `KeyLeaseRegistry` Soroban contract that holds services, leases and
    escrow. See [Architecture → `KeyLeaseRegistry`](architecture.md#keyleaseregistry).

Revocation
:   `revoke_expired_lease`: the consumer reclaims the full deposit of a lease
    the provider never settled, once the grace window has elapsed. See
    [Architecture → `revoke_expired_lease`](architecture.md#3-revoke_expired_lease-one-transfer-after-the-grace-window).

SAC (Stellar Asset Contract)
:   The built-in contract that exposes a classic Stellar asset, including
    native XLM, through the SEP-41 interface. See
    [Networks → Escrow asset: the native SAC](networks.md#escrow-asset-the-native-sac).

SEP-41
:   The Stellar token interface (`transfer`, `balance`, …) the registry uses to
    move escrow. See [Architecture → SEP-41 token](architecture.md#sep-41-token).

Service
:   A provider's priced listing: provider address, `rate_per_call`,
    `endpoint_hash` and an active flag, identified by `service_id`. See
    [Protocol → Service](protocol.md#service).

Settlement
:   `settle_lease`: the provider reports verified calls, is paid for them, and
    the remainder is refunded in the same call. See
    [Protocol → When can a lease be settled?](protocol.md#when-can-a-lease-be-settled).

Stroop
:   The smallest unit of XLM: `1 XLM = 10,000,000 stroops`. It is the base unit
    of the native SAC, not of every token. See
    [Rate units and stroop conversion](contract-reference.md#rate-units-and-stroop-conversion).

TTL (time to live)
:   How many ledgers a Soroban storage entry has left before eviction; the
    registry extends it on writes and through `bump_service` / `bump_lease`. See
    [Protocol → Storage and lifetime](protocol.md#storage-and-lifetime).

Upgrade timelock
:   The `UPGRADE_DELAY_SECS` delay between `propose_upgrade` and
    `execute_upgrade`. See [Design: time-locked upgrades](design/upgrade-timelock.md).
