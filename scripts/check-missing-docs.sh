#!/usr/bin/env bash
#
# check-missing-docs.sh — fail when a public item in a contract crate has no
# documentation.
#
# The contract crates enable `missing_docs` only under `--cfg keylease_doc_lint`
# (see the comment above that attribute in each lib.rs). The Soroban
# `#[contract]`, `#[contractimpl]`, `#[contracttype]`, `#[contractevent]` and
# `#[contracterror]` macros generate public items with no doc comment and no
# way to attach one, and Rust can only scope a lint to a module, not to one
# macro expansion. So this script builds each crate with the lint on, drops the
# findings whose span comes from one of those macros, and fails on the rest:
# every hand-written public item has to be documented.
#
# Usage
#   ./scripts/check-missing-docs.sh
#
# Requires: cargo, jq. Runs in CI in the "Rustdoc" job. See
# CONTRIBUTING.md#text-and-documentation-checks.

set -euo pipefail

case "${1:-}" in
  "") ;;
  -h|--help) sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
esac

command -v jq >/dev/null 2>&1 || { echo "error: jq is required" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CRATES=(keylease-registry keylease-mock-token)

# Keep only missing_docs findings whose primary span is not inside a Soroban
# contract macro expansion, and print them as "file:line: message".
# `$span` and `\(...)` are jq syntax, not shell expansions.
# shellcheck disable=SC2016
FILTER='
  select(.reason == "compiler-message")
  | .message
  | select(.code.code == "missing_docs")
  | (.spans[] | select(.is_primary)) as $span
  | select(($span.expansion.macro_decl_name // "")
           | test("^#\\[(contract|contractimpl|contracttype|contractevent|contracterror)\\]$")
           | not)
  | "\($span.file_name):\($span.line_start): \(.message)"
'

findings=""
for crate in "${CRATES[@]}"; do
  # `cargo rustc` passes the cfg to this crate only, so dependencies are not
  # rebuilt. A compile error must fail the check, not read as "no findings".
  json="$(cargo rustc --quiet --lib -p "$crate" --message-format=json -- --cfg keylease_doc_lint)" \
    || { printf 'error: %s failed to build with --cfg keylease_doc_lint\n' "$crate" >&2; exit 1; }
  findings+="$(printf '%s\n' "$json" | jq -r "$FILTER")"$'\n'
done

findings="$(printf '%s' "$findings" | sed '/^$/d' | sort -u)"
if [ -n "$findings" ]; then
  printf '%s\n' "$findings" >&2
  printf '\nerror: %d undocumented public item(s); add a /// doc comment to each.\n' \
    "$(printf '%s\n' "$findings" | wc -l)" >&2
  exit 1
fi
echo "ok: every hand-written public item in the contract crates is documented"
