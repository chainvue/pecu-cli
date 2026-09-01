#!/usr/bin/env bash
#
# Create (or update) every label the Claude automation depends on.
#
#   scripts/setup-labels.sh                    # the repo you are standing in
#   scripts/setup-labels.sh chainvue/pecu-cli  # somewhere else
#
# Idempotent: `gh label create --force` updates the colour and description of a
# label that already exists rather than failing, so re-running after an edit to
# this file brings the repo back in line.
#
# The labels are the automation's whole state machine. Nothing else records
# where an issue is, which is deliberate -- the state is visible in the issue
# list instead of in a database nobody can see.
set -euo pipefail

REPO="${1:-}"
if [ -z "$REPO" ]; then
  REPO=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
fi

if ! gh auth status >/dev/null 2>&1; then
  echo "not logged in: run \`gh auth login\` first" >&2
  exit 1
fi

echo "Labels for $REPO"

label() {
  local name="$1" color="$2" description="$3"
  if gh label create "$name" --repo "$REPO" --color "$color" \
       --description "$description" --force >/dev/null 2>&1; then
    printf '  %-26s %s\n' "$name" "$description"
  else
    printf '  %-26s FAILED\n' "$name" >&2
    return 1
  fi
}

# --- the pipeline, in the order an issue moves through it --------------------
#
# A maintainer sets `claude:ready` by hand. Everything after that is set by a
# workflow, and every one of them is terminal or hands over to the next stage.

label 'claude:ready'      '1d76db' 'Ready for the spec gate -- set this by hand when the issue is written'
label 'claude:needs-spec' 'fbca04' 'Spec gate found gaps; see its numbered questions on the issue'
label 'claude:approved'   '0e8a16' 'Spec is complete; implementation starts on this label'
label 'claude:in-progress' 'c5def5' 'An implementation run holds this issue (lock)'
label 'claude:blocked'    '000000' 'Automation gave up -- a human has to look'
label 'claude:rejected'   'ffffff' 'Declined by the spec gate, with a reason in the comments'

# --- pull request outcome ----------------------------------------------------

label 'claude:merge-candidate' '0e8a16' 'Adversarial review scored this high on a low-risk path'

# --- blast radius ------------------------------------------------------------
#
# Set by the PR review from .github/claude-risk-paths.yml, and by the spec gate
# on the issue as an early warning. `risk:high` alone blocks a merge
# recommendation, whatever the score.

label 'risk:low'    'c2e0c6' 'Blast radius: visible immediately, costs nothing'
label 'risk:medium' 'fbca04' 'Blast radius: shows a person a wrong number or a wrong refusal'
label 'risk:high'   'b60205' 'Blast radius: money, key material, identity control, or CI itself'

echo
echo "Done. Nothing was deleted -- labels this script does not name were left alone."
