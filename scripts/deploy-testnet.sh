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
# Upgrade an existing deployment in place (24h time lock):
#   1. --upgrade          build, upload and propose (prints the execution ledger)
#   2. wait at least 24h so consumers can settle or revoke before rules change
#   3. --execute-upgrade  install the proposed Wasm (fails early on the contract),
#                         then run `migrate`, which lifts the stop a
#                         schema-raising upgrade puts in place (a no-op when the
#                         storage schema is unchanged, refused after a downgrade)
#   or --cancel-upgrade   drop the proposal before it is executed
#   4. stellar contract invoke -- version / schema_version  (verify)
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
#       -> uploads the built Wasm and queues it behind the 24h delay
#
#   REGISTRY=C... ./scripts/deploy-testnet.sh --execute-upgrade
#       -> installs the queued Wasm once the execution ledger has been reached
#
#   REGISTRY=C... ./scripts/deploy-testnet.sh --cancel-upgrade
#       -> drops the queued Wasm without installing it
#
#   ./scripts/deploy-testnet.sh --dry-run
#       -> validates the inputs and prints every command with its resolved
#          values, then exits. Builds nothing, signs nothing, sends nothing
#          and reads nothing from a network. Values that cannot be resolved
#          without a keystore or a network are printed as <placeholders>.
#
# Safety
#   The script refuses to act on mainnet unless KEYLEASE_ALLOW_MAINNET=1 is
#   exported for that one invocation. Mainnet deployment is deferred until the
#   contract is audited (docs/deployments.md). The resolved network is printed
#   in a banner before the first send, and again in the final summary.
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
#   KEYLEASE_ALLOW_MAINNET  Set to 1 to permit NETWORK=mainnet.  Default: unset (refused)
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
KEYLEASE_ALLOW_MAINNET="${KEYLEASE_ALLOW_MAINNET:-}"

MODE="deploy"
DRY_RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --upgrade)         MODE="upgrade" ;;
    --execute-upgrade) MODE="execute-upgrade" ;;
    --cancel-upgrade)  MODE="cancel-upgrade" ;;
    --deploy)          MODE="deploy" ;;
    --dry-run)         DRY_RUN=1 ;;
    -h|--help)
      awk 'NR > 1 { if ($0 ~ /^set -euo pipefail/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
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
warn() { printf '\n\033[1;33mwarning:\033[0m %s\n' "$*"; }
die()  { printf '\n\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Network guard
#
# NETWORK is user-controlled, so a typo or an exported variable can point this
# script at mainnet. Mainnet is intentionally deferred until the contract is
# audited (docs/deployments.md); refuse it unless the operator opts in for this
# one invocation.
# ---------------------------------------------------------------------------

case "$NETWORK" in
  mainnet)
    if [ "$KEYLEASE_ALLOW_MAINNET" != "1" ]; then
      die "refusing NETWORK=mainnet: mainnet deployment is deferred until the contract is audited.
     Set KEYLEASE_ALLOW_MAINNET=1 for this invocation to override, for example:
       KEYLEASE_ALLOW_MAINNET=1 NETWORK=mainnet $0 --dry-run"
    fi
    ;;
esac

banner_network() {
  local mode_note=""
  if [ "$DRY_RUN" = "1" ]; then
    mode_note=" (dry run — nothing is sent)"
  fi
  printf '\n============================================================================\n'
  if [ "$NETWORK" = "mainnet" ]; then
    printf ' !!! MAINNET — real XLM is at risk. KEYLEASE_ALLOW_MAINNET=1 was set. !!!\n'
  fi
  printf ' resolved network : %s\n' "$NETWORK"
  printf ' mode             : %s%s\n' "$MODE" "$mode_note"
  printf ' signing identity : %s\n' "$SOURCE_ACCOUNT"
  printf '============================================================================\n'
}

# ---------------------------------------------------------------------------
# Dry-run plumbing
#
# In --dry-run every network-facing step goes through run_send or run_capture,
# which print the command instead of executing it. Plan output goes to fd 3 so
# that the caller's $(...) capture does not swallow it.
# ---------------------------------------------------------------------------

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

# run_send <command...> — a state-changing or network-touching command.
run_send() {
  if [ "$DRY_RUN" = "1" ]; then
    plan "$@"
  else
    "$@"
  fi
}

