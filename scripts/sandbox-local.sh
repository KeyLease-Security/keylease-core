#!/usr/bin/env bash
#
# sandbox-local.sh — stand up a throwaway KeyLease deployment on a local network.
#
# The unit suite proves the contract logic in a simulated ledger, and
# deploy-testnet.sh is the network reference. This script removes the manual
# steps from docs/developer-guide.md: it connects to (or starts) a local
# Stellar network, funds three identities, deploys the registry and a token,
# registers one demo service, and writes the values to .env.sandbox.
#
# What it does:
#   1. connects to the local network, or starts the stellar/quickstart container
#   2. creates and Friendbot-funds kl-sandbox-admin / -provider / -consumer
#   3. builds the registry and mock-token Wasm
#   4. deploys + initializes the registry, deploys + mints the mock token,
#      points the registry at it, and registers one demo service
#   5. writes .env.sandbox (gitignored, no secrets in it)
#
# Usage
#   ./scripts/sandbox-local.sh                # connect or start, deploy, write .env.sandbox
#   ./scripts/sandbox-local.sh --dry-run      # print every command; send nothing
#   ./scripts/sandbox-local.sh --connect      # fail instead of starting a container
#   ./scripts/sandbox-local.sh --force        # overwrite an existing .env.sandbox
#   ./scripts/sandbox-local.sh --teardown     # stop the network, remove identities and the env file
#
# Safety
#   The only network this script knows is `local`. There is no NETWORK variable:
#   every stellar command passes `--network local` literally, so it cannot
#   reach Testnet or Mainnet. Teardown stops the local container and removes
#   only the three kl-sandbox-* identities and .env.sandbox that this script
#   creates.
#
# Prerequisites
#   - Stellar CLI:               https://developers.stellar.org/docs/tools/cli/stellar-cli
#   - Rust 1.84+ with wasm32v1-none
#   - A container engine (Docker, Podman or Colima) when the script has to start
#     the network. Port 8000 must be free on localhost.
#
# The escrow token is contracts/mock_token, which has an unlimited mint. That is
# safe here precisely because the container's ledger is disposable.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ENV_FILE="$REPO_ROOT/.env.sandbox"
SANDBOX_LOG="$REPO_ROOT/.stellar/sandbox.log"
REGISTRY_WASM="$REPO_ROOT/target/wasm32v1-none/release/keylease_registry.wasm"
TOKEN_WASM="$REPO_ROOT/target/wasm32v1-none/release/keylease_mock_token.wasm"

ADMIN_ID="kl-sandbox-admin"
PROVIDER_ID="kl-sandbox-provider"
CONSUMER_ID="kl-sandbox-consumer"

RPC_URL="http://localhost:8000/rpc"
PASSPHRASE="Standalone Network ; February 2017"
RATE_PER_CALL=1000000
# 1000 tokens at 7 decimals, minted to the sandbox consumer.
CONSUMER_MINT=10000000000

DRY_RUN=0
CONNECT_ONLY=0
FORCE=0
MODE="up"

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --connect) CONNECT_ONLY=1 ;;
    --force)   FORCE=1 ;;
    --teardown) MODE="teardown" ;;
    -h|--help)
      awk 'NR > 1 { if ($0 ~ /^set -euo pipefail/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
      exit 0
      ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
