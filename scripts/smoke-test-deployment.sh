#!/usr/bin/env bash
#
# smoke-test-deployment.sh — opt-in post-deploy smoke test for KeyLease.
#
# Creates a one-call lease and settles it with zero or one verified call.
# This script is intentionally guarded: it refuses to run unless explicitly
# enabled, and it refuses mainnet unless explicitly allowed.
#
# Usage
#   KEYLEASE_SMOKE_TEST=1 ./scripts/smoke-test-deployment.sh [--env-file .env] [--dry-run]
#
# Required environment
#   KEYLEASE_SMOKE_TEST=1            Explicit opt-in.
#   KEYLEASE_CONTRACT_ID             Registry contract id (C...).
#   KEYLEASE_SERVICE_ID              Service id to lease.
#   KEYLEASE_TOKEN_ADDRESS           SEP-41/SAC token used for escrow.
#   CONSUMER_IDENTITY                stellar CLI identity for the consumer.
#   PROVIDER_IDENTITY                stellar CLI identity for the provider.
#
# Optional environment
#   KEYLEASE_NETWORK=testnet         Network name.
#   KEYLEASE_ALLOW_MAINNET=1         Required when KEYLEASE_NETWORK=mainnet.
#   KEYLEASE_SMOKE_VERIFIED_CALLS=1  0 or 1. Default: 1.
#   KEYLEASE_SMOKE_DURATION_SECS=120 Lease duration. Default: 120.
#   KEYLEASE_SMOKE_AMOUNT            Token amount to mint/approve is not handled;
#                                    consumer must already hold enough balance.
#
# Notes
#   - Prints transaction hashes/ids returned by stellar CLI.
#   - Settles the lease it creates. If settlement fails after creation, the
#     lease is short-lived and can be reclaimed after expiry + grace period.

set -euo pipefail

ENV_FILE=".env"
DRY_RUN=0

