#!/usr/bin/env bash
# Operator manifest drift tripwire, NOT signature verification (the
# relayer verifies the signature against the operator key at boot; see
# operator_manifest::load_and_verify in src/operator_manifest.rs). This
# only catches the cheap-to-catch failure mode: someone edits
# operator-manifest.src.json and forgets to re-run the sign-manifest
# workflow, so the served (signed) bytes silently keep the OLD terms.
# Compare canonical content — signature stripped, keys sorted — and
# fail loudly on mismatch.
#
# Shared by ci.yml and release.yml so the two checks cannot diverge.
#
# Usage: check-manifest-drift.sh [--warn]
#   --warn  see manifest-check-lib.sh (pull_request mode: drift is red
#           BY DESIGN on the very PR that edits the src — sign-manifest
#           only re-aligns the signed bytes after that PR merges — so
#           on PRs this annotates instead of gating).
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
src=operator-manifest.src.json

if [ ! -f "$signed" ]; then
  echo "No $signed yet (manifest never signed) — skipping drift check."
  exit 0
fi

src_norm="$(jq -S 'del(.signature)' "$src")"
signed_norm="$(jq -S 'del(.signature)' "$signed")"
if [ "$src_norm" != "$signed_norm" ]; then
  {
    echo "DRIFT: $src no longer matches $signed (canonical content)."
    diff <(printf '%s\n' "$src_norm") <(printf '%s\n' "$signed_norm") || true
    echo 'The src was edited after the last signing run, so the served'
    echo 'bytes still carry the old terms. If the edit was accidental,'
    echo 'revert it. If it was intentional (e.g. bumping validUntil),'
    echo 'this is expected until the signing commit lands: sign-manifest'
    echo 'only runs on main, so merge first, then dispatch it — the'
    echo 'signed bytes it commits to main are what re-align src and'
    echo 'manifest-signed/.'
  } >&2
  problem "$src drifted from $signed — see the log above; dispatch sign-manifest after merging to re-align."
fi
echo "OK: $src matches $signed modulo the signature."
