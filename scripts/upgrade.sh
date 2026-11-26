#!/usr/bin/env bash
#
# upgrade.sh — upgrade a deployed KeyLeaseRegistry in place, in the only safe order.
#
# The contract id, storage and balances survive an upgrade, so the sequence
# matters: build exactly what is in the workspace, upload it, install it with
# admin auth, then prove the live contract reports the version you just built.
# The script stops at the first step that fails and exits non-zero if the
# on-chain version() does not match the workspace version.
#
#   1. cargo build --target wasm32v1-none --release --package keylease-registry
#   2. stellar contract upload                 -> Wasm hash (checked against sha256 of the file)
#   3. stellar contract invoke -- upgrade      --admin <ADMIN> --new_wasm_hash <HASH>
#   4. stellar contract invoke -- version      -> must equal [workspace.package] version
#
# Usage
#   ./scripts/upgrade.sh --contract-id C... --admin <identity> [options]
#
# Required (no defaults, on purpose)
#   --contract-id <C...>   Registry contract id to upgrade.        Env: CONTRACT_ID
#   --admin <identity>     stellar CLI identity of the registry     Env: ADMIN_IDENTITY
#                          admin. It signs the transaction and its
#                          address is passed as --admin.
#
# Options
#   --network <name>       stellar CLI network name.  Default: testnet  Env: NETWORK
#   --allow-same-version   Proceed even if the live contract already reports the
#                          workspace version (re-installing the same release).
#   --yes                  Do not ask for confirmation before the upgrade transaction.
#   -h, --help             Show this help.
#
# Read docs/deployment.md#upgrading-safety-checklist before running this
# against a contract that holds escrow.

set -euo pipefail

CONTRACT_ID="${CONTRACT_ID:-}"
ADMIN_IDENTITY="${ADMIN_IDENTITY:-}"
NETWORK="${NETWORK:-testnet}"
ALLOW_SAME_VERSION=0
ASSUME_YES=0

usage() { sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
step()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
info()  { printf '    %s\n' "$*"; }
die()   { printf '\n\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

need_value() { if [ $# -lt 2 ] || [ -z "${2:-}" ]; then die "$1 requires a value"; fi; }

while [ $# -gt 0 ]; do
  case "$1" in
    --contract-id) need_value "$@"; CONTRACT_ID="$2"; shift ;;
    --contract-id=*) CONTRACT_ID="${1#*=}" ;;
    --admin) need_value "$@"; ADMIN_IDENTITY="$2"; shift ;;
    --admin=*) ADMIN_IDENTITY="${1#*=}" ;;
    --network) need_value "$@"; NETWORK="$2"; shift ;;
    --network=*) NETWORK="${1#*=}" ;;
    --allow-same-version) ALLOW_SAME_VERSION=1 ;;
    --yes|-y) ASSUME_YES=1 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'error: unknown argument: %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

# ---------------------------------------------------------------------------
# Validate inputs before touching the network
# ---------------------------------------------------------------------------

[ -n "$CONTRACT_ID" ] || die "--contract-id <C...> is required (no default: name the contract you mean to upgrade)"
[ -n "$ADMIN_IDENTITY" ] || die "--admin <identity> is required (no default: name the admin key that signs the upgrade)"
printf '%s' "$CONTRACT_ID" | grep -Eq '^C[A-Z2-7]{55}$' \
  || die "--contract-id must be a 56-character contract id starting with C, got: $CONTRACT_ID"

if [ "$ASSUME_YES" != "1" ] && [ ! -t 0 ]; then
  die "stdin is not a terminal, so the upgrade cannot be confirmed interactively; pass --yes"
fi

command -v cargo   >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"
command -v stellar >/dev/null 2>&1 || die "stellar CLI not found. See https://developers.stellar.org/docs/tools/cli/stellar-cli"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"
REGISTRY_WASM="$REPO_ROOT/target/wasm32v1-none/release/keylease_registry.wasm"

