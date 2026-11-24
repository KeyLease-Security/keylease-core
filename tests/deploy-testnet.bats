#!/usr/bin/env bats

load 'test_helper'

setup() {
  setup_stubs
  SCRIPT="$REPO_ROOT/scripts/deploy-testnet.sh"
  HEX64="$(printf 'ab%.0s' $(seq 1 32))"
}

@test "rejects an unknown argument with exit 2" {
  run "$SCRIPT" --nope
  [ "$status" -eq 2 ]
  [[ "$output" == *"unknown argument: --nope"* ]]
}

@test "--help documents the dry run and the mainnet opt-in" {
  run "$SCRIPT" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"--dry-run"* ]]
  [[ "$output" == *"KEYLEASE_ALLOW_MAINNET"* ]]
}

@test "dry run prints every command and calls no network" {
  run "$SCRIPT" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"[dry-run] stellar contract deploy"* ]]
  [[ "$output" == *"[dry-run] stellar contract invoke"* ]]
  [[ "$output" == *"Dry run — nothing was built, signed or sent"* ]]
  no_network_calls
}

@test "dry run prints the resolved network, admin, token and rate" {
  ADMIN="GADMINAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
  PROVIDER="GPROVIDERAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
  TOKEN="CTOKENAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB"
  export ADMIN PROVIDER TOKEN ENDPOINT_HASH="$HEX64"
  run "$SCRIPT" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"network        : testnet"* ]]
  [[ "$output" == *"admin          : $ADMIN"* ]]
  [[ "$output" == *"provider       : $PROVIDER"* ]]
  [[ "$output" == *"escrow token   : $TOKEN"* ]]
  [[ "$output" == *"rate / call    : 1000000"* ]]
  [[ "$output" == *"endpoint hash  : $HEX64"* ]]
}

@test "dry run resolves the admin from the local keystore" {
  run "$SCRIPT" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"admin          : GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF"* ]]
  grep -q '^stellar keys address ' "$STUB_LOG"
  no_network_calls
}

@test "refuses mainnet without the explicit opt-in" {
  run env NETWORK=mainnet "$SCRIPT" --dry-run
  [ "$status" -eq 1 ]
  [[ "$output" == *"refusing NETWORK=mainnet"* ]]
  [ ! -s "$STUB_LOG" ]
}

@test "allows a mainnet dry run with KEYLEASE_ALLOW_MAINNET=1 and warns loudly" {
  run env NETWORK=mainnet KEYLEASE_ALLOW_MAINNET=1 "$SCRIPT" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"MAINNET — real XLM is at risk"* ]]
  [[ "$output" == *"resolved network : mainnet"* ]]
  [[ "$output" == *"[dry-run] stellar contract deploy"* ]]
  no_network_calls
}

@test "rejects a malformed ENDPOINT_HASH before planning a send" {
  run env ENDPOINT_HASH=deadbeef "$SCRIPT" --dry-run
  [ "$status" -eq 1 ]
  [[ "$output" == *"ENDPOINT_HASH must be exactly 64 hex characters"* ]]
  [[ "$output" != *"[dry-run] stellar contract deploy"* ]]
}

@test "rejects a non-numeric RATE_PER_CALL" {
  run env RATE_PER_CALL=0.1xlm "$SCRIPT" --dry-run
  [ "$status" -eq 1 ]
  [[ "$output" == *"RATE_PER_CALL must be a whole number of stroops"* ]]
}

@test "upgrade mode requires a REGISTRY" {
  run "$SCRIPT" --upgrade
  [ "$status" -eq 1 ]
  [[ "$output" == *"REGISTRY=<contract id> is required"* ]]
}

@test "rejects a malformed REGISTRY id" {
  run env REGISTRY=not-a-contract-id "$SCRIPT" --execute-upgrade
  [ "$status" -eq 1 ]
  [[ "$output" == *"REGISTRY must be a contract id starting with C"* ]]
}

@test "upgrade dry run plans the upload and proposal but sends nothing" {
  REGISTRY="C$(printf 'A%.0s' $(seq 1 55))"
  export REGISTRY
  run "$SCRIPT" --upgrade --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"[dry-run] stellar contract upload"* ]]
  [[ "$output" == *"[dry-run] stellar contract invoke --id $REGISTRY"* ]]
  no_network_calls
}
