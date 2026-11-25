#!/usr/bin/env bats

load 'test_helper'

setup() {
  setup_stubs
  SCRIPT="$REPO_ROOT/scripts/setup-repo.sh"
}

@test "fails clearly when the GitHub CLI is missing" {
  run_without_gh "$SCRIPT" --dry-run
  [ "$status" -eq 1 ]
  [[ "$output" == *"gh CLI not found"* ]]
}

@test "dry run prints topics, labels and branch protection without changing anything" {
  run "$SCRIPT" --dry-run
  [ "$status" -eq 0 ]
  [[ "$output" == *"[dry-run] PUT /repos/"* ]]
  [[ "$output" == *"wave-medium"* ]]
  [[ "$output" == *"Shell Tests"* ]]
  [ ! -s "$STUB_LOG" ]
}

@test "an unrecognized argument is ignored rather than applied" {
  # setup-repo only reads $1 (--dry-run), so anything else must stay a dry run.
  run "$SCRIPT" --dry-run --unexpected
  [ "$status" -eq 0 ]
  [[ "$output" == *"[dry-run]"* ]]
  [ ! -s "$STUB_LOG" ]
}
