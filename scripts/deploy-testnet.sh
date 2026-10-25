#!/usr/bin/env bash
#
# deploy-testnet.sh — deploy KeyLease to Stellar Testnet in dependency order.
#
# This script is deliberately sequential: each step depends on the previous one,
# and it prints the exact values a Stellar client or indexer needs afterwards.
#
# Deploy:
#   1. cargo build --target wasm32v1-none --release --all
#   2. stellar contract deploy   (registry)  -> REGISTRY contract id
#   3. stellar contract invoke -- init         --admin <ADMIN>
#   4. stellar contract invoke -- set_token    --admin <ADMIN> --token <TOKEN>
#   5. stellar contract invoke -- register_service --provider <P> --rate_per_call <R> --endpoint_hash <H>
#
# Upgrade an existing deployment in place:
#   1. cargo build --target wasm32v1-none --release --all
#   2. stellar contract upload   (registry)  -> Wasm hash
#   3. stellar contract invoke -- upgrade --admin <ADMIN> --new_wasm_hash <HASH>
#   4. stellar contract invoke -- version  (verify)
#
# Prerequisites
#   - Rust 1.84+ with `rustup target add wasm32v1-none`
#   - Stellar CLI:  https://developers.stellar.org/docs/tools/cli/stellar-cli
#   - A funded Testnet identity (see KEYLEASE_GENERATE_KEY=1 below)
#
# Usage
#   SOURCE_ACCOUNT=keylease-admin ./scripts/deploy-testnet.sh
#
#   KEYLEASE_GENERATE_KEY=1 SOURCE_ACCOUNT=keylease-admin ./scripts/deploy-testnet.sh
#       -> creates + Friendbot-funds the identity first (testnet only)
#
#   REGISTRY=C... SOURCE_ACCOUNT=keylease-admin ./scripts/deploy-testnet.sh --upgrade
#       -> uploads the built Wasm and installs it over an existing contract id
#
# Environment
#   SOURCE_ACCOUNT          stellar CLI identity/key used to sign. Default: keylease-admin
#   NETWORK                 stellar CLI network name.           Default: testnet
#   REGISTRY_ALIAS          stellar contract alias to save.      Default: keylease-registry
#   ADMIN                   Registry admin address.             Default: address of SOURCE_ACCOUNT
#   PROVIDER                Service provider address.           Default: ADMIN
#   TOKEN                   SEP-41 / SAC token address.         Default: native SAC on NETWORK
#   RATE_PER_CALL           Price per call in token stroops.    Default: 1000000 (0.1 XLM @ 7dp)
#   ENDPOINT_HASH           64 hex chars (32 bytes).            Default: sha256("<PROVIDER>:<SERVICE_NAME>")
#   SERVICE_NAME            Only used to derive the default ENDPOINT_HASH. Default: keylease-demo
#   KEYLEASE_GENERATE_KEY   Set to 1 to generate + fund SOURCE_ACCOUNT. Default: unset
#
# The script never deploys contracts/mock_token. That token has an unlimited
# mint and is for tests and local sandboxes only.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

SOURCE_ACCOUNT="${SOURCE_ACCOUNT:-keylease-admin}"
NETWORK="${NETWORK:-testnet}"
REGISTRY_ALIAS="${REGISTRY_ALIAS:-keylease-registry}"
RATE_PER_CALL="${RATE_PER_CALL:-1000000}"
SERVICE_NAME="${SERVICE_NAME:-keylease-demo}"
KEYLEASE_GENERATE_KEY="${KEYLEASE_GENERATE_KEY:-}"

MODE="deploy"
while [ $# -gt 0 ]; do
  case "$1" in
    --upgrade) MODE="upgrade" ;;
    --deploy)  MODE="deploy" ;;
    -h|--help)
      sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

