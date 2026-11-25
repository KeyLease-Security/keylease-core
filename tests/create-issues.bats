#!/usr/bin/env bats

load 'test_helper'

setup() {
  setup_stubs
  SCRIPT="$REPO_ROOT/scripts/create-issues.sh"
}

@test "rejects an unknown argument with exit 2" {
  run "$SCRIPT" --nope
  [ "$status" -eq 2 ]
  [[ "$output" == *"unknown argument: --nope"* ]]
}

@test "fails clearly when the GitHub CLI is missing" {
  run_without_gh "$SCRIPT" --dry-run
  [ "$status" -eq 1 ]
  [[ "$output" == *"gh CLI not found"* ]]
}

@test "dry run prints the backlog and creates no issue" {
  run "$SCRIPT" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"title : [medium] Add a local-network sandbox script"* ]]
  [[ "$output" == *"title : [high] Fuzz the escrow invariant"* ]]
  [[ "$output" == *"dry run: would create"* ]]
  ! grep -q '^gh issue create' "$STUB_LOG"
}

@test "--only limits the run to one tier" {
  run "$SCRIPT" --dry-run --only high
  [ "$status" -eq 0 ]
  [[ "$output" == *"title : [high] Fuzz the escrow invariant"* ]]
  [[ "$output" != *"title : [medium] Let providers update a service"* ]]
  [[ "$output" != *"title : [trivial] Enforce a Wasm size budget in CI"* ]]
}

@test "--backlog-only skips the hand-written issues" {
  run "$SCRIPT" --dry-run --backlog-only
  [ "$status" -eq 0 ]
  # "Fuzz the escrow invariant" is only defined inline in create-issues.sh.
  [[ "$output" != *"title : [high] Fuzz the escrow invariant"* ]]
  [[ "$output" == *"title : [medium] Add a local-network sandbox script"* ]]
  [[ "$output" == *"dry run: would create"* ]]
}

@test "an unknown tier is rejected" {
  run "$SCRIPT" --dry-run --only urgent
  [ "$status" -eq 0 ]
  [[ "$output" == *"dry run: would create 0 issue(s)"* ]]
}
