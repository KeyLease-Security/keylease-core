#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  examples/consumer/keylease-consumer.sh create [--env FILE]
  examples/consumer/keylease-consumer.sh status --lease-id ID [--env FILE]
  examples/consumer/keylease-consumer.sh revoke --lease-id ID [--env FILE]
EOF
}

ENV_FILE=".env"
COMMAND="${1:-}"
if [ -z "$COMMAND" ]; then
  usage >&2
  exit 2
fi
shift

while [ $# -gt 0 ]; do
  case "$1" in
    --env) [ $# -ge 2 ] || { echo "error: --env requires a value" >&2; exit 2; }; ENV_FILE="$2"; shift 2 ;;
    --lease-id) [ $# -ge 2 ] || { echo "error: --lease-id requires a value" >&2; exit 2; }; LEASE_ID="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$COMMAND" in create|status|revoke) ;; *) usage >&2; exit 2 ;; esac
[ -f "$ENV_FILE" ] || { echo "error: env file not found: $ENV_FILE" >&2; exit 1; }
set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a

required() {
  local name="$1"
  local value
  value="$(printenv "$name" || true)"
  [ -n "$value" ] || { echo "error: $name is required in $ENV_FILE" >&2; exit 1; }
}

required KEYLEASE_CONTRACT_ID
required KEYLEASE_SERVICE_ID
required KEYLEASE_NETWORK
required KEYLEASE_CONSUMER_IDENTITY

[[ "$KEYLEASE_CONTRACT_ID" =~ ^C[A-Z2-7]{55}$ ]] || { echo "error: KEYLEASE_CONTRACT_ID must be a 56-character contract id starting with C" >&2; exit 1; }
[[ "$KEYLEASE_SERVICE_ID" =~ ^[1-9][0-9]*$ ]] || { echo "error: KEYLEASE_SERVICE_ID must be a positive integer" >&2; exit 1; }
[[ "$KEYLEASE_NETWORK" != "mainnet" ]] || echo "warning: this command targets mainnet" >&2

stellar contract invoke --id "$KEYLEASE_CONTRACT_ID" \
  --source-account "$KEYLEASE_CONSUMER_IDENTITY" --network "$KEYLEASE_NETWORK" -- \
  get_service --service_id "$KEYLEASE_SERVICE_ID" >/dev/null

case "$COMMAND" in
  create)
    required KEYLEASE_REQUESTED_CALLS
    required KEYLEASE_DURATION_SECS
    [[ "$KEYLEASE_REQUESTED_CALLS" =~ ^[1-9][0-9]*$ ]] || { echo "error: KEYLEASE_REQUESTED_CALLS must be a positive integer" >&2; exit 1; }
    [[ "$KEYLEASE_DURATION_SECS" =~ ^[1-9][0-9]*$ ]] || { echo "error: KEYLEASE_DURATION_SECS must be a positive integer" >&2; exit 1; }
    CONSUMER="$(stellar keys address "$KEYLEASE_CONSUMER_IDENTITY")"
    OUTPUT="$(stellar contract invoke --id "$KEYLEASE_CONTRACT_ID" \
      --source-account "$KEYLEASE_CONSUMER_IDENTITY" --network "$KEYLEASE_NETWORK" --send=yes -- \
      create_lease --consumer "$CONSUMER" --service_id "$KEYLEASE_SERVICE_ID" \
      --requested_calls "$KEYLEASE_REQUESTED_CALLS" --duration_secs "$KEYLEASE_DURATION_SECS")"
    LEASE_ID="$(printf '%s\n' "$OUTPUT" | tr -d '"[:space:]' | tail -n1)"
    [[ "$LEASE_ID" =~ ^[1-9][0-9]*$ ]] || { echo "error: create_lease did not return a positive lease id" >&2; exit 1; }
    LEASE="$(stellar contract invoke --id "$KEYLEASE_CONTRACT_ID" \
      --source-account "$KEYLEASE_CONSUMER_IDENTITY" --network "$KEYLEASE_NETWORK" -- \
      get_lease --lease_id "$LEASE_ID")"
    printf 'lease_id=%s\nlease_expiry=%s\n' "$LEASE_ID" "$(printf '%s\n' "$LEASE" | sed -n 's/.*expires_at[^0-9]*\([0-9][0-9]*\).*/\1/p' | head -n1)"
    ;;
  status)
    [ -n "${LEASE_ID:-}" ] || { echo "error: --lease-id is required" >&2; exit 2; }
    [[ "$LEASE_ID" =~ ^[1-9][0-9]*$ ]] || { echo "error: lease id must be a positive integer" >&2; exit 1; }
    stellar contract invoke --id "$KEYLEASE_CONTRACT_ID" \
      --source-account "$KEYLEASE_CONSUMER_IDENTITY" --network "$KEYLEASE_NETWORK" -- \
      get_lease --lease_id "$LEASE_ID"
    ;;
  revoke)
    [ -n "${LEASE_ID:-}" ] || { echo "error: --lease-id is required" >&2; exit 2; }
    [[ "$LEASE_ID" =~ ^[1-9][0-9]*$ ]] || { echo "error: lease id must be a positive integer" >&2; exit 1; }
    CONSUMER="$(stellar keys address "$KEYLEASE_CONSUMER_IDENTITY")"
    stellar contract invoke --id "$KEYLEASE_CONTRACT_ID" \
      --source-account "$KEYLEASE_CONSUMER_IDENTITY" --network "$KEYLEASE_NETWORK" --send=yes -- \
      revoke_expired_lease --consumer "$CONSUMER" --lease_id "$LEASE_ID"
    ;;
esac
