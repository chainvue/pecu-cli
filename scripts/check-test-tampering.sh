#!/usr/bin/env bash
#
# Look for the specific ways a change can turn a red build green by testing
# less than it did before.
#
#   scripts/check-test-tampering.sh origin/main            # ...HEAD
#   scripts/check-test-tampering.sh origin/main my-branch
#
# Prints JSON on stdout. Exit status is 0 whatever it finds -- the finding is
# data for the review, not a verdict here.
#
# Two tiers, because they are different problems:
#
#   hard  A `#[test]` removed, an `#[ignore]` added, or a test file deleted.
#         There is no reading of these that is a normal part of implementing a
#         feature, so the review is forced to `changes_requested` and the score
#         does not get a vote.
#
#   soft  An existing test file or an insta snapshot was modified. This is
#         often legitimate -- a spec that changes behaviour changes the test
#         that pins it -- so it is not fatal, but the reviewer has to say out
#         loud why each one was justified.
#
# The patterns below are Rust and insta. For another language, that is the part
# to change: the two tiers and everything downstream stay as they are.
set -euo pipefail

TEST_DIR="tests"
SNAPSHOT_DIR="tests/snapshots"
TEST_ATTR='#\[test\]'
IGNORE_ATTR='#\[ignore'

BASE="${1:?usage: check-test-tampering.sh <base-ref> [<head-ref>]}"
HEAD="${2:-HEAD}"

# `...` is the right operator: compare against the merge base, so commits that
# landed on main after the branch started are not read as the branch's doing.
RANGE="$BASE...$HEAD"

diff_in_tests=$(git diff "$RANGE" -- "$TEST_DIR" || true)

count_matching() {
  # $1 = '+' or '-', $2 = extended regex. `+++`/`---` header lines are dropped
  # so a renamed file does not read as a removed test.
  local sign="$1" pattern="$2"
  printf '%s\n' "$diff_in_tests" \
    | grep -E "^\\${sign}" \
    | grep -vE "^\\${sign}\\${sign}\\${sign}" \
    | grep -cE "$pattern" || true
}

removed_tests=$(count_matching '-' "$TEST_ATTR")
added_ignores=$(count_matching '+' "$IGNORE_ATTR")
removed_asserts=$(count_matching '-' 'assert')

files_json() {
  # $1 = git diff --diff-filter value, $2 = pathspec
  git diff --name-only --diff-filter="$1" "$RANGE" -- "$2" 2>/dev/null \
    | jq -R . | jq -sc . || echo '[]'
}

deleted_test_files=$(files_json D "$TEST_DIR")
modified_test_files=$(files_json M "$TEST_DIR")
added_test_files=$(files_json A "$TEST_DIR")
changed_snapshots=$(git diff --name-only "$RANGE" -- "$SNAPSHOT_DIR" 2>/dev/null \
  | jq -R . | jq -sc . || echo '[]')

hard=false
if [ "$removed_tests" -gt 0 ] || [ "$added_ignores" -gt 0 ] \
   || [ "$(echo "$deleted_test_files" | jq 'length')" -gt 0 ]; then
  hard=true
fi

soft=false
if [ "$(echo "$modified_test_files" | jq 'length')" -gt 0 ] \
   || [ "$(echo "$changed_snapshots" | jq 'length')" -gt 0 ] \
   || [ "$removed_asserts" -gt 0 ]; then
  soft=true
fi

jq -n \
  --argjson hard "$hard" \
  --argjson soft "$soft" \
  --argjson removed_tests "$removed_tests" \
  --argjson added_ignores "$added_ignores" \
  --argjson removed_asserts "$removed_asserts" \
  --argjson deleted_test_files "$deleted_test_files" \
  --argjson modified_test_files "$modified_test_files" \
  --argjson added_test_files "$added_test_files" \
  --argjson changed_snapshots "$changed_snapshots" \
  '{
     hard_violation: $hard,
     needs_justification: $soft,
     removed_test_attributes: $removed_tests,
     added_ignore_attributes: $added_ignores,
     removed_assertion_lines: $removed_asserts,
     deleted_test_files: $deleted_test_files,
     modified_test_files: $modified_test_files,
     added_test_files: $added_test_files,
     changed_snapshots: $changed_snapshots
   }'