usage() { sed -n '2,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
need_value() { if [ $# -lt 2 ] || [ -z "${2:-}" ]; then die "$1 requires a value"; fi; }

while [ $# -gt 0 ]; do
  case "$1" in
    --env-file) need_value "$@"; ENV_FILE="$2"; shift ;;
    --env-file=*) ENV_FILE="${1#*=}" ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'error: unknown argument: %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

load_env_file() {
  local file="$1"
  [ -f "$file" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line#"${line%%[![:space:]]*}"}"
    [ -z "$line" ] && continue
    case "$line" in \#*) continue ;; esac
    case "$line" in *=*) ;; *) continue ;; esac
    local key="${line%%=*}"
    local value="${line#*=}"
    key="${key%"${key##*[![:space:]]}"}"
    value="${value#"${value%%[![:space:]]*}"}"
    case "$key" in
      KEYLEASE_*|SOURCE_ACCOUNT|NETWORK|CONSUMER_IDENTITY|PROVIDER_IDENTITY) ;;
      *) continue ;;
    esac
    if [ -z "${!key:-}" ]; then
      if [[ "$value" =~ ^\".*\"$ ]]; then value="${value:1:${#value}-2}";
      elif [[ "$value" =~ ^\'.*\'$ ]]; then value="${value:1:${#value}-2}"; fi
      export "$key=$value"
    fi
  done < "$file"
}

load_env_file "$ENV_FILE"

KEYLEASE_SMOKE_TEST="${KEYLEASE_SMOKE_TEST:-}"
KEYLEASE_CONTRACT_ID="${KEYLEASE_CONTRACT_ID:-}"
KEYLEASE_SERVICE_ID="${KEYLEASE_SERVICE_ID:-}"
KEYLEASE_TOKEN_ADDRESS="${KEYLEASE_TOKEN_ADDRESS:-}"
KEYLEASE_NETWORK="${KEYLEASE_NETWORK:-${NETWORK:-testnet}}"
KEYLEASE_ALLOW_MAINNET="${KEYLEASE_ALLOW_MAINNET:-}"
CONSUMER_IDENTITY="${CONSUMER_IDENTITY:-}"
PROVIDER_IDENTITY="${PROVIDER_IDENTITY:-}"
KEYLEASE_SMOKE_VERIFIED_CALLS="${KEYLEASE_SMOKE_VERIFIED_CALLS:-1}"
KEYLEASE_SMOKE_DURATION_SECS="${KEYLEASE_SMOKE_DURATION_SECS:-120}"

[ "$KEYLEASE_SMOKE_TEST" = "1" ] || die "KEYLEASE_SMOKE_TEST=1 is required; this script is opt-in"
if [ "$KEYLEASE_NETWORK" = "mainnet" ] && [ "$KEYLEASE_ALLOW_MAINNET" != "1" ]; then
  die "refusing KEYLEASE_NETWORK=mainnet without KEYLEASE_ALLOW_MAINNET=1"
fi

[ -n "$KEYLEASE_CONTRACT_ID" ] || die "KEYLEASE_CONTRACT_ID is required"
[ -n "$KEYLEASE_SERVICE_ID" ] || die "KEYLEASE_SERVICE_ID is required"
[ -n "$KEYLEASE_TOKEN_ADDRESS" ] || die "KEYLEASE_TOKEN_ADDRESS is required"
[ -n "$CONSUMER_IDENTITY" ] || die "CONSUMER_IDENTITY is required"
[ -n "$PROVIDER_IDENTITY" ] || die "PROVIDER_IDENTITY is required"

printf '%s' "$KEYLEASE_CONTRACT_ID" | grep -Eq '^C[A-Z0-9]{55}$' || die "invalid KEYLEASE_CONTRACT_ID: $KEYLEASE_CONTRACT_ID"
printf '%s' "$KEYLEASE_SERVICE_ID" | grep -Eq '^[0-9]+$' || die "KEYLEASE_SERVICE_ID must be numeric"
printf '%s' "$KEYLEASE_SMOKE_VERIFIED_CALLS" | grep -Eq '^[01]$' || die "KEYLEASE_SMOKE_VERIFIED_CALLS must be 0 or 1"
printf '%s' "$KEYLEASE_SMOKE_DURATION_SECS" | grep -Eq '^[0-9]+$' || die "KEYLEASE_SMOKE_DURATION_SECS must be numeric"

run_capture() {
  local placeholder="$1"; shift
  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] %s\n' "$*" >&2
    printf '%s' "$placeholder"
  else
    "$@"
  fi
}

run_send() {
  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

if [ "$DRY_RUN" != "1" ]; then
  command -v stellar >/dev/null 2>&1 || die "stellar CLI not found"
fi

CONSUMER_ADDR="$(run_capture '<consumer-address>' stellar keys address "$CONSUMER_IDENTITY")"
PROVIDER_ADDR="$(run_capture '<provider-address>' stellar keys address "$PROVIDER_IDENTITY")"

step "Smoke test configuration"
info "network: $KEYLEASE_NETWORK"
info "contract: $KEYLEASE_CONTRACT_ID"
info "service:  $KEYLEASE_SERVICE_ID"
info "token:    $KEYLEASE_TOKEN_ADDRESS"
info "consumer: $CONSUMER_ADDR ($CONSUMER_IDENTITY)"
info "provider: $PROVIDER_ADDR ($PROVIDER_IDENTITY)"
info "verified_calls: $KEYLEASE_SMOKE_VERIFIED_CALLS"
info "duration_secs:  $KEYLEASE_SMOKE_DURATION_SECS"

step "Creating one-call lease"
CREATE_OUT="$(run_capture '{"lease_id":"<lease-id>","tx":"<create-tx>"}' stellar contract invoke \
  --id "$KEYLEASE_CONTRACT_ID" \
  --source-account "$CONSUMER_IDENTITY" \
  --network "$KEYLEASE_NETWORK" \
  --send=yes \
  -- create_lease \
  --consumer "$CONSUMER_ADDR" \
  --service_id "$KEYLEASE_SERVICE_ID" \
  --requested_calls 1 \
  --duration_secs "$KEYLEASE_SMOKE_DURATION_SECS")"
printf '%s\n' "$CREATE_OUT"

LEASE_ID=""
if [ "$DRY_RUN" = "1" ]; then
  LEASE_ID="<lease-id>"
else
  LEASE_ID="$(printf '%s\n' "$CREATE_OUT" | tail -n1 | tr -dc '0-9')"
fi
[ -n "$LEASE_ID" ] || die "could not determine lease id from create_lease output"
info "lease id: $LEASE_ID"

step "Settling lease"
SETTLE_OUT="$(run_capture '{"tx":"<settle-tx>"}' stellar contract invoke \
  --id "$KEYLEASE_CONTRACT_ID" \
  --source-account "$PROVIDER_IDENTITY" \
  --network "$KEYLEASE_NETWORK" \
  --send=yes \
  -- settle_lease \
  --provider "$PROVIDER_ADDR" \
  --lease_id "$LEASE_ID" \
  --verified_calls "$KEYLEASE_SMOKE_VERIFIED_CALLS")"
printf '%s\n' "$SETTLE_OUT"

step "Smoke test complete"
info "lease created and settled"
info "cleanup: none required after settlement; if settlement had not completed, the short lease would expire and could be reclaimed after the grace window"
