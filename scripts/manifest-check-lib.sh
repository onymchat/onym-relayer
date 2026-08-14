# Shared warn/fail plumbing for the operator-manifest tripwire scripts
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
