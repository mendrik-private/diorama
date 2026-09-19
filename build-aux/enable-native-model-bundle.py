#!/usr/bin/env python3
"""Enable a generated native model bundle for one Flatpak release build."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("bundle_path")
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    modules = manifest.get("modules")
    if not isinstance(modules, list):
        raise SystemExit("Flatpak manifest has no module list")
    diorama = next((module for module in modules if module.get("name") == "diorama"), None)
    if not isinstance(diorama, dict):
        raise SystemExit("Flatpak manifest has no diorama module")
    options = diorama.setdefault("config-opts", [])
    if not isinstance(options, list):
        raise SystemExit("Flatpak diorama module config-opts is not a list")
    option = f"-Dnative_model_bundle={args.bundle_path}"
    if option not in options:
        options.append(option)
    sources = diorama.get("sources")
    if not isinstance(sources, list):
        raise SystemExit("Flatpak diorama module has no source list")
    source_root = next(
        (source for source in sources if isinstance(source, dict) and source.get("path") == ".."),
        None,
    )
    if not isinstance(source_root, dict):
        raise SystemExit("Flatpak diorama module has no repository directory source")
    skipped = source_root.setdefault("skip", [])
    if not isinstance(skipped, list):
        raise SystemExit("Flatpak repository source skip is not a list")
    if "release-models" not in skipped:
        skipped.append("release-models")
    bundle_source = {
        "type": "dir",
        "path": "../release-models",
        "dest": "release-models",
    }
    if bundle_source not in sources:
        sources.append(bundle_source)
    args.manifest.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
