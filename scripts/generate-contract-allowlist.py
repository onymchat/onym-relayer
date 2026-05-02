#!/usr/bin/env python3
"""Generate the relayer contract allowlist from onym-contracts releases."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.request
from typing import Any


CONTRACT_TYPES = ["anarchy", "oneonone", "democracy", "oligarchy", "tyranny"]
NETWORKS = ["testnet", "public"]
CONTRACT_RE = re.compile(
    r"\|\s*`sep-(anarchy|oneonone|democracy|oligarchy|tyranny)`\s*"
    r"\|\s*\[.*?\]\(https://stellar\.expert/explorer/([^/\)]+)/contract/(C[A-Z0-9]+)\)",
    re.IGNORECASE,
)
NETWORK_RE = re.compile(r"\*\*Network:\*\*\s*([A-Za-z0-9_-]+)", re.IGNORECASE)


def normalize_network(raw: str) -> str:
    value = raw.strip().lower().replace("-", "").replace("_", "")
    if value in {"testnet", "test"}:
        return "testnet"
    if value in {"public", "mainnet", "pubnet"}:
        return "public"
    raise ValueError(f"unknown network: {raw}")


def fetch_releases(repo: str) -> list[dict[str, Any]]:
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    releases: list[dict[str, Any]] = []
    page = 1

    while True:
        request = urllib.request.Request(
            f"https://api.github.com/repos/{repo}/releases?per_page=100&page={page}",
            headers={
                "Accept": "application/vnd.github+json",
                "User-Agent": "onym-relayer-allowlist-generator",
            },
        )
        if token:
            request.add_header("Authorization", f"Bearer {token}")
        with urllib.request.urlopen(request, timeout=30) as response:
            batch = json.load(response)

        if not batch:
            break
        releases.extend(batch)
        if len(batch) < 100:
            break
        page += 1

    return releases


def load_releases(args: argparse.Namespace) -> list[dict[str, Any]]:
    if args.input:
        with open(args.input, "r", encoding="utf-8") as handle:
            return json.load(handle)
    return fetch_releases(args.repo)


def extract_network(release: dict[str, Any]) -> str | None:
    body = release.get("body") or ""
    match = NETWORK_RE.search(body)
    if not match:
        return None
    return normalize_network(match.group(1))


def generate_allowlist(releases: list[dict[str, Any]]) -> dict[str, dict[str, list[str]]]:
    found: dict[str, dict[str, set[str]]] = {
        network: {contract_type: set() for contract_type in CONTRACT_TYPES}
        for network in NETWORKS
    }

    for release in releases:
        if release.get("draft"):
            continue
        body = release.get("body") or ""
        release_network = extract_network(release)

        for contract_type, url_network, contract_id in CONTRACT_RE.findall(body):
            url_network = normalize_network(url_network)
            if release_network and url_network != release_network:
                raise ValueError(
                    f"{release.get('tag_name', '<unknown>')} mixes body network "
                    f"{release_network} with contract URL network {url_network}"
                )
            found[url_network][contract_type.lower()].add(contract_id)

    if not any(ids for contracts in found.values() for ids in contracts.values()):
        raise ValueError("no deployed contract addresses found in onym-contracts releases")

    return {
        network: {
            contract_type: sorted(found[network][contract_type])
            for contract_type in CONTRACT_TYPES
        }
        for network in NETWORKS
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default="onymchat/onym-contracts")
    parser.add_argument("--input", help="Read GitHub releases JSON from a file instead of the API")
    parser.add_argument("--output", required=True)
    parser.add_argument("--compact", action="store_true")
    args = parser.parse_args()

    allowlist = generate_allowlist(load_releases(args))
    with open(args.output, "w", encoding="utf-8") as handle:
        if args.compact:
            json.dump(allowlist, handle, separators=(",", ":"))
        else:
            json.dump(allowlist, handle, indent=2, sort_keys=True)
        handle.write("\n")

    counts = {
        network: sum(len(ids) for ids in contracts.values())
        for network, contracts in allowlist.items()
    }
    print(f"generated contract allowlist: {counts}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
