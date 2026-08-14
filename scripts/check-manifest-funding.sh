#!/usr/bin/env bash
# Operator manifest funding tripwire: every networks[] entry declares a
# submitterAccount as fee-payer, and an account that does not EXIST on
# that network cannot pay anything — every relayed transaction there
# fails, while the manifest (whose hash clients pin, immutably) keeps
# promising it works. Ask each network's Horizon whether the account
# exists: HTTP 200 means funded/exists, 404 means it does not.
#
# This can regress independently of any PR (accounts are funded and
# merged out-of-band, and can in principle be emptied below reserve),
# which is why it runs on push to main too, not only when the manifest
# changes — and why sign-manifest.yml runs it as a hard gate BEFORE
# signing: signing publishes an immutable commitment, so a nonexistent
# fee-payer must block the signature, not surface after it.
#
# Shared by ci.yml and sign-manifest.yml so the checks cannot diverge.
#
# Usage: check-manifest-funding.sh [--warn] [<manifest.json>]
#   --warn        see manifest-check-lib.sh (pull_request mode)
#   <manifest>    file to check; defaults to the signed manifest
#                 (manifest-signed/manifest.json) when it exists, else
#                 operator-manifest.src.json.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/manifest-check-lib.sh

parse_mode_flag "$@"
shift "$MODE_ARGS"
if [ "$#" -gt 1 ]; then
  echo "usage: $0 [--warn] [<manifest.json>]" >&2
  exit 2
fi

manifest="${1:-}"
if [ -z "$manifest" ]; then
  if [ -f manifest-signed/manifest.json ]; then
    manifest="manifest-signed/manifest.json"
  else
    manifest="operator-manifest.src.json"
  fi
fi
if [ ! -f "$manifest" ]; then
  problem "no manifest at $manifest to check funding for."
fi

# SHA-256 of the network passphrases, as used in networks[].namespace.
NS_TESTNET=cee0302d59844d32bdca915c8203dd44b33fbb7edc19051ea37abedf28ecd472
NS_PUBLIC=7ac33997544e3175d266bd022439b22cdb16508c01163f26e5cb2a3e1045a979

count="$(jq '.networks | length' "$manifest")"
if [ "$count" -eq 0 ]; then
  echo "$manifest declares no networks[] entries — nothing to check."
  exit 0
fi

for i in $(seq 0 $((count - 1))); do
  entry="$(jq -c ".networks[$i]" "$manifest")"
  namespace="$(jq -r '.namespace // empty' <<<"$entry")"
  network="$(jq -r '.network // empty' <<<"$entry")"
  account="$(jq -r '.submitterAccount // empty' <<<"$entry")"
  if [ -z "$account" ]; then
    problem "networks[$i] in $manifest has no submitterAccount."
  fi
  # Resolve the Horizon for the entry — by namespace hash first (the
  # normative field), by network name as a fallback; refuse rather
  # than guess when neither is recognized.
  case "$namespace" in
    "stellar:$NS_TESTNET") horizon=https://horizon-testnet.stellar.org ;;
    "stellar:$NS_PUBLIC") horizon=https://horizon.stellar.org ;;
    *)
      case "$network" in
        testnet) horizon=https://horizon-testnet.stellar.org ;;
        public) horizon=https://horizon.stellar.org ;;
        *) problem "networks[$i] in $manifest: unrecognized namespace '$namespace' / network '$network' — cannot pick a Horizon to verify the fee-payer against." ;;
      esac
      ;;
  esac
  status="$(curl -sS -o /dev/null -w '%{http_code}' \
    --max-time 30 "$horizon/accounts/$account" || echo 000)"
  case "$status" in
    200)
      echo "OK: networks[$i] ($network) submitterAccount $account exists on $horizon."
      ;;
    404)
      problem "networks[$i] ($network) submitterAccount $account does NOT exist on $horizon (HTTP 404) — an unfunded fee-payer means every relayed transaction on that network fails. Fund the account (testnet: friendbot; public: real XLM above base reserve), or remove the entry, before the manifest is (re-)signed."
      ;;
    *)
      problem "networks[$i] ($network): Horizon $horizon returned HTTP $status for $account — cannot verify the fee-payer exists."
      ;;
  esac
done
