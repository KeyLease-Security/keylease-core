#!/usr/bin/env bash
#
# setup-repo.sh — apply the repository settings keylease-core expects.
#
# Applies, in order:
#   1. GitHub topics      (so the repo is discoverable in the Stellar ecosystem)
#   2. Wave labels        (wave-trivial/100pts, wave-medium/150pts, wave-high/200pts, security)
#   3. Branch protection  (require PRs + approvals + the exact CI checks)
#
# Each section is applied independently. A section that fails (for example
# because the token lacks the `administration` permission) is reported and the
# script continues, then exits non-zero.
#
# Usage
#   ./scripts/setup-repo.sh --dry-run     # show what would change
#   ./scripts/setup-repo.sh               # apply
#
# Requires the GitHub CLI. Topics and branch protection require a token with
# `administration: write`; labels and issues only need `issues: write`.

set -uo pipefail

REPO="${KEYLEASE_REPO:-KeyLease-Security/keylease-core}"
BRANCH="${KEYLEASE_BRANCH:-main}"
DRY_RUN=0

[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

command -v gh >/dev/null 2>&1 || { echo "error: gh CLI not found" >&2; exit 1; }

FAILED=0
fail() { echo "  ✗ $1"; FAILED=1; }

# ---------------------------------------------------------------------------
# 1. Topics
# ---------------------------------------------------------------------------

TOPICS='["soroban","stellar","smart-contracts","rust","escrow","payments","api","rpc","drips-wave","metering"]'

echo "==> Topics"
if [ "$DRY_RUN" = "1" ]; then
  echo "  [dry-run] PUT /repos/$REPO/topics"
  echo "            {\"names\": $TOPICS}"
elif printf '{"names": %s}' "$TOPICS" | gh api -X PUT "repos/$REPO/topics" \
       -H "Accept: application/vnd.github+json" --input - >/dev/null 2>&1; then
  echo "  ✓ set $TOPICS"
else
  fail "could not set topics — the token needs 'administration: write'"
fi

# ---------------------------------------------------------------------------
# 2. Wave labels
# ---------------------------------------------------------------------------

# name|color|description
LABELS_OF='
wave-trivial|0e8a16|Drips Wave: trivial task (100 pts)
good first issue|7057ff|Good for newcomers
wave-medium|fbca04|Drips Wave: medium task (150 pts)
wave-high|b60205|Drips Wave: high-impact task (200 pts)
100pts|c2e0c6|Worth 100 Wave points
150pts|fef2c0|Worth 150 Wave points
200pts|f9d0c4|Worth 200 Wave points
security|d73a4a|Security-sensitive change
'

echo "==> Labels"
while IFS='|' read -r name color desc; do
  [ -z "$name" ] && continue
  if [ "$DRY_RUN" = "1" ]; then
    echo "  [dry-run] gh label create \"$name\" --color $color --description \"$desc\" --force"
  elif gh label create "$name" --repo "$REPO" --color "$color" \
       --description "$desc" --force >/dev/null 2>&1; then
    echo "  ✓ $name"
  else
    fail "could not create label '$name'"
  fi
done <<< "$LABELS_OF"

# ---------------------------------------------------------------------------
# 3. Branch protection
#
# The required status check names MUST match the `name:` fields in
# .github/workflows/ci.yml. If you rename a job there, update them here.
# ---------------------------------------------------------------------------

PROTECTION='{
  "required_status_checks": {
    "strict": true,
    "contexts": ["Format", "Clippy", "Test", "Build (wasm32v1-none)"]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": false,
    "required_approving_review_count": 1
  },
  "restrictions": null,
  "required_conversation_resolution": true,
  "allow_force_pushes": false,
  "allow_deletions": false
}'

echo "==> Branch protection on $BRANCH"
if [ "$DRY_RUN" = "1" ]; then
  echo "  [dry-run] PUT /repos/$REPO/branches/$BRANCH/protection"
  echo "$PROTECTION" | sed 's/^/            /'
elif printf '%s' "$PROTECTION" | gh api -X PUT \
       "repos/$REPO/branches/$BRANCH/protection" \
       -H "Accept: application/vnd.github+json" --input - >/dev/null 2>&1; then
  echo "  ✓ protection applied"
else
  fail "could not set branch protection — the token needs 'administration: write'"
fi

echo
if [ "$FAILED" = "1" ]; then
  echo "Finished with errors. Verify permissions at https://github.com/$REPO/settings"
  exit 1
fi
echo "Done. Verify at https://github.com/$REPO/settings/branches"