REGISTRY_WASM="$REPO_ROOT/target/wasm32v1-none/release/keylease_registry.wasm"

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
info() { printf '    %s\n' "$*"; }
die()  { printf '\n\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

build_wasm() {
  rustup target list --installed 2>/dev/null | grep -qx 'wasm32v1-none' \
    || rustup target add wasm32v1-none
  cargo build --target wasm32v1-none --release --package keylease-registry
  [ -f "$REGISTRY_WASM" ] || die "expected Wasm not found: $REGISTRY_WASM"
}

command -v cargo   >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"
command -v stellar >/dev/null 2>&1 || die "stellar CLI not found. See https://developers.stellar.org/docs/tools/cli/stellar-cli"

# ---------------------------------------------------------------------------
# 0. Signing identity
# ---------------------------------------------------------------------------

if [ "$KEYLEASE_GENERATE_KEY" = "1" ]; then
  step "Generating and funding a Testnet identity: $SOURCE_ACCOUNT"
  stellar keys generate "$SOURCE_ACCOUNT" --network "$NETWORK" --fund
fi

if ! stellar keys address "$SOURCE_ACCOUNT" >/dev/null 2>&1; then
  die "identity '$SOURCE_ACCOUNT' not found. Create one with:
       stellar keys generate $SOURCE_ACCOUNT --network $NETWORK --fund
     or set KEYLEASE_GENERATE_KEY=1 (testnet only)."
fi

ADMIN="${ADMIN:-$(stellar keys address "$SOURCE_ACCOUNT")}"
PROVIDER="${PROVIDER:-$ADMIN}"

# ---------------------------------------------------------------------------
# Upgrade mode: build -> upload -> upgrade an existing contract id
# ---------------------------------------------------------------------------

if [ "$MODE" = "upgrade" ]; then
  [ -n "${REGISTRY:-}" ] || die "REGISTRY=<contract id> is required with --upgrade"
  printf '%s' "$REGISTRY" | grep -Eq '^C[A-Z0-9]{55}$' \
    || die "REGISTRY must be a contract id starting with C, got: $REGISTRY"

  step "Building Soroban Wasm (wasm32v1-none, release)"
  build_wasm

  step "Uploading registry Wasm"
  WASM_HASH="$(stellar contract upload \
    --wasm "$REGISTRY_WASM" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" | tail -n1)"

  printf '%s' "$WASM_HASH" | grep -Eq '^[0-9a-fA-F]{64}$' \
    || die "unexpected upload output, expected a 64-char Wasm hash: $WASM_HASH"
  info "wasm hash: $WASM_HASH"

  step "Upgrading $REGISTRY (admin = $ADMIN)"
  stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    --send=yes \
    -- upgrade --admin "$ADMIN" --new_wasm_hash "$WASM_HASH"

  step "Verifying on-chain version"
  VERSION="$(stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    -- version | tail -n1)"

  cat <<EOF

============================================================================
 KeyLeaseRegistry upgraded on $NETWORK
============================================================================
 KEYLEASE_CONTRACT_ID=$REGISTRY
 KEYLEASE_NETWORK=$NETWORK
 version()            = $VERSION
 wasm hash            = $WASM_HASH
============================================================================
 Next steps
   - Watch the upgrade event:
       stellar events --id $REGISTRY --network $NETWORK --start-ledger <ledger>
   - Verify storage and balances are intact:
       stellar contract invoke --id $REGISTRY --source-account $SOURCE_ACCOUNT \
         --network $NETWORK -- get_service --service_id 1
   - Explorer: https://stellar.expert/explorer/$NETWORK/contract/$REGISTRY
============================================================================
EOF
  exit 0
fi

if [ -z "${TOKEN:-}" ]; then
  TOKEN="$(stellar contract id asset --network "$NETWORK" --asset native)"
fi

if [ -z "${ENDPOINT_HASH:-}" ]; then
  if command -v sha256sum >/dev/null 2>&1; then
    ENDPOINT_HASH="$(printf '%s' "${PROVIDER}:${SERVICE_NAME}" | sha256sum | awk '{print $1}')"
  else
    ENDPOINT_HASH="$(printf '%s' "${PROVIDER}:${SERVICE_NAME}" | shasum -a 256 | awk '{print $1}')"
  fi
fi

if ! printf '%s' "$ENDPOINT_HASH" | grep -Eq '^[0-9a-fA-F]{64}$'; then
  die "ENDPOINT_HASH must be exactly 64 hex characters (32 bytes), got: $ENDPOINT_HASH"