# The registry inherits `version.workspace = true`, and contractmeta!/version()
# embed CARGO_PKG_VERSION, so this is the value version() must return after.
WORKSPACE_VERSION="$(awk '
  /^\[/ { in_pkg = ($0 == "[workspace.package]"); next }
  in_pkg && $1 == "version" { gsub(/[" ]/, "", $0); sub(/^version=/, "", $0); print; exit }
' Cargo.toml)"
[ -n "$WORKSPACE_VERSION" ] || die "could not read [workspace.package] version from Cargo.toml"

ADMIN_ADDRESS="$(stellar keys address "$ADMIN_IDENTITY" 2>/dev/null)" \
  || die "identity '$ADMIN_IDENTITY' not found in the stellar CLI. List identities with: stellar keys ls"

# Strip the JSON quotes the CLI puts around a returned String.
unquote() { tail -n1 | tr -d '"[:space:]'; }

invoke_read() {
  stellar contract invoke \
    --id "$CONTRACT_ID" \
    --source-account "$ADMIN_IDENTITY" \
    --network "$NETWORK" \
    -- "$@"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

info "network           : $NETWORK"
info "contract id       : $CONTRACT_ID"
info "admin identity    : $ADMIN_IDENTITY"
info "admin address     : $ADMIN_ADDRESS"
info "workspace version : $WORKSPACE_VERSION"

# ---------------------------------------------------------------------------
# 0. Record what is live now
# ---------------------------------------------------------------------------

step "Reading the live contract"
LIVE_ADMIN="$(invoke_read get_admin | unquote)" \
  || die "could not read get_admin from $CONTRACT_ID on $NETWORK (wrong id or network?)"
[ "$LIVE_ADMIN" = "$ADMIN_ADDRESS" ] \
  || die "identity '$ADMIN_IDENTITY' ($ADMIN_ADDRESS) is not the registry admin ($LIVE_ADMIN)"
BEFORE_VERSION="$(invoke_read version | unquote)" \
  || die "could not read version() from $CONTRACT_ID"
info "live version      : $BEFORE_VERSION"
info "live admin        : $LIVE_ADMIN"

if [ "$BEFORE_VERSION" = "$WORKSPACE_VERSION" ] && [ "$ALLOW_SAME_VERSION" != "1" ]; then
  die "the live contract already reports $WORKSPACE_VERSION.
       Bump [workspace.package] version in Cargo.toml so version() proves the new
       code is live, or pass --allow-same-version to re-install this release."
fi

# Best effort: the ledger before the upgrade is the --start-ledger for the event replay.
START_LEDGER="$(stellar ledger latest --network "$NETWORK" --output json 2>/dev/null \
  | grep -Eo '"sequence"[[:space:]]*:[[:space:]]*[0-9]+' | grep -Eo '[0-9]+$' || true)"
START_LEDGER="${START_LEDGER:-<ledger>}"

# ---------------------------------------------------------------------------
# 1. Build
# ---------------------------------------------------------------------------

step "Building Soroban Wasm (wasm32v1-none, release)"
rustup target list --installed 2>/dev/null | grep -qx 'wasm32v1-none' \
  || rustup target add wasm32v1-none
cargo build --target wasm32v1-none --release --package keylease-registry
[ -f "$REGISTRY_WASM" ] || die "expected Wasm not found: $REGISTRY_WASM"
LOCAL_HASH="$(sha256_of "$REGISTRY_WASM")"
info "wasm              : $REGISTRY_WASM"
info "sha256            : $LOCAL_HASH"

# ---------------------------------------------------------------------------
# 2. Upload
# ---------------------------------------------------------------------------

step "Uploading registry Wasm"
WASM_HASH="$(stellar contract upload \
  --wasm "$REGISTRY_WASM" \
  --source-account "$ADMIN_IDENTITY" \
  --network "$NETWORK" | tail -n1 | tr -d '[:space:]' | tr 'A-F' 'a-f')"
printf '%s' "$WASM_HASH" | grep -Eq '^[0-9a-f]{64}$' \
  || die "unexpected upload output, expected a 64-char Wasm hash: $WASM_HASH"
[ "$WASM_HASH" = "$LOCAL_HASH" ] \
  || die "uploaded hash $WASM_HASH does not match the local build $LOCAL_HASH"
info "wasm hash         : $WASM_HASH"

# ---------------------------------------------------------------------------
# 3. Upgrade
# ---------------------------------------------------------------------------

if [ "$ASSUME_YES" != "1" ]; then
  printf '\n    Install %s (%s -> %s) on %s? [y/N] ' \
    "$WASM_HASH" "$BEFORE_VERSION" "$WORKSPACE_VERSION" "$CONTRACT_ID"
  read -r reply || reply=""
  case "$reply" in y|Y|yes|YES) ;; *) die "aborted before the upgrade transaction; nothing was installed" ;; esac
fi

step "Upgrading $CONTRACT_ID (admin = $ADMIN_ADDRESS)"
stellar contract invoke \
  --id "$CONTRACT_ID" \
  --source-account "$ADMIN_IDENTITY" \
  --network "$NETWORK" \
  --send=yes \
  -- upgrade --admin "$ADMIN_ADDRESS" --new_wasm_hash "$WASM_HASH"

# ---------------------------------------------------------------------------
# 4. Verify version()
# ---------------------------------------------------------------------------

step "Verifying on-chain version"
AFTER_VERSION="$(invoke_read version | unquote)" \
  || die "upgrade was sent but version() could not be read back; check the contract manually"
info "version() before  : $BEFORE_VERSION"
info "version() after   : $AFTER_VERSION"
info "workspace version : $WORKSPACE_VERSION"

STATUS="OK"
if [ "$AFTER_VERSION" != "$WORKSPACE_VERSION" ]; then
  STATUS="MISMATCH"
fi

cat <<EOF

============================================================================
 KeyLeaseRegistry upgrade on $NETWORK: $STATUS
============================================================================
 KEYLEASE_CONTRACT_ID=$CONTRACT_ID
 KEYLEASE_NETWORK=$NETWORK
 wasm hash            = $WASM_HASH
 version() before     = $BEFORE_VERSION
 version() after      = $AFTER_VERSION
 workspace version    = $WORKSPACE_VERSION
============================================================================
 Upgrade event
   Expect exactly one contract_upgraded with new_wasm_hash = $WASM_HASH:
     stellar events --id $CONTRACT_ID --network $NETWORK \\
       --start-ledger $START_LEDGER \\
       --topic "AAAADwAAABFjb250cmFjdF91cGdyYWRlZAAAAA=="
 Next steps (docs/deployment.md#upgrading-safety-checklist)
   - Re-read a pre-upgrade lease and service and diff against your notes.
   - Record version, wasm hash and upgrade ledger in docs/deployments.md.
   - Explorer: https://stellar.expert/explorer/$NETWORK/contract/$CONTRACT_ID
============================================================================
EOF

[ "$STATUS" = "OK" ] || die "version() reports '$AFTER_VERSION', expected '$WORKSPACE_VERSION'.
       The new Wasm ($WASM_HASH) is installed but does not report the workspace version."
