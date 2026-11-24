#!/usr/bin/env bash
#
# verify-deployment.sh — verify a deployed KeyLeaseRegistry against .env.
#
# Reads KEYLEASE_CONTRACT_ID (and optional network/source settings) from .env or
# the environment, then checks version, get_admin, get_token and the demo
# service. Prints PASS/FAIL per check and exits non-zero on any mismatch.
#
# Usage
#   ./scripts/verify-deployment.sh [--env-file .env] [--dry-run]
#
# Options
#   --env-file <path>   Env file to read. Default: .env
#   --dry-run           Validate inputs and print the checks that would run.
#   -h, --help          Show this help.
#
# Environment
#   KEYLEASE_CONTRACT_ID    Required. Registry contract id (C...).
#   KEYLEASE_NETWORK        Network name. Default: testnet
#   SOURCE_ACCOUNT          stellar CLI identity for read invocations. Default: keylease-admin
#   KEYLEASE_ADMIN          Expected admin address (optional check target).
#   KEYLEASE_TOKEN_ADDRESS  Expected escrow token (optional check target).
#   KEYLEASE_SERVICE_ID     Service id to verify. Default: 1
#   KEYLEASE_EXPECTED_VERSION Expected version() output (optional).

set -euo pipefail

ENV_FILE=".env"
DRY_RUN=0

usage() { sed -n '2,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }
info() { printf '    %s\n' "$*"; }

need_value() { [ $# -ge 2 ] && [ -n "$2" ] || die "$1 requires a value"; }

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
    case "$line" in
      *=*) ;;
      *) continue ;;
    esac
    local key="${line%%=*}"
    local value="${line#*=}"
    key="${key%"${key##*[![:space:]]}"}"
    value="${value#"${value%%[![:space:]]*}"}"
    case "$key" in
      KEYLEASE_*|SOURCE_ACCOUNT|NETWORK) ;;
      *) continue ;;
    esac
    if [ -z "${!key:-}" ]; then
      if [[ "$value" =~ ^\".*\"$ ]]; then
        value="${value:1:${#value}-2}"
      elif [[ "$value" =~ ^\'.*\'$ ]]; then
        value="${value:1:${#value}-2}"
      fi
      export "$key=$value"
    fi
  done < "$file"
}

load_env_file "$ENV_FILE"

KEYLEASE_CONTRACT_ID="${KEYLEASE_CONTRACT_ID:-}"
KEYLEASE_NETWORK="${KEYLEASE_NETWORK:-${NETWORK:-testnet}}"
SOURCE_ACCOUNT="${SOURCE_ACCOUNT:-keylease-admin}"
KEYLEASE_SERVICE_ID="${KEYLEASE_SERVICE_ID:-1}"
KEYLEASE_ADMIN="${KEYLEASE_ADMIN:-}"
KEYLEASE_TOKEN_ADDRESS="${KEYLEASE_TOKEN_ADDRESS:-}"
KEYLEASE_EXPECTED_VERSION="${KEYLEASE_EXPECTED_VERSION:-}"

[ -n "$KEYLEASE_CONTRACT_ID" ] || die "KEYLEASE_CONTRACT_ID is required (set it in $ENV_FILE or the environment)"
printf '%s' "$KEYLEASE_CONTRACT_ID" | grep -Eq '^C[A-Z0-9]{55}$' \
  || die "KEYLEASE_CONTRACT_ID must be a contract id starting with C, got: $KEYLEASE_CONTRACT_ID"
printf '%s' "$KEYLEASE_SERVICE_ID" | grep -Eq '^[0-9]+$' \
  || die "KEYLEASE_SERVICE_ID must be a whole number, got: $KEYLEASE_SERVICE_ID"

if [ "$DRY_RUN" = "1" ]; then
  cat <<EOF
dry run: no network calls will be made
  env file: $ENV_FILE
  contract: $KEYLEASE_CONTRACT_ID
  network:  $KEYLEASE_NETWORK
  source:   $SOURCE_ACCOUNT
  checks:
    - version
    - get_admin${KEYLEASE_ADMIN:+ (expect $KEYLEASE_ADMIN)}
    - get_token${KEYLEASE_TOKEN_ADDRESS:+ (expect $KEYLEASE_TOKEN_ADDRESS)}
    - get_service --service_id $KEYLEASE_SERVICE_ID
EOF
  exit 0
fi

command -v stellar >/dev/null 2>&1 || die "stellar CLI not found. See https://developers.stellar.org/docs/tools/cli/stellar-cli"

unquote() { tail -n1 | tr -d '"[:space:]'; }

FAILURES=0
PASSES=0

check() {
  local name="$1"
  shift
  local output
  if output="$("$@" 2>&1)"; then
    printf 'PASS %s\n' "$name"
    printf '%s\n' "$output" | sed 's/^/  /'
    PASSES=$((PASSES + 1))
    return 0
  else
    printf 'FAIL %s\n' "$name"
    printf '%s\n' "$output" | sed 's/^/  /'
    FAILURES=$((FAILURES + 1))
    return 1
  fi
}

invoke_read() {
  stellar contract invoke \
    --id "$KEYLEASE_CONTRACT_ID" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$KEYLEASE_NETWORK" \
    -- "$@"
}

check_eq() {
  local name="$1"
  local expected="$2"
  shift 2
  local output
  if ! output="$("$@" 2>&1)"; then
    printf 'FAIL %s\n' "$name"
    printf '%s\n' "$output" | sed 's/^/  /'
    FAILURES=$((FAILURES + 1))
    return 1
  fi
  local actual
  actual="$(printf '%s\n' "$output" | unquote)"
  if [ -n "$expected" ] && [ "$actual" != "$expected" ]; then
    printf 'FAIL %s\n' "$name"
    printf '  expected: %s\n  actual:   %s\n' "$expected" "$actual"
    FAILURES=$((FAILURES + 1))
    return 1
  fi
  printf 'PASS %s\n' "$name"
  printf '  %s\n' "$actual"
  PASSES=$((PASSES + 1))
}

printf 'Verifying KeyLease deployment\n'
info "contract: $KEYLEASE_CONTRACT_ID"
info "network:  $KEYLEASE_NETWORK"
info "source:   $SOURCE_ACCOUNT"

check_eq "version" "$KEYLEASE_EXPECTED_VERSION" invoke_read version || true
check_eq "get_admin" "$KEYLEASE_ADMIN" invoke_read get_admin || true
check_eq "get_token" "$KEYLEASE_TOKEN_ADDRESS" invoke_read get_token || true
check "get_service(service_id=$KEYLEASE_SERVICE_ID)" invoke_read get_service --service_id "$KEYLEASE_SERVICE_ID" || true

printf '\nSummary: %d passed, %d failed\n' "$PASSES" "$FAILURES"
[ "$FAILURES" -eq 0 ]
