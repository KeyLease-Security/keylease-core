# Proxy

`@keylease/proxy` is a lease-verifying reverse proxy built on Fastify. It sits in
front of a protected upstream API and admits **only** requests backed by a valid,
unexpired, non-exhausted lease. The upstream never sees an unverified request.

It never needs a Stellar **secret key** — it only reads. Compromising the proxy
does not compromise escrowed funds.

## Run it

```bash
export KEYLEASE_UPSTREAM_URL=https://api.example.com
export KEYLEASE_CONTRACT_ID=C...
pnpm --filter @keylease/proxy start
```

Then call through it:

```bash
curl -H "Authorization: Bearer $KEYLEASE_SESSION_TOKEN" \
  http://localhost:8080/v1/weather
```

## Configuration

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `KEYLEASE_UPSTREAM_URL` | yes | – | Protected API base URL (path prefixes are preserved). |
| `KEYLEASE_CONTRACT_ID` | yes | – | `keylease-core` contract id. |
| `KEYLEASE_NETWORK` | no | `testnet` | `testnet` \| `mainnet` \| `local`. |
| `KEYLEASE_RPC_URL` | no | network default | Soroban RPC endpoint override. |
| `KEYLEASE_CACHE_TTL_MS` | no | `30000` | Lease-state cache TTL. |
| `KEYLEASE_FORWARD_AUTH` | no | `false` | Forward the caller's bearer token upstream. |
| `PORT` / `HOST` | no | `8080` / `0.0.0.0` | Listen address. |

## Verification pipeline

Every request passes through the same decision function:

1. **Extract** the `Authorization: Bearer <token>` header. A missing or
   non-bearer header is rejected before any network call.
2. **Verify the signature offline** — structure, version, ed25519 signature and
   the `iat`/`exp` window. Forged or expired tokens never reach Soroban RPC.
   See [Session tokens](session-tokens.md).
3. **Resolve the lease on chain.** The lease state is read over Soroban RPC and
   cached (30 s by default), so a hot path makes far fewer RPC calls than it
   handles requests. A missing lease is `403 lease_not_found`; an unreachable RPC
   fails **closed** with `503 verification_unavailable`.
4. **Bind the token to the lease.** The token's `consumer` must equal the lease's
   on-chain `consumer` (`403 consumer_mismatch`), the lease must be active
   (`403 lease_inactive`), and it must not be past `expires_at`
   (`403 lease_expired`).
5. **Enforce the quota locally.** The effective count is
   `max(on-chain calls_used, local count)`; once it reaches `calls_limit` the
   request is refused with `403 quota_exhausted`. Allowed requests increment the
   in-memory counter.
6. **Forward** the request to the upstream, stripping hop-by-hop headers and (by
   default) the caller's `Authorization`, and adding the KeyLease headers below.

## Headers

Requests that pass verification are forwarded with:

| Header | Value |
| --- | --- |
| `x-keylease-lease-id` | The lease backing the request. |
| `x-keylease-calls-remaining` | Calls still available after this request. |
| `x-forwarded-for`, `x-forwarded-host`, `x-forwarded-proto` | Standard forwarding metadata. |

The upstream response is relayed back and also carries `x-keylease-lease-id` and
`x-keylease-calls-remaining`, so a client can watch its budget without a
separate call.

`KEYLEASE_FORWARD_AUTH=true` forwards the caller's original bearer token
upstream; leave it off unless the upstream needs the raw credential.

## Responses

Denials are JSON `{ "error": "<reason>", "message": "..." }`.

| Status | `error` | Meaning |
| --- | --- | --- |
| `401` | `missing_token` | No `Authorization: Bearer` header. |
| `401` | `invalid_token` | Header is malformed, or the token is forged or structurally invalid. |
| `401` | `token_expired` | The session token's validity window has passed. |
| `403` | `lease_not_found` | The lease id in the token does not exist on chain. |
| `403` | `lease_inactive` | The lease has been revoked. |
| `403` | `lease_expired` | The on-chain lease has expired. |
| `403` | `consumer_mismatch` | The token's consumer does not own the lease. |
| `403` | `quota_exhausted` | The lease's call limit has been spent. |
| `503` | `verification_unavailable` | Soroban RPC could not be reached (fail closed). |
| `502` | `bad_gateway` | The upstream was unreachable — only after the lease passed. |
| `504` | `gateway_timeout` | The upstream did not respond within the request timeout. |

## Scaling caveat

The per-lease call counter lives in the proxy instance's memory. Running two
instances means each allows up to `allocated_calls`, but on-chain settlement is
still bounded by `allocated_calls × rate_per_call` — so the worst case is a
provider under-charging, never a consumer being over-charged. For a hard quota,
run one instance or move the counter to shared storage (e.g. Redis).

For failure modes when the proxy, RPC or provider is down, see
[Hosting topology](../hosting-topology.md#liveness-and-failure-modes).

## Read next

- [Session tokens](session-tokens.md) — the credential the proxy verifies.
- [Provider guide](../provider-guide.md) — running the proxy for your own API.