# run_capture <placeholder> <command...> — same, for commands whose stdout the
# caller captures. In a dry run the placeholder is emitted instead.
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

build_wasm() {
  if [ "$DRY_RUN" = "1" ]; then
    rustup target list --installed 2>/dev/null | grep -qx 'wasm32v1-none' \
      || plan rustup target add wasm32v1-none
    plan cargo build --target wasm32v1-none --release --package keylease-registry
    return 0
  fi
  rustup target list --installed 2>/dev/null | grep -qx 'wasm32v1-none' \
    || rustup target add wasm32v1-none
  cargo build --target wasm32v1-none --release --package keylease-registry
  [ -f "$REGISTRY_WASM" ] || die "expected Wasm not found: $REGISTRY_WASM"
}

if [ "$DRY_RUN" != "1" ]; then
  command -v cargo   >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"
  command -v stellar >/dev/null 2>&1 || die "stellar CLI not found. See https://developers.stellar.org/docs/tools/cli/stellar-cli"
fi

banner_network

# ---------------------------------------------------------------------------
# 0. Signing identity
# ---------------------------------------------------------------------------

if [ "$KEYLEASE_GENERATE_KEY" = "1" ]; then
  step "Generating and funding a Testnet identity: $SOURCE_ACCOUNT"
  run_send stellar keys generate "$SOURCE_ACCOUNT" --network "$NETWORK" --fund
fi

if [ "$DRY_RUN" != "1" ] && ! stellar keys address "$SOURCE_ACCOUNT" >/dev/null 2>&1; then
  die "identity '$SOURCE_ACCOUNT' not found. Create one with:
       stellar keys generate $SOURCE_ACCOUNT --network $NETWORK --fund
     or set KEYLEASE_GENERATE_KEY=1 (testnet only)."
fi

# Resolve the admin from the keystore when it can be read locally. A dry run
# must not fail because the toolchain or the identity is missing: it prints a
# placeholder so the plan still shows every argument's position.
ADMIN="${ADMIN:-}"
if [ -z "$ADMIN" ]; then
  ADMIN="$(stellar keys address "$SOURCE_ACCOUNT" 2>/dev/null || true)"
  [ -n "$ADMIN" ] || ADMIN="<address of $SOURCE_ACCOUNT>"
fi
PROVIDER="${PROVIDER:-$ADMIN}"

# ---------------------------------------------------------------------------
# Upgrade modes: propose -> wait 24h -> execute (or cancel)
# ---------------------------------------------------------------------------

require_registry() {
  [ -n "${REGISTRY:-}" ] || die "REGISTRY=<contract id> is required for upgrade modes"
  printf '%s' "$REGISTRY" | grep -Eq '^C[A-Z0-9]{55}$' \
    || die "REGISTRY must be a contract id starting with C, got: $REGISTRY"
}

if [ "$MODE" = "upgrade" ]; then
  require_registry

  step "Building Soroban Wasm (wasm32v1-none, release)"
  build_wasm

  step "Uploading registry Wasm"
  WASM_HASH="$(run_capture '<wasm-hash>' stellar contract upload \
    --wasm "$REGISTRY_WASM" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" | tail -n1)"

  if [ "$DRY_RUN" != "1" ]; then
    printf '%s' "$WASM_HASH" | grep -Eq '^[0-9a-fA-F]{64}$' \
      || die "unexpected upload output, expected a 64-char Wasm hash: $WASM_HASH"
  fi
  info "wasm hash: $WASM_HASH"

  step "Proposing upgrade for $REGISTRY (admin = $ADMIN)"
  run_send stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    --send=yes \
    -- propose_upgrade --admin "$ADMIN" --new_wasm_hash "$WASM_HASH"

  step "Pending upgrade: Wasm hash and execution ledger"
  run_send stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    -- get_pending_upgrade

  if [ "$DRY_RUN" = "1" ]; then
    printf '\ndry run: the proposal was not sent; nothing is pending on %s.\n' "$NETWORK"
    exit 0
  fi

  cat <<EOF

============================================================================
 KeyLeaseRegistry upgrade proposed on $NETWORK
============================================================================
 KEYLEASE_CONTRACT_ID=$REGISTRY
 wasm hash            = $WASM_HASH
