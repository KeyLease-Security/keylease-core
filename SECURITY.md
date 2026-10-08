# Security Policy

KeyLease escrows real value on-chain. Security reports are taken seriously and
are the fastest way to get a fix shipped.

## Reporting a vulnerability

Please **do not** open a public issue for security-sensitive reports.

Report privately through GitHub's
[private vulnerability reporting](https://github.com/KeyLease-Security/keylease-core/security/advisories/new)
for this repository, or, if that is unavailable, email `security@keylease.dev`
with a description and reproduction steps.

A good report includes:

- the affected entry point(s) (e.g. `settle_lease`) and the deployed contract id
  or commit you tested
- the invariant you believe is violated, and the preconditions required to
  violate it
- steps to reproduce, or a proof of concept
- the expected failure mode (what the contract should have done instead)

Please give us a reasonable window to ship a fix before public disclosure. We
will credit reporters in the release notes unless you ask us not to.

## Scope

**In scope** — the `KeyLeaseRegistry` contract (`contracts/registry`):

- theft, loss, or stranding of escrowed deposits
- double settlement, re-entrancy, or a payout larger than
  `allocated_calls × rate_per_call`
- authorization bypasses: acting on a lease or service you do not own
- the consumer refund path (`revoke_expired_lease`) being blocked or front-run
- arithmetic overflow/truncation in deposit, payout, or timestamp math
- storage TTL/griefing issues that evict a live lease or service
- a way to make a settled lease settle again, or to resurrect one

**Out of scope**

- vulnerabilities in the gateway (`@keylease/cli`, `@keylease/proxy`) — report
  them on
  [keylease-gateway](https://github.com/KeyLease-Security/keylease-gateway)
- `contracts/mock_token` — it is a deliberately insecure test-only token with
  unlimited mint; findings there are expected and not actionable
- issues queued in the [Stellar Core](https://github.com/stellar/stellar-core)
  or [Soroban](https://github.com/stellar/rs-soroban-env) reference
  implementations
- denial of service against the RPC provider itself
- economic design opinions (rates, grace periods) that are working as documented

## Audit status

This contract is **pre-1.0 and has not been independently audited**. It is
suitable for Testnet and for limited-value evaluation. Do not escrow mainnet
value you would be unhappy to lose until an audit is announced in the release
notes.

## Deployed addresses

Only trust contract ids published in this repository's release notes and
`README.md`. Any other "KeyLease" contract id is not ours.

## Supported versions

| Version | Supported |
| --- | --- |
| latest `main` | ✅ |
| latest tagged release | ✅ |
| older releases | ❌ (upgrade to the latest tag) |
