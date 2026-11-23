# Changelog

All notable changes to `keylease-core` are documented here. This file follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the workspace
version in [`Cargo.toml`](./Cargo.toml). Per-release notes live in
[`docs/releases/`](./docs/releases/) and are attached to GitHub releases by
`.github/workflows/release.yml`.

## [Unreleased]

### Added

- Time-locked Wasm upgrades: `propose_upgrade`, `execute_upgrade` and
  `cancel_upgrade`, with a 24-hour on-ledger delay and a `get_pending_upgrade`
  view. The `upgrade_proposed` and `contract_upgraded` events report the Wasm
  hash and execution ledger timestamp.
- Property test for the lease TTL policy over arbitrary expiry and ledger
  timestamps, asserting monotonicity, grace-window coverage and the documented
  maximum.
- Shared `setup_with_service` test helper that returns a funded registry with
  one active service while leaving ledger time explicit in every test.
- CI reproducibility check: the Wasm build job rebuilds the release Wasm twice
  from clean targets, compares hashes, and prints the registry hash for
  comparison with `docs/deployments.md`.

### Changed

- The instant `upgrade(admin, new_wasm_hash)` entry point is replaced by the
  propose/execute/cancel flow. Existing deployments must use the new flow after
  adopting this release.
- `contract_upgraded` now includes `execute_after` in its event payload.

## [0.2.1] - 2026-10-25

### Changed

- Lease storage TTL is term-aware: the entry is extended for the remaining
  lease term, the 24-hour settlement grace window and a one-day read buffer
  instead of a fixed thirty-day horizon.

[Full notes](./docs/releases/v0.2.1.md)

## [0.2.0] - 2026-10-08

### Added

- In-place Wasm upgrades, escrow events and the Stellar Asset Contract
  integration test.

[Full notes](./docs/releases/v0.2.0.md)

## [0.1.0] - 2026-10-07

### Added

- First release: service registration, leases, settlement and consumer
  reclamation.

[Full notes](./docs/releases/v0.1.0.md)

[Unreleased]: https://github.com/KeyLease-Security/keylease-core/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/KeyLease-Security/keylease-core/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/KeyLease-Security/keylease-core/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/KeyLease-Security/keylease-core/releases/tag/v0.1.0