============================================================================
 The Wasm is NOT installed yet. Execution is legal only at/after the
 execute_after ledger timestamp printed above (24h after this proposal).
 Consumers can watch upgrade_proposed / get_pending_upgrade and settle or
 revoke in-flight leases during that window.
 Next steps
   - Execute after the delay:
       REGISTRY=$REGISTRY $0 --execute-upgrade
   - Cancel the proposal instead:
       REGISTRY=$REGISTRY $0 --cancel-upgrade
============================================================================
EOF
  exit 0
fi

if [ "$MODE" = "execute-upgrade" ]; then
  require_registry

  step "Executing pending upgrade on $REGISTRY"
  run_send stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    --send=yes \
    -- execute_upgrade --admin "$ADMIN"

  # State-changing calls stay stopped until the stored data matches the new
  # Wasm's storage schema. `migrate` is a silent no-op when nothing changed and
  # fails with SchemaVersionUnsupported after a downgrade, in which case the
  # previous Wasm must be reinstalled instead.
  step "Migrating storage schema"
  run_send stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    --send=yes \
    -- migrate --admin "$ADMIN" \
    || die "migrate failed: the installed Wasm cannot read this registry's storage schema; propose the previous Wasm hash"

  SCHEMA="$(run_capture '<schema-version>' stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    -- schema_version | tail -n1)"

  step "Verifying on-chain version"
  VERSION="$(run_capture '<version>' stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    -- version | tail -n1)"

  if [ "$DRY_RUN" = "1" ]; then
    printf '\ndry run: the upgrade was not executed; %s is unchanged.\n' "$REGISTRY"
    exit 0
  fi

  cat <<EOF

============================================================================
 KeyLeaseRegistry upgraded on $NETWORK
============================================================================
 KEYLEASE_CONTRACT_ID=$REGISTRY
 KEYLEASE_NETWORK=$NETWORK
 version()            = $VERSION
 schema_version()     = $SCHEMA
============================================================================
 Next steps
   - Watch the upgrade event:
       stellar events --id $REGISTRY --network $NETWORK --start-ledger <ledger>
   - Verify storage and balances are intact:
       stellar contract invoke --id $REGISTRY --source-account $SOURCE_ACCOUNT \\
         --network $NETWORK -- get_service --service_id 1
   - Explorer: https://stellar.expert/explorer/$NETWORK/contract/$REGISTRY
============================================================================
EOF
  exit 0
fi

if [ "$MODE" = "cancel-upgrade" ]; then
  require_registry

  step "Cancelling pending upgrade on $REGISTRY"
  run_send stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    --send=yes \
    -- cancel_upgrade --admin "$ADMIN"

  info "pending upgrade removed; the running Wasm was not changed"
  exit 0
fi

if [ -z "${TOKEN:-}" ]; then
  if [ "$DRY_RUN" = "1" ]; then
    TOKEN="$(stellar contract id asset --network "$NETWORK" --asset native 2>/dev/null || true)"
    [ -n "$TOKEN" ] || TOKEN="<native SAC on $NETWORK>"
  else
    TOKEN="$(stellar contract id asset --network "$NETWORK" --asset native)"
  fi
fi

if [ -z "${ENDPOINT_HASH:-}" ]; then
  if printf '%s' "$PROVIDER" | grep -q '^<'; then
    ENDPOINT_HASH="<sha256(\"$PROVIDER:$SERVICE_NAME\")>"
  else
    if command -v sha256sum >/dev/null 2>&1; then
      ENDPOINT_HASH="$(printf '%s' "${PROVIDER}:${SERVICE_NAME}" | sha256sum | awk '{print $1}')"
    else
      ENDPOINT_HASH="$(printf '%s' "${PROVIDER}:${SERVICE_NAME}" | shasum -a 256 | awk '{print $1}')"
    fi
  fi
fi

# Input validation runs in every mode, including a dry run: the point of the
# plan is to fail before a transaction could be built, not after.
case "$RATE_PER_CALL" in
  ''|*[!0-9]*) die "RATE_PER_CALL must be a whole number of stroops, got: $RATE_PER_CALL" ;;
esac

