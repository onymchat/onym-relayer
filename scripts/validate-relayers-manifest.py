#!/usr/bin/env python3
"""Validate the relayers.json registry published by release.yml."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from urllib.parse import urlparse


SUPPORTED_NETWORKS = {"testnet", "public"}


def die(message: str) -> None:
    print(f"relayers manifest error: {message}", file=sys.stderr)
    raise SystemExit(1)


def validate_url(value: object, path: str) -> str:
    if not isinstance(value, str) or not value.strip():
        die(f"{path} must be a non-empty string")
    url = value.strip()
    parsed = urlparse(url)
    if parsed.scheme != "https" or not parsed.netloc or parsed.path not in ("", "/"):
        die(f"{path} must be an https origin URL without a path: {url}")
    if parsed.params or parsed.query or parsed.fragment:
        die(f"{path} must not include params, query, or fragment: {url}")
    return url.rstrip("/")


def validate_networks(value: object, path: str) -> list[str]:
    if not isinstance(value, list) or not value:
        die(f"{path} must be a non-empty array")
    networks: list[str] = []
    for index, network in enumerate(value):
        if not isinstance(network, str):
            die(f"{path}[{index}] must be a string")
        normalized = network.strip().lower()
        if normalized not in SUPPORTED_NETWORKS:
            die(f"{path}[{index}] has unsupported network: {network}")
        networks.append(normalized)
    if len(set(networks)) != len(networks):
        die(f"{path} must not contain duplicate networks")
    return networks


def validate_manifest(path: Path) -> dict:
    with path.open(encoding="utf-8") as handle:
        manifest = json.load(handle)

    if not isinstance(manifest, dict):
        die("manifest root must be an object")
    if manifest.get("version") != 1:
        die("version must be 1")
    relayers = manifest.get("relayers")
    if not isinstance(relayers, list) or not relayers:
        die("relayers must be a non-empty array")

    seen_urls: set[str] = set()
    for index, relayer in enumerate(relayers):
        path_prefix = f"relayers[{index}]"
        if not isinstance(relayer, dict):
            die(f"{path_prefix} must be an object")
        allowed = {"name", "url", "networks"}
        extra = sorted(set(relayer) - allowed)
        if extra:
            die(f"{path_prefix} has unsupported keys: {', '.join(extra)}")
        name = relayer.get("name")
        if not isinstance(name, str) or not name.strip():
            die(f"{path_prefix}.name must be a non-empty string")
        url = validate_url(relayer.get("url"), f"{path_prefix}.url")
        if url in seen_urls:
            die(f"duplicate relayer url: {url}")
        seen_urls.add(url)
        validate_networks(relayer.get("networks"), f"{path_prefix}.networks")

    return manifest


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    validate_manifest(args.manifest)
    print(f"validated relayers manifest: {args.manifest}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