info() { printf '    %s\n' "$*"; }
die()  { printf '\n\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# A state-changing or network-touching command. In --dry-run it is printed
# instead of executed; plan output goes to fd 3 so the caller's $(...) capture
# does not swallow it.
exec 3>&1

format_cmd() {
  local out="" arg
  for arg in "$@"; do
    if [ -z "$out" ]; then
      out="$(printf '%q' "$arg")"
    else
      out="$out $(printf '%q' "$arg")"
    fi
  done
  printf '%s' "$out"
}

plan() {
  printf '    [dry-run] %s\n' "$(format_cmd "$@")" >&3
}

run() {
  if [ "$DRY_RUN" = "1" ]; then
    plan "$@"
  else
    "$@"
  fi
}

# run_capture <placeholder> <command...>
run_capture() {
  local placeholder="$1"
  shift
  if [ "$DRY_RUN" = "1" ]; then
    plan "$@"
    printf '%s' "$placeholder"
  else
    "$@"
  fi
}

network_is_up() {
  stellar network health --network local >/dev/null 2>&1
}

ensure_network() {
  if [ "$DRY_RUN" = "1" ]; then
    plan stellar network health --network local
    [ "$CONNECT_ONLY" = "1" ] || plan stellar container start local
    return 0
  fi

  if network_is_up; then
    info "local network already reachable at $RPC_URL"
    return 0
  fi

  if [ "$CONNECT_ONLY" = "1" ]; then
    die "no local network at $RPC_URL and --connect was given.
     Start one in another terminal with: stellar container start local"
  fi

  command -v stellar >/dev/null 2>&1 \
    || die "stellar CLI not found. See https://developers.stellar.org/docs/tools/cli/stellar-cli"

  mkdir -p "$(dirname "$SANDBOX_LOG")"
  step "Starting the local network container"
  # `stellar container start` has no detach mode and runs the node in the
  # foreground, so it is backgrounded here with its output in a log.
  nohup stellar container start local >"$SANDBOX_LOG" 2>&1 &
  info "container starting in the background (log: ${SANDBOX_LOG#"$REPO_ROOT"/})"

  local attempt
  for (( attempt = 0; attempt < 60; attempt++ )); do
    if network_is_up; then
      info "local network is healthy at $RPC_URL"
      return 0
    fi
    sleep 2
  done
  die "the local network did not become healthy within 120s.
     See ${SANDBOX_LOG#"$REPO_ROOT"/} and try: stellar container start local"
}

ensure_identity() {
  local identity="$1"
  if [ "$DRY_RUN" = "1" ]; then
    plan stellar keys generate "$identity" --network local --fund
    return 0
  fi
  if stellar keys address "$identity" >/dev/null 2>&1; then
    info "reusing identity $identity"
    return 0
  fi
  run stellar keys generate "$identity" --network local --fund
  info "created and funded $identity"
}

# ---------------------------------------------------------------------------
# Teardown
# ---------------------------------------------------------------------------

if [ "$MODE" = "teardown" ]; then
  step "Tearing down the local sandbox"
  info "this only touches the local container, the kl-sandbox-* identities and .env.sandbox"

  if [ "$DRY_RUN" = "1" ]; then
    plan stellar container stop local
    plan stellar keys rm --force "$ADMIN_ID"
    plan stellar keys rm --force "$PROVIDER_ID"
    plan stellar keys rm --force "$CONSUMER_ID"
    plan rm -f "$ENV_FILE"
    printf '\ndry run: nothing was stopped or removed.\n'
    exit 0
  fi

  if stellar container stop local; then
    info "local network container stopped"
  else
    info "no local network container to stop"
  fi

  for identity in "$ADMIN_ID" "$PROVIDER_ID" "$CONSUMER_ID"; do
    if stellar keys address "$identity" >/dev/null 2>&1; then
      stellar keys rm --force "$identity"
      info "removed identity $identity"
    fi
  done

  if [ -f "$ENV_FILE" ]; then
    rm -f "$ENV_FILE"
    info "removed ${ENV_FILE#"$REPO_ROOT"/}"
  fi

  info "teardown complete; Testnet and Mainnet were never touched"
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

if [ "$DRY_RUN" != "1" ]; then
  command -v cargo   >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"
  command -v stellar >/dev/null 2>&1 || die "stellar CLI not found. See https://developers.stellar.org/docs/tools/cli/stellar-cli"

  if [ -f "$ENV_FILE" ] && [ "$FORCE" != "1" ]; then
    die "${ENV_FILE#"$REPO_ROOT"/} already exists.
     Pass --force to replace it, or run --teardown to remove it first."
  fi
fi

printf '\n============================================================================\n'
printf ' KeyLease local sandbox\n'
printf ' network : local (hardcoded — Testnet and Mainnet are never touched)\n'
printf ' rpc     : %s\n' "$RPC_URL"
MODE_NOTE=""
if [ "$DRY_RUN" = "1" ]; then
  MODE_NOTE=" (dry run — nothing is sent)"
fi
printf ' mode    : %s%s\n' "$MODE" "$MODE_NOTE"
printf '============================================================================\n'

ensure_network

# ---------------------------------------------------------------------------
# Identities
# ---------------------------------------------------------------------------

step "Ensuring three funded sandbox identities"
ensure_identity "$ADMIN_ID"
ensure_identity "$PROVIDER_ID"
ensure_identity "$CONSUMER_ID"

if [ "$DRY_RUN" = "1" ]; then
  ADMIN="<address of $ADMIN_ID>"
  PROVIDER="<address of $PROVIDER_ID>"
  CONSUMER="<address of $CONSUMER_ID>"
else
  ADMIN="$(stellar keys address "$ADMIN_ID")"
  PROVIDER="$(stellar keys address "$PROVIDER_ID")"
  CONSUMER="$(stellar keys address "$CONSUMER_ID")"
fi

step "Addresses"
info "admin    : $ADMIN"
info "provider : $PROVIDER"
info "consumer : $CONSUMER"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

step "Building Soroban Wasm (wasm32v1-none, release)"
if [ "$DRY_RUN" = "1" ]; then
  plan cargo build --target wasm32v1-none --release --all
else
  rustup target list --installed 2>/dev/null | grep -qx 'wasm32v1-none' \
    || rustup target add wasm32v1-none
  cargo build --target wasm32v1-none --release --all
  [ -f "$REGISTRY_WASM" ] || die "expected Wasm not found: ${REGISTRY_WASM#"$REPO_ROOT"/}"
  [ -f "$TOKEN_WASM" ]    || die "expected Wasm not found: ${TOKEN_WASM#"$REPO_ROOT"/}"
fi

# ---------------------------------------------------------------------------
# Registry
# ---------------------------------------------------------------------------

step "Deploying KeyLeaseRegistry"
REGISTRY="$(run_capture '<registry-contract-id>' stellar contract deploy \
  --wasm "$REGISTRY_WASM" \
  --source-account "$ADMIN_ID" \
  --network local \
  --alias keylease-sandbox-registry | tail -n1)"

if [ "$DRY_RUN" != "1" ]; then
  printf '%s' "$REGISTRY" | grep -Eq '^C[A-Z0-9]{55}$' \
    || die "unexpected deploy output, expected a contract id starting with C: $REGISTRY"
fi
info "registry: $REGISTRY"

step "Initializing registry (admin = $ADMIN)"
run stellar contract invoke --id "$REGISTRY" --source-account "$ADMIN_ID" \
  --network local --send=yes -- init --admin "$ADMIN"

# ---------------------------------------------------------------------------
# Escrow token (mock SEP-41, disposable ledger only)
# ---------------------------------------------------------------------------

step "Deploying the mock escrow token"
TOKEN="$(run_capture '<mock-token-contract-id>' stellar contract deploy \
  --wasm "$TOKEN_WASM" \
  --source-account "$ADMIN_ID" \
  --network local \
  --alias keylease-sandbox-token | tail -n1)"

if [ "$DRY_RUN" != "1" ]; then
  printf '%s' "$TOKEN" | grep -Eq '^C[A-Z0-9]{55}$' \
    || die "unexpected deploy output, expected a contract id starting with C: $TOKEN"
fi
info "token: $TOKEN"

step "Initializing the mock token and minting to the consumer"
run stellar contract invoke --id "$TOKEN" --source-account "$ADMIN_ID" \
  --network local --send=yes -- init --admin "$ADMIN"
run stellar contract invoke --id "$TOKEN" --source-account "$ADMIN_ID" \
  --network local --send=yes -- mint --to "$CONSUMER" --amount "$CONSUMER_MINT"

step "Pointing the registry at the mock token"
run stellar contract invoke --id "$REGISTRY" --source-account "$ADMIN_ID" \
  --network local --send=yes -- set_token --admin "$ADMIN" --token "$TOKEN"

# ---------------------------------------------------------------------------
# Demo service
# ---------------------------------------------------------------------------

if command -v sha256sum >/dev/null 2>&1; then
  ENDPOINT_HASH="$(printf '%s' "${PROVIDER}:local-sandbox" | sha256sum | awk '{print $1}')"
else
  ENDPOINT_HASH="$(printf '%s' "${PROVIDER}:local-sandbox" | shasum -a 256 | awk '{print $1}')"
fi

step "Registering a demo service (provider = $PROVIDER)"
SERVICE_ID="$(run_capture '<service-id>' stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$PROVIDER_ID" \
  --network local \
  --send=yes \
  -- register_service \
     --provider "$PROVIDER" \
     --rate_per_call "$RATE_PER_CALL" \
     --endpoint_hash "$ENDPOINT_HASH" | tail -n1)"

if [ "$DRY_RUN" != "1" ]; then
  printf '%s' "$SERVICE_ID" | grep -Eq '^[0-9]+$' \
    || die "unexpected register_service output, expected a u64 service id: $SERVICE_ID"
fi
info "service id: $SERVICE_ID"

# ---------------------------------------------------------------------------
# Write .env.sandbox
# ---------------------------------------------------------------------------

step "Writing ${ENV_FILE#"$REPO_ROOT"/}"
if [ "$DRY_RUN" = "1" ]; then
  plan env_file "$ENV_FILE"
  cat <<EOF

============================================================================
 Dry run — nothing was started, built, signed or sent
============================================================================
 network      : local
 rpc          : $RPC_URL
 registry     : $REGISTRY
 token        : $TOKEN
 service id   : $SERVICE_ID
 admin        : $ADMIN
 provider     : $PROVIDER
 consumer     : $CONSUMER
 rate / call  : $RATE_PER_CALL
============================================================================
 Re-run without --dry-run to create the sandbox and write
 ${ENV_FILE#"$REPO_ROOT"/}.
============================================================================
EOF
  exit 0
fi

(
  umask 077
  cat >"$ENV_FILE" <<EOF
# ---------------------------------------------------------------------------
# Generated by scripts/sandbox-local.sh — local Stellar network sandbox.
# Do not commit this file; it is gitignored and disposable with:
#   ./scripts/sandbox-local.sh --teardown
# No secret seed is written here: every identity lives in the stellar CLI
# keystore, and every value below is public on the local ledger.
# ---------------------------------------------------------------------------
KEYLEASE_NETWORK=local
KEYLEASE_RPC_URL=$RPC_URL
KEYLEASE_NETWORK_PASSPHRASE="$PASSPHRASE"
KEYLEASE_CONTRACT_ID=$REGISTRY
KEYLEASE_SERVICE_ID=$SERVICE_ID
KEYLEASE_TOKEN_ADDRESS=$TOKEN
KEYLEASE_ADMIN=$ADMIN
KEYLEASE_PROVIDER=$PROVIDER
KEYLEASE_CONSUMER=$CONSUMER
KEYLEASE_RATE_PER_CALL=$RATE_PER_CALL
KEYLEASE_ENDPOINT_HASH=$ENDPOINT_HASH
EOF
)

info "wrote ${ENV_FILE#"$REPO_ROOT"/} (mode 600)"

cat <<EOF

============================================================================
 KeyLease local sandbox is up
============================================================================
 KEYLEASE_CONTRACT_ID=$REGISTRY
 KEYLEASE_SERVICE_ID=$SERVICE_ID
 KEYLEASE_TOKEN_ADDRESS=$TOKEN
 KEYLEASE_RPC_URL=$RPC_URL
 KEYLEASE_ADMIN=$ADMIN
 KEYLEASE_PROVIDER=$PROVIDER
 KEYLEASE_CONSUMER=$CONSUMER
============================================================================
 Next steps
   - Load the values:
       set -a; . ./.env.sandbox; set +a
   - Create a lease as the consumer (the mock token was minted to it):
       stellar contract invoke --id $REGISTRY --source-account $CONSUMER_ID \\
         --network local --send=yes -- create_lease \\
           --consumer $CONSUMER --service_id $SERVICE_ID \\
           --requested_calls 100 --duration_secs 120
   - Read the lifecycle:
       stellar events --id $REGISTRY --network local -c 50
   - Tear it all down:
       ./scripts/sandbox-local.sh --teardown
============================================================================
EOF
