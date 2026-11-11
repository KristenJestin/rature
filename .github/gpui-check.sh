#!/usr/bin/env bash
# Runs one check of .github/workflows/gpui-checks.yml and records its result, so the job's last
# step can list every check and the lines that tell why one failed.
#
# Usage: gpui-check.sh <title> <command...>
set -uo pipefail

title=$1
shift
slug=$(printf '%s' "$title" | tr -c 'A-Za-z0-9' '-')
log="$RUNNER_TEMP/check-$slug.log"

echo "+ $*"
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}

if [ "$status" -eq 0 ]; then
  echo "PASS  $title" >>"$RUNNER_TEMP/results.txt"
else
  echo "FAIL  $title (exit $status)" >>"$RUNNER_TEMP/results.txt"
  {
    echo "===== $title (exit $status)"
    sed 's/\x1b\[[0-9;]*[A-Za-z]//g' "$log" |
      grep -E -A14 '^error|^warning: .* generated|^test .* FAILED$|panicked at|^failures:$|^FAIL:|^Diff in|^test result: FAILED' |
      head -n 250
    echo "----- last lines"
    sed 's/\x1b\[[0-9;]*[A-Za-z]//g' "$log" | tail -n 25
    echo
  } >>"$RUNNER_TEMP/failures.txt"
fi
exit "$status"
