# CLI reference

`@keylease/cli` is the developer CLI. It acquires a lease on `keylease-core`,
mints a signed session bearer token, and remembers the session locally so other
tooling can pick it up.

## Install

The `@keylease` scope is not published to npm yet, so install from source:

```bash
git clone https://github.com/KeyLease-Security/keylease-gateway
cd keylease-gateway
pnpm install
pnpm build

# run it
node packages/cli/dist/index.js --help
# or link the `keylease` bin onto your PATH
pnpm --filter @keylease/cli link --global
```

Requires Node.js `>= 20.11` and pnpm.

## Commands

| Command | Purpose |
| --- | --- |
| `keylease acquire --service <id> --calls <count> --duration <secs> --secret <key>` | Invokes `create_lease` on `keylease-core` and prints a signed session bearer token. |
| `keylease env --service <id> [--file <path>]` | Writes the temporary keys of an acquired lease into a local `.env`. |
| `keylease status --lease <id>` | Reads live lease state over Soroban RPC. |
| `keylease status --token <token>` | Verifies a session token offline (no RPC). |
| `keylease help`, `keylease --version` | Help and version. |

### `acquire`

```bash
keylease acquire --service 1 --calls 500 --duration 3600 --secret "$STELLAR_SECRET"
```

Escrows `rate_per_call × calls` on chain and returns a session token. The printed
output includes the lease id, consumer, call allocation and expiry:

```text
Lease acquired for service "1"
  lease id   : 7
  consumer   : G...
  calls      : 500
  expires at : 2026-10-09T18:00:00.000Z
  tx hash    : …

  Authorization: Bearer kls1.<payload>.<signature>
```

### `env`

```bash
keylease env --service 1            # writes ./.env
keylease env --service 1 --file .env.local
```

Merges seven keys into the target file, **replacing existing values and
preserving every other line**:

```text
KEYLEASE_SERVICE
KEYLEASE_LEASE_ID
KEYLEASE_CONSUMER
KEYLEASE_SESSION_TOKEN
KEYLEASE_CALLS_LIMIT
KEYLEASE_ISSUED_AT
KEYLEASE_EXPIRES_AT
```

### `status`

```bash
keylease status --lease 7                  # live on-chain state
keylease status --token "$SESSION_TOKEN"   # offline token verification
```

`--lease` reads the lease from Soroban RPC; `--token` verifies the token's
signature and validity window without touching the network. One of the two is
required.

## Flags

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--service <id>` | `acquire`, `env` | Service the lease is scoped to (required). |
| `--calls <count>` | `acquire` | Number of calls the lease authorises (positive integer). |
| `--duration <secs>` | `acquire` | Lease lifetime in seconds (positive integer). |
| `--secret <key>` | `acquire` | Consumer's Stellar secret seed (`S...`). |
| `--lease <id>` | `status` | Lease id to inspect on chain. |
| `--token <token>` | `status` | Session bearer token to verify offline. |
| `--file <path>` | `env` | Target env file (default `.env`). |
| `--network <name>` | `acquire`, `status` | `testnet` \| `mainnet` \| `local` (default `testnet`). |
| `--rpc <url>` | `acquire`, `status` | Soroban RPC endpoint override. |
| `--contract <id>` | `acquire`, `status` | `keylease-core` contract id override. |
| `--json` | all | Machine-readable JSON output. |
| `-h`, `--help` | all | Show help. |

`--secret` is the consumer account's **Stellar secret seed**. It signs both the
`create_lease` transaction and the session token, and it never leaves the
process — it is not printed, logged, or written to disk. Prefer passing it from
an environment variable or secret manager rather than the shell history.
`KEYLEASE_NETWORK`, `KEYLEASE_RPC_URL` and `KEYLEASE_CONTRACT_ID` are read from
the environment when the corresponding flag is omitted.

## Session store

`acquire` remembers each lease in `.keylease/sessions.json` (relative to the
current directory, mode `0600`), keyed by service id, so `env` can find it later.
Add `.keylease/` to your `.gitignore`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Runtime failure — an RPC error, an invalid secret, a token that fails verification, or no stored session for the requested service. |
| `2` | Usage error — a missing/invalid option, an unknown command, or an unexpected positional argument. |

## Read next

- [Session tokens](session-tokens.md) — what `acquire` mints and `status --token` verifies.
- [Consumer guide](../consumer-guide.md) — the protocol-level view of the same flow.
