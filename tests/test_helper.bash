# Shared helpers for the bats suite.
#
# Every external CLI the scripts call is stubbed from tests/stubs: the stubs
# record their argv in $STUB_LOG and answer with fixed values, so the suite
# needs no network, no credentials and no Stellar toolchain.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STUBS="$REPO_ROOT/tests/stubs"

# setup_stubs puts the stubs first on PATH and starts a fresh call log.
setup_stubs() {
  export STUB_LOG="$BATS_TEST_TMPDIR/cli.log"
  : >"$STUB_LOG"
  export PATH="$STUBS:$PATH"
}

# run_without_gh <script> [args...] runs a script under bash with a PATH that
# contains no `gh`, to exercise the "CLI not found" branch.
run_without_gh() {
  local script="$1"
  shift
  mkdir -p "$BATS_TEST_TMPDIR/no-tools"
  run env PATH="$BATS_TEST_TMPDIR/no-tools" /bin/bash "$script" "$@"
}

# no_network_calls asserts the stub log holds no command that would talk to a
# network or change state.
no_network_calls() {
  ! grep -Eq 'contract (deploy|invoke|upload)|keys generate|container (start|stop)|network health' "$STUB_LOG"
}
