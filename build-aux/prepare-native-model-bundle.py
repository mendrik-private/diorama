#!/usr/bin/env python3
"""Create a verified, offline Diorama BiRefNet model bundle.

Release automation supplies the audited GGUF. This builder never downloads a
model and refuses to overwrite an existing bundle directory.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
from pathlib import Path


BIREFNET_FILENAME = "BiRefNet-F16.gguf"
BIREFNET_SHA256 = "5d5fd824c8fb2c1a65fc4345458b2e78777d949418385ea7bba5a9f104364d77"


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def checked(path: Path, expected: str) -> str:
    if not path.is_file():
        raise SystemExit(f"missing model artifact: {path}")
    actual = digest(path)
    if actual != expected:
        raise SystemExit(f"unexpected SHA-256 for {path}: {actual}")
    return actual


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--birefnet", required=True, type=Path)
    parser.add_argument("--dest", required=True, type=Path)
    args = parser.parse_args()

    if args.dest.exists():
        raise SystemExit(f"refusing to overwrite existing bundle: {args.dest}")
    source_hash = checked(args.birefnet, BIREFNET_SHA256)

    args.dest.mkdir(parents=True)
    output = args.dest / BIREFNET_FILENAME
    shutil.copyfile(args.birefnet, output)
    output_hash = checked(output, BIREFNET_SHA256)
    manifest = {
        "format": "diorama-native-model-bundle-v1",
        "models": {
            "birefnet": {
                "file": output.name,
                "sha256": output_hash,
                "source_code_license": "MIT",
                "source_sha256": source_hash,
                "preprocessing": "vision.cpp-compatible RGB ImageNet normalization and alpha output",
            }
        },
    }
    manifest_path = args.dest / "manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    loaded = json.loads(manifest_path.read_text(encoding="utf-8"))
    if loaded != manifest:
        raise SystemExit(f"failed to write a checked manifest: {manifest_path}")


if __name__ == "__main__":
    main()
