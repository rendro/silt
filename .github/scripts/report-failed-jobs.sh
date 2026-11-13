#!/usr/bin/env bash
# Each job of this workflow run that failed gets one GitHub issue,
# "<label>: <job> failed": opened the first time, and while it is open
# each later failure is added to it as a comment. A failed scheduled run
# so reaches the people who watch the repository; the issue is closed by
# hand once the cause is fixed.
#
# Usage: report-failed-jobs.sh <label>
#   <label>  what the run is, at the start of the title: nightly, weekly
#
# Run as the last job of the workflow (it reads the other jobs of the
# run, its own not finished yet). It needs `gh`, `jq`, GH_TOKEN with
# `actions: read` and `issues: write`, and the variables GitHub sets for
# a job: GITHUB_REPOSITORY, GITHUB_RUN_ID, GITHUB_RUN_ATTEMPT,
# GITHUB_SERVER_URL, GITHUB_SHA.
set -euo pipefail

label="${1:?usage: report-failed-jobs.sh <label>}"
repo="$GITHUB_REPOSITORY"
run_url="$GITHUB_SERVER_URL/$repo/actions/runs/$GITHUB_RUN_ID"

# The jobs of this attempt of the run that did not succeed. A job that
# ran over its time limit is `cancelled` (or `timed_out`); this job runs
# only when the run itself was not cancelled, so such a job counts.
failed=$(gh api --paginate "repos/$repo/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs" \
  --jq '.jobs[] | select(.conclusion == "failure" or .conclusion == "timed_out" or .conclusion == "cancelled") | "\(.conclusion)\t\(.name)"')

if [ -z "$failed" ]; then
  echo "No job of this run failed."
  exit 0
fi

# The open issues, by title (the newest 500 are plenty).
open=$(gh issue list --repo "$repo" --state open --limit 500 --json number,title)

while IFS=$'\t' read -r conclusion job; do
  title="$label: $job failed"
  number=$(jq -r --arg title "$title" '[.[] | select(.title == $title)][0].number // empty' <<< "$open")
  if [ -n "$number" ]; then
    gh issue comment "$number" --repo "$repo" \
      --body "It failed again ($conclusion) in $run_url, on commit $GITHUB_SHA."
    echo "Added the run to issue #$number: $title"
  else
    gh issue create --repo "$repo" --title "$title" \
      --body "The scheduled job \"$job\" failed ($conclusion) in $run_url, on commit $GITHUB_SHA.

Each later failure of it is added here while this issue is open. Close it once the cause is fixed; the next failure opens a new one."
    echo "Opened an issue: $title"
  fi
done <<< "$failed"
