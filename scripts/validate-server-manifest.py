#!/usr/bin/env python3
"""Validate a server-directory manifest (nostr-relays.json /
blossom-servers.json) published by release.yml.

Both feeds share the shape `{ "version": 1, "<key>": [ {name,url,isDefault} ] }`
and differ only in the array key and the allowed URL schemes:

  nostr-relays.json   --key relays  --schemes wss,ws
  blossom-servers.json --key servers --schemes https,http

(The chain relayer manifest has its own validator,
`validate-relayers-manifest.py`, because its items carry `networks`.)
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from urllib.parse import urlparse


def die(message: str) -> None:
    print(f"server manifest error: {message}", file=sys.stderr)
    raise SystemExit(1)


def validate_url(value: object, path: str, schemes: set[str]) -> str:
    if not isinstance(value, str) or not value.strip():
        die(f"{path} must be a non-empty string")
    url = value.strip()
    parsed = urlparse(url)
    if parsed.scheme.lower() not in schemes or not parsed.netloc:
        die(f"{path} must use {' or '.join(sorted(schemes))}:// with a host: {url}")
    if parsed.params or parsed.query or parsed.fragment:
        die(f"{path} must not include params, query, or fragment: {url}")
    return url


def validate_manifest(path: Path, key: str, schemes: set[str]) -> dict:
    with path.open(encoding="utf-8") as handle:
        manifest = json.load(handle)

    if not isinstance(manifest, dict):
        die("manifest root must be an object")
    if manifest.get("version") != 1:
        die("version must be 1")
    items = manifest.get(key)
    if not isinstance(items, list) or not items:
        die(f"{key} must be a non-empty array")

    seen_urls: set[str] = set()
    for index, item in enumerate(items):
        prefix = f"{key}[{index}]"
        if not isinstance(item, dict):
            die(f"{prefix} must be an object")
        allowed = {"name", "url", "isDefault"}
        extra = sorted(set(item) - allowed)
        if extra:
            die(f"{prefix} has unsupported keys: {', '.join(extra)}")
        name = item.get("name")
        if not isinstance(name, str) or not name.strip():
            die(f"{prefix}.name must be a non-empty string")
        url = validate_url(item.get("url"), f"{prefix}.url", schemes)
        if url in seen_urls:
            die(f"duplicate url: {url}")
        seen_urls.add(url)
        if "isDefault" in item and not isinstance(item["isDefault"], bool):
            die(f"{prefix}.isDefault must be a boolean")

    if sum(1 for i in items if i.get("isDefault")) > 1:
        die(f"at most one {key[:-1]} may be marked isDefault")

    return manifest


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--key", required=True, help="array key, e.g. relays / servers")
    parser.add_argument(
        "--schemes",
        required=True,
        help="comma-separated allowed URL schemes, e.g. wss,ws or https",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    schemes = {s.strip().lower() for s in args.schemes.split(",") if s.strip()}
    validate_manifest(args.manifest, args.key, schemes)
    print(f"validated server manifest: {args.manifest}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
