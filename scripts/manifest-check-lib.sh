# shellcheck shell=bash
# Shared warn/fail plumbing for the operator-manifest tripwire scripts
# (bash: the deferred-problem list below uses arrays; every sourcing
# script already runs under #!/usr/bin/env bash).
# (check-manifest-drift.sh, check-manifest-expiry.sh,
# check-manifest-funding.sh). Sourced, not executed — so the mode
# handling cannot drift between the scripts.
#
# Usage in a script:
#   . "$(dirname "$0")/manifest-check-lib.sh"
#   parse_mode_flag "$@"; shift "$MODE_ARGS"
#   ...
#   problem "what went wrong"
#
# parse_mode_flag consumes a leading --warn: mode=warn makes problem()
# emit a single-line GitHub ::warning:: annotation and exit 0 instead
# of failing. Used on pull_request runs, where the flagged condition is
# a state of main that the PR author cannot fix (expiry and funding are
# fixed by dispatching sign-manifest / funding the account; and drift
# is *by design* red on the very PR that remedies it, since sign-manifest
# only re-aligns the signed bytes after that PR merges) — so it must
# not gate their PR. Push-to-main runs use the default hard-fail mode,
# where the people who can act see it.

# MODE_ARGS is consumed by the sourcing scripts' `shift "$MODE_ARGS"`.
# shellcheck disable=SC2034
mode=fail
MODE_ARGS=0

parse_mode_flag() {
  if [ "${1:-}" = "--warn" ]; then
    mode=warn
    MODE_ARGS=1
  fi
}

# Report a problem: hard failure by default, a single-line ::warning::
# annotation (annotations cannot span lines) under --warn.
problem() {
  if [ "$mode" = warn ]; then
    echo "::warning::$*"
    exit 0
  fi
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

# Deferred variant for scripts that check several independent items
# (the funding script's networks[] loop): note_problem records instead
# of exiting, so ONE bad network does not hide the state of the rest —
# report_problems then emits every finding at once and exits with the
# same warn/fail semantics as problem(). A single run tells the whole
# story instead of one finding per re-run.
problem_list=()

note_problem() {
  problem_list+=("$*")
}

report_problems() {
  if [ "${#problem_list[@]}" -eq 0 ]; then
    return 0
  fi
  if [ "$mode" = warn ]; then
    for entry in "${problem_list[@]}"; do
      echo "::warning::$entry"
    done
    exit 0
  fi
  for entry in "${problem_list[@]}"; do
    printf 'FAIL: %s\n' "$entry" >&2
  done
  exit 1
}
