#!/usr/bin/env bats

load 'test_helper'

setup() {
  setup_stubs
  SCRIPT="$REPO_ROOT/examples/consumer/keylease-consumer.sh"
  ENV="$BATS_TEST_TMPDIR/consumer.env"
  cat >"$ENV" <<EOF
KEYLEASE_CONTRACT_ID=C$(printf 'A%.0s' $(seq 1 55))
KEYLEASE_SERVICE_ID=1
KEYLEASE_NETWORK=testnet
KEYLEASE_CONSUMER_IDENTITY=consumer
KEYLEASE_REQUESTED_CALLS=500
KEYLEASE_DURATION_SECS=3600
EOF
  chmod +x "$SCRIPT"
}

@test "create validates service and prints lease id" {
  run "$SCRIPT" create --env "$ENV"
  [ "$status" -eq 0 ]
  [[ "$output" == *"lease_id="* ]]
  grep -q 'contract invoke.*get_service.*--service_id 1' "$STUB_LOG"
  grep -q 'contract invoke.*create_lease' "$STUB_LOG"
  ! grep -q 'secret' "$STUB_LOG"
}

@test "status validates lease id and service" {
  run "$SCRIPT" status --lease-id 7 --env "$ENV"
  [ "$status" -eq 0 ]
  grep -q 'get_service.*--service_id 1' "$STUB_LOG"
  grep -q 'get_lease.*--lease_id 7' "$STUB_LOG"
}

@test "revoke uses the consumer keystore identity and grace-aware contract entry point" {
  run "$SCRIPT" revoke --lease-id 7 --env "$ENV"
  [ "$status" -eq 0 ]
  grep -q 'revoke_expired_lease.*--lease_id 7' "$STUB_LOG"
  grep -q 'keys address consumer' "$STUB_LOG"
}

@test "rejects missing env and malformed service id" {
  run "$SCRIPT" create --env "$BATS_TEST_TMPDIR/missing.env"
  [ "$status" -eq 1 ]
  [[ "$output" == *"env file not found"* ]]
  sed -i.bak 's/KEYLEASE_SERVICE_ID=1/KEYLEASE_SERVICE_ID=0/' "$ENV"
  run "$SCRIPT" create --env "$ENV"
  [ "$status" -eq 1 ]
  [[ "$output" == *"KEYLEASE_SERVICE_ID must be a positive integer"* ]]
}
