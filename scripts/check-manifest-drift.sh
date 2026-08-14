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
# Takes no arguments; exits nonzero on drift.
set -euo pipefail
cd "$(dirname "$0")/.."

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
    echo 'this check stays red on the PR by design and goes green after'
    echo 'the signing commit lands: sign-manifest only runs on main, so'
    echo 'merge first, then dispatch it — the signed bytes it commits to'
    echo 'main are what re-align src and manifest-signed/.'
  } >&2
  exit 1
fi
echo "OK: $src matches $signed modulo the signature."
