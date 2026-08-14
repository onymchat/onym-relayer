#!/usr/bin/env bash
# Operator manifest expiry tripwire: fail while there is still time to
# act. Clients REJECT expired manifests, so validUntil lapsing breaks
# every pinned client at once — re-sign (dispatch sign-manifest with a
# bumped validUntil in the src) well before the date.
#
# Shared by ci.yml and release.yml so the two checks cannot diverge.
#
# Usage: check-manifest-expiry.sh [--warn]
#   --warn  report problems as a GitHub ::warning:: annotation and exit
#           0. Used on pull_request runs: an expiring manifest is a
#           state of main, fixed by dispatching sign-manifest there —
#           a PR author cannot fix it, so it must not gate their PR.
#
# Needs GNU date (-d); CI runs on ubuntu-latest.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=fail
if [ "${1:-}" = "--warn" ]; then
  mode=warn
  shift
fi
if [ "$#" -ne 0 ]; then
  echo "usage: $0 [--warn]" >&2
  exit 2
fi

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

signed=manifest-signed/manifest.json
if [ ! -f "$signed" ]; then
  echo "No $signed yet (manifest never signed) — skipping validUntil check."
  exit 0
fi

valid_until="$(jq -r '.validUntil // empty' "$signed")"
if [ -z "$valid_until" ]; then
  problem "$signed has no validUntil field."
fi
until_s="$(date -u -d "$valid_until" +%s)" \
  || problem "cannot parse validUntil '$valid_until'."
now_s="$(date -u +%s)"
thirty_days=$((30 * 24 * 60 * 60))
if [ "$until_s" -le "$now_s" ]; then
  problem "signed manifest EXPIRED at $valid_until. Clients reject" \
    'expired manifests — every pinned client is broken. Bump validUntil' \
    'in operator-manifest.src.json and dispatch sign-manifest immediately.'
fi
if [ $((until_s - now_s)) -lt "$thirty_days" ]; then
  problem "signed manifest validUntil ($valid_until) is within 30 days." \
    'Re-sign BEFORE expiry: bump validUntil in operator-manifest.src.json' \
    'and dispatch sign-manifest — clients reject expired manifests.'
fi
echo "OK: validUntil $valid_until is more than 30 days out."
