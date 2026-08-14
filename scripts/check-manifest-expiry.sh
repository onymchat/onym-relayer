#!/usr/bin/env bash
# Operator manifest expiry tripwire: fail while there is still time to
# act. Clients REJECT expired manifests, so validUntil lapsing breaks
# every pinned client at once — re-sign (dispatch sign-manifest with a
# bumped validUntil in the src) well before the date. The relayer
# additionally refuses to BOOT on an expired manifest
# (operator_manifest::load_and_verify), which covers deploy paths that
# never pass through this CI, e.g. the onym-infra droplet.
#
# Shared by ci.yml and release.yml so the two checks cannot diverge.
#
# Usage: check-manifest-expiry.sh [--warn]
#   --warn  see manifest-check-lib.sh (pull_request mode: expiry is a
#           state of main, fixed by dispatching sign-manifest there —
#           a PR author cannot fix it, so it must not gate their PR).
#
# Needs GNU date (-d); CI runs on ubuntu-latest.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/manifest-check-lib.sh

parse_mode_flag "$@"
shift "$MODE_ARGS"
if [ "$#" -ne 0 ]; then
  echo "usage: $0 [--warn]" >&2
  exit 2
fi

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
