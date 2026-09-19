#!/usr/bin/env python3
"""Fetch the pinned release-only BiRefNet input and create its bundle."""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
BIREFNET_URL = "https://huggingface.co/Acly/BiRefNet-GGUF/resolve/main/BiRefNet-F16.gguf"
BIREFNET_SHA256 = "5d5fd824c8fb2c1a65fc4345458b2e78777d949418385ea7bba5a9f104364d77"


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def download(url: str, destination: Path, expected: str) -> None:
    with urllib.request.urlopen(url, timeout=120) as response, destination.open("xb") as target:
        while block := response.read(1024 * 1024):
            target.write(block)
    actual = digest(destination)
    if actual != expected:
        raise SystemExit(f"download checksum mismatch for {url}: {actual}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dest", required=True, type=Path)
    args = parser.parse_args()
    if args.dest.exists():
        raise SystemExit(f"refusing to overwrite existing bundle: {args.dest}")

    with tempfile.TemporaryDirectory(prefix="diorama-native-model-release-") as directory:
        model = Path(directory) / "BiRefNet-F16.gguf"
        download(BIREFNET_URL, model, BIREFNET_SHA256)
        subprocess.run(
            [
                sys.executable,
                ROOT / "build-aux/prepare-native-model-bundle.py",
                "--birefnet",
                model,
                "--dest",
                args.dest,
            ],
            check=True,
        )


if __name__ == "__main__":
    main()