fi
ENDPOINT_HASH="$(printf '%s' "$ENDPOINT_HASH" | tr 'A-F' 'a-f')"

info "network        : $NETWORK"
info "signer         : $SOURCE_ACCOUNT"
info "admin          : $ADMIN"
info "provider       : $PROVIDER"
info "escrow token   : $TOKEN"
info "rate / call    : $RATE_PER_CALL"
info "endpoint hash  : $ENDPOINT_HASH"

# ---------------------------------------------------------------------------
# 1. Build the registry Wasm
# ---------------------------------------------------------------------------

step "Building Soroban Wasm (wasm32v1-none, release)"
build_wasm

# ---------------------------------------------------------------------------
# 2. Deploy the registry
# ---------------------------------------------------------------------------

step "Deploying KeyLeaseRegistry"
REGISTRY="$(stellar contract deploy \
  --wasm "$REGISTRY_WASM" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --alias "$REGISTRY_ALIAS" | tail -n1)"

printf '%s' "$REGISTRY" | grep -Eq '^C[A-Z0-9]{55}$' \
  || die "unexpected deploy output, expected a contract id starting with C: $REGISTRY"
info "registry contract id: $REGISTRY"

# ---------------------------------------------------------------------------
# 3. Initialize (once)
# ---------------------------------------------------------------------------

step "Initializing registry (admin = $ADMIN)"
stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --send=yes \
  -- init --admin "$ADMIN"

# ---------------------------------------------------------------------------
# 4. Configure the escrow token
# ---------------------------------------------------------------------------

step "Configuring escrow token ($TOKEN)"
stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --send=yes \
  -- set_token --admin "$ADMIN" --token "$TOKEN"

# ---------------------------------------------------------------------------
# 5. Publish a service
# ---------------------------------------------------------------------------

step "Registering service (provider = $PROVIDER)"
SERVICE_ID="$(stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --send=yes \
  -- register_service \
     --provider "$PROVIDER" \
     --rate_per_call "$RATE_PER_CALL" \
     --endpoint_hash "$ENDPOINT_HASH" | tail -n1)"

printf '%s' "$SERVICE_ID" | grep -Eq '^[0-9]+$' \
  || die "unexpected register_service output, expected a u64 service id: $SERVICE_ID"

# Verify the write actually landed on-chain.
stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  -- get_service --service_id "$SERVICE_ID" >/dev/null

# ---------------------------------------------------------------------------
# Done — configuration for Stellar clients, indexers and explorers
# ---------------------------------------------------------------------------

cat <<EOF

============================================================================
 KeyLease deployed to $NETWORK
============================================================================
 KEYLEASE_CONTRACT_ID=$REGISTRY
 KEYLEASE_SERVICE_ID=$SERVICE_ID
 KEYLEASE_TOKEN_ADDRESS=$TOKEN
 KEYLEASE_NETWORK=$NETWORK
 KEYLEASE_RPC_URL=$( [ "$NETWORK" = "testnet" ] && echo "https://soroban-testnet.stellar.org" || echo "<your ${NETWORK} RPC url>" )
 KEYLEASE_ADMIN=$ADMIN
 KEYLEASE_PROVIDER=$PROVIDER
 KEYLEASE_RATE_PER_CALL=$RATE_PER_CALL
 KEYLEASE_ENDPOINT_HASH=$ENDPOINT_HASH
============================================================================
 Next steps
   - Copy the values above into your .env (see .env.example).
   - Confirm the on-chain metadata and configuration:
       stellar contract invoke --id $REGISTRY --source-account $SOURCE_ACCOUNT \
         --network $NETWORK -- version
       stellar contract invoke --id $REGISTRY --source-account $SOURCE_ACCOUNT \
         --network $NETWORK -- get_token
   - Follow the lifecycle events:
       stellar events --id $REGISTRY --network $NETWORK --start-ledger <ledger>
   - Fund the consumer account so create_lease can escrow a deposit.
   - Verification pipeline for providers: docs/integration.md
   - Explorer: https://stellar.expert/explorer/$NETWORK/contract/$REGISTRY
============================================================================
EOF