case "$ENDPOINT_HASH" in
  '<'*'>')
    # Only possible in --dry-run: an unresolved PROVIDER cannot be hashed.
    info "endpoint hash  : $ENDPOINT_HASH (unresolved in this dry run)"
    ;;
  *)
    if ! printf '%s' "$ENDPOINT_HASH" | grep -Eq '^[0-9a-fA-F]{64}$'; then
      die "ENDPOINT_HASH must be exactly 64 hex characters (32 bytes), got: $ENDPOINT_HASH"
    fi
    ENDPOINT_HASH="$(printf '%s' "$ENDPOINT_HASH" | tr 'A-F' 'a-f')"
    ;;
esac

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
REGISTRY="$(run_capture '<registry-contract-id>' stellar contract deploy \
  --wasm "$REGISTRY_WASM" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --alias "$REGISTRY_ALIAS" | tail -n1)"

if [ "$DRY_RUN" != "1" ]; then
  printf '%s' "$REGISTRY" | grep -Eq '^C[A-Z0-9]{55}$' \
    || die "unexpected deploy output, expected a contract id starting with C: $REGISTRY"
fi
info "registry contract id: $REGISTRY"

# ---------------------------------------------------------------------------
# 3. Initialize (once)
# ---------------------------------------------------------------------------

step "Initializing registry (admin = $ADMIN)"
run_send stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --send=yes \
  -- init --admin "$ADMIN"

# ---------------------------------------------------------------------------
# 4. Configure the escrow token
# ---------------------------------------------------------------------------

step "Configuring escrow token ($TOKEN)"
run_send stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --send=yes \
  -- set_token --admin "$ADMIN" --token "$TOKEN"

# ---------------------------------------------------------------------------
# 5. Publish a service
# ---------------------------------------------------------------------------

step "Registering service (provider = $PROVIDER)"
SERVICE_ID="$(run_capture '<service-id>' stellar contract invoke \
  --id "$REGISTRY" \
  --source-account "$SOURCE_ACCOUNT" \
  --network "$NETWORK" \
  --send=yes \
  -- register_service \
     --provider "$PROVIDER" \
     --rate_per_call "$RATE_PER_CALL" \
     --endpoint_hash "$ENDPOINT_HASH" | tail -n1)"

if [ "$DRY_RUN" != "1" ]; then
  printf '%s' "$SERVICE_ID" | grep -Eq '^[0-9]+$' \
    || die "unexpected register_service output, expected a u64 service id: $SERVICE_ID"

  # Verify the write actually landed on-chain.
  stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    -- get_service --service_id "$SERVICE_ID" >/dev/null
else
  run_send stellar contract invoke \
    --id "$REGISTRY" \
    --source-account "$SOURCE_ACCOUNT" \
    --network "$NETWORK" \
    -- get_service --service_id "$SERVICE_ID"
fi

# ---------------------------------------------------------------------------
# Dry run: stop before printing values that only exist after a real deploy.
# ---------------------------------------------------------------------------

if [ "$DRY_RUN" = "1" ]; then
  cat <<EOF

============================================================================
 Dry run — nothing was built, signed or sent
============================================================================
 network            : $NETWORK
 mode               : $MODE
 signing identity   : $SOURCE_ACCOUNT
 admin              : $ADMIN
 provider           : $PROVIDER
 escrow token       : $TOKEN
 rate / call        : $RATE_PER_CALL
 endpoint hash      : $ENDPOINT_HASH
============================================================================
 Every command above was printed instead of executed. Any value shown as
 <...> needs a keystore, an environment variable or a network, and is only
 resolved by a real run. Re-run without --dry-run to deploy.
============================================================================
EOF
  exit 0
fi

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
       stellar contract invoke --id $REGISTRY --source-account $SOURCE_ACCOUNT \\
         --network $NETWORK -- version
       stellar contract invoke --id $REGISTRY --source-account $SOURCE_ACCOUNT \\
         --network $NETWORK -- get_token
   - Follow the lifecycle events:
       stellar events --id $REGISTRY --network $NETWORK --start-ledger <ledger>
   - Fund the consumer account so create_lease can escrow a deposit.
   - Verification pipeline for providers: docs/integration.md
   - Explorer: https://stellar.expert/explorer/$NETWORK/contract/$REGISTRY
============================================================================
EOF
