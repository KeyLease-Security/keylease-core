#!/usr/bin/env bats

load 'test_helper'

setup() {
  setup_stubs
  SCRIPT="$REPO_ROOT/scripts/sandbox-local.sh"
}

@test "rejects an unknown argument with exit 2" {
  run "$SCRIPT" --nope
  [ "$status" -eq 2 ]
  [[ "$output" == *"unknown argument: --nope"* ]]
}

@test "--help documents the dry run, teardown and the local-only network" {
  run "$SCRIPT" --help
  [ "$status" -eq 0 ]
  [[ "$output" == *"--dry-run"* ]]
  [[ "$output" == *"--teardown"* ]]
  [[ "$output" == *"--network local"* ]]
}

@test "dry run plans a local deployment and calls no CLI" {
  run "$SCRIPT" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"[dry-run] stellar container start local"* ]]
  [[ "$output" == *"[dry-run] stellar contract deploy"* ]]
  [[ "$output" == *"--network local"* ]]
  [[ "$output" != *"--network testnet"* ]]
  [[ "$output" != *"--network mainnet"* ]]
  no_network_calls
}

@test "dry run teardown only stops the local container and removes sandbox files" {
  run "$SCRIPT" --teardown --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"[dry-run] stellar container stop local"* ]]
  [[ "$output" == *"[dry-run] stellar keys rm --force kl-sandbox-admin"* ]]
  [[ "$output" == *"[dry-run] rm -f"* ]]
  [[ "$output" != *"testnet"* ]]
  [[ "$output" != *"mainnet"* ]]
  no_network_calls
}

@test "the sandbox only ever addresses the local network" {
  # No NETWORK variable and no testnet/mainnet mention in any command the
  # script can print: the container ledger is the only target it knows.
  ! grep -Eq -- '--network (testnet|mainnet)' "$SCRIPT"
  grep -q -- '--network local' "$SCRIPT"
}
