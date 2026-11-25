# Attestation verification hardening

## Issue references

- Closes #23
- Closes #28
- Closes #27
- Closes #26

## Summary

This change completes the security boundary for optional metered-usage
attestations. A service that opts into attested settlement now requires a
valid Ed25519 signature from its configured attestor over the canonical,
domain-separated settlement message. Previously, the contract checked only the
attestation fields and accepted any 64-byte value as a signature.

## Security impact

Without cryptographic verification, an authorized provider could submit an
invented usage report while presenting a structurally valid attestation. That
defeated the purpose of opting a service into an independent attestor and left
consumers dependent on the provider's unsigned call count.

The verification now binds the lease id, service id, exact verified-call count,
and attestation expiry timestamp to the configured public key. Existing
freshness, service, lease, and call-count checks remain in place, and
verification happens before settlement state changes or token transfers.
Invalid signatures are rejected by Soroban's host Ed25519 primitive.

## Implementation details

### `contracts/registry/src/lib.rs`

- Corrected the attestation message documentation to state that the canonical
  bytes are signed directly and must not be pre-hashed.
- Reused `usage_attestation_message` to construct the exact signed message.
- Loaded the service's configured Ed25519 public key and called
  `env.crypto().ed25519_verify` with the public key, message, and signature.
- Kept verification ahead of `_settle_lease_body`, preserving the
  checks-before-effects-before-interactions ordering.

### Compatibility

- Services without an attestor continue to use `settle_lease` unchanged.
- Attested services continue to use `settle_lease_attested`; signatures are now
  actually enforced.
- No storage keys, contract type layouts, public entry-point names, or error
  discriminants were changed.

## Validation

Per the task instruction, no dependencies were installed and no tests were
executed. The change uses the existing Soroban SDK crypto API. CI should run
the complete repository checks, including formatting, clippy, host tests, Wasm
builds, rustdoc, shell checks, and security scans.

## Reviewer checklist

- [ ] Confirm invalid signatures abort before settlement.
- [ ] Confirm valid signatures cover the exact bytes returned by
      `usage_attestation_message`.
- [ ] Confirm stale, mismatched, and unconfigured attestations retain their
      existing error behavior.
- [ ] Confirm non-attested services retain the existing settlement path.
