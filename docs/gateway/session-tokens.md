# Session tokens

A session token is the credential a developer sends to a KeyLease proxy. It
proves *which lease* the caller holds and that the caller controls the consumer
account that owns it. It is **not** a substitute for the on-chain lease, and it
cannot move funds.

## Format

```text
kls1.<base64url(payload JSON)>.<base64url(ed25519 signature)>
```

- `kls1` is the format prefix; the payload field `v` carries the version (`1`).
- The signature is produced by the consumer's Stellar ed25519 key — the same
  account that created the lease on chain.
- The signature covers the **transmitted payload segment verbatim**, so decoding
  is canonical by construction: there is no JSON key-order or float
  round-tripping to get wrong.

### Payload

| Field | Type | Meaning |
| --- | --- | --- |
| `v` | number | Token format version (`1`). |
| `lease_id` | string | The on-chain lease this session rides on. |
| `consumer` | string | Stellar public key (ed25519) that owns the lease. |
| `iat` | number | Issued-at, unix seconds. |
| `exp` | number | Expiry, unix seconds. |

## Verification

`verifySessionToken` performs all of these, in order, and throws
`TokenVerificationError` with a specific code on failure:

1. **Structure** — three dot-separated segments, a known prefix, base64url
   payload and a 64-byte ed25519 signature.
2. **Version** — `v` must be the supported version.
3. **Consumer key** — `consumer` must be a valid Stellar public key.
4. **Signature** — ed25519 verify over the exact payload segment.
5. **Binding** — when a proxy expects a specific lease or consumer, the token
   must match it.
6. **Window** — `exp` must be in the future; `iat` may be at most 60 seconds
   ahead of the verifier's clock (skew tolerance).

## Error codes

| Code | Raised when |
| --- | --- |
| `malformed_token` | Wrong segment count, bad prefix, non-base64url segment, bad signature length, non-JSON payload, or missing fields. |
| `unsupported_version` | `v` is not the supported version. |
| `invalid_consumer` | `consumer` is not a valid Stellar public key (also used when a secret seed cannot be parsed at mint time). |
| `bad_signature` | The ed25519 signature does not verify. |
| `consumer_mismatch` | The token's consumer does not match the expected consumer. |
| `lease_mismatch` | The token is not scoped to the requested lease. |
| `token_expired` | `now >= exp`. |
| `token_not_yet_valid` | `iat` is more than the allowed skew ahead of the verifier's clock. |

The proxy maps these onto HTTP responses: `token_expired` and
`token_not_yet_valid` become `401 token_expired`; every other code becomes
`401 invalid_token`. See [Proxy responses](proxy.md#responses).

## Offline verification

The CLI can validate a token without contacting the network — useful in CI or
when debugging a proxy rejection:

```bash
keylease status --token "$KEYLEASE_SESSION_TOKEN"
```

```text
Session token is valid
  lease id   : 7
  consumer   : G...
  expires at : 2026-10-09T18:00:00.000Z (2431s left)
```

## Threat notes

- **Forgery.** Without the consumer's secret seed an attacker cannot produce a
  valid signature; a bad signature is rejected offline, before any RPC call.
- **Replay to another lease.** The token is bound to a specific `lease_id`, and
  the proxy cross-checks `consumer` against the on-chain lease.
- **Stolen token.** A token is a bearer credential. It is bounded by `exp` and
  the lease's call quota, so the blast radius is the remaining calls on that
  lease — not a permanent credential. Keep proxies on TLS and prefer short
  `--duration` values.

## Read next

- [Proxy](proxy.md) — how tokens are enforced per request.
- [Contract reference](../contract-reference.md) — the on-chain lease they reference.
