#!/usr/bin/env python3
"""Create a deterministic, embeddable Diorama Python runtime archive.

The input must already be a relocatable Python prefix. This tool does not turn
an arbitrary venv into one: it validates the isolated interpreter and copies
only files reachable from that prefix into the archive.
"""

import argparse
import gzip
import io
import os
from pathlib import Path, PurePosixPath
import stat
import subprocess
import sys
import tarfile
import tempfile

SCHEMA = "diorama-python-runtime-v1"
ROOT = "diorama-python-runtime"


def parser():
    command = argparse.ArgumentParser(description=__doc__)
    command.add_argument("prefix", type=Path, help="audited relocatable Python prefix")
    command.add_argument("output", type=Path, help="output .tar.gz (must not be inside prefix)")
    command.add_argument("--target", required=True, help="Cargo target triple this prefix supports")
    command.add_argument("--worker", type=Path, default=Path("src/tools/lama_worker.py"))
    command.add_argument("--skip-dependency-check", action="store_true", help="only for small packaging fixtures")
    return command.parse_args()


def within(path, root):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def require_regular(path, message):
    if not path.is_file() or path.is_symlink():
        raise RuntimeError(message)


def check_dependencies(prefix, python):
    code = r'''
import importlib, os, pathlib, sys, sysconfig
root = pathlib.Path(sys.argv[1]).resolve()
if pathlib.Path(sys.prefix).resolve() != root or pathlib.Path(sys.base_prefix).resolve() != root:
    raise SystemExit(f"interpreter prefix is not self-contained: {sys.prefix} / {sys.base_prefix}")
for location in (pathlib.Path(os.__file__).resolve(), pathlib.Path(sysconfig.get_path("stdlib")).resolve()):
    try:
        location.relative_to(root)
    except ValueError:
        raise SystemExit(f"standard library resolves outside the prefix: {location}")
for name in ("torch", "numpy", "PIL"):
    module = importlib.import_module(name)
    origin = pathlib.Path(module.__file__).resolve()
    try:
        origin.relative_to(root)
    except ValueError:
        raise SystemExit(f"{name} resolves outside the prefix: {origin}")
for entry in sys.path:
    if not entry:
        continue
    location = pathlib.Path(entry).resolve()
    try:
        location.relative_to(root)
    except ValueError:
        raise SystemExit(f"sys.path resolves outside the prefix: {location}")
'''
    environment = os.environ.copy()
    for key in ("PYTHONHOME", "PYTHONPATH", "PYTHONUSERBASE", "LD_LIBRARY_PATH", "LD_PRELOAD"):
        environment.pop(key, None)
    environment.update(PYTHONNOUSERSITE="1", PYTHONSAFEPATH="1")
    run = subprocess.run([python, "-I", "-B", "-c", code, str(prefix)], text=True, capture_output=True, env=environment)
    if run.returncode:
        detail = (run.stderr or run.stdout).strip()
        raise RuntimeError(
            "isolated interpreter cannot load torch, numpy and Pillow from this prefix"
            + (f": {detail}" if detail else "")
        )


def collect(prefix):
    """Return archive paths and dereferenced source files, rejecting escapes/special files."""
    prefix = prefix.resolve()
    files = {}
    visiting = set()

    def walk(logical, physical):
        physical = physical.resolve()
        if not within(physical, prefix):
            raise RuntimeError(f"symlink escapes Python prefix: {logical}")
        if physical in visiting:
            raise RuntimeError(f"symlink directory cycle: {logical}")
        visiting.add(physical)
        for child in sorted(physical.iterdir(), key=lambda item: item.name):
            destination = logical / child.name
            resolved = child.resolve()
            if not within(resolved, prefix):
                raise RuntimeError(f"symlink escapes Python prefix: {child}")
            mode = child.lstat().st_mode
            if stat.S_ISLNK(mode):
                if resolved.is_dir():
                    walk(destination, resolved)
                elif resolved.is_file():
                    files[destination.as_posix()] = resolved
                else:
                    raise RuntimeError(f"symlink resolves to unsupported file: {child}")
            elif stat.S_ISDIR(mode):
                walk(destination, child)
            elif stat.S_ISREG(mode):
                files[destination.as_posix()] = child
            else:
                raise RuntimeError(f"prefix contains unsupported special file: {child}")
        visiting.remove(physical)

    walk(PurePosixPath("python"), prefix)
    return files


def add_bytes(archive, name, content, mode):
    info = tarfile.TarInfo(f"{ROOT}/{name}")
    info.size = len(content)
    info.mode = mode & 0o777
    info.uid = info.gid = info.mtime = 0
    info.uname = info.gname = ""
    archive.addfile(info, io.BytesIO(content))


def add_file(archive, name, source):
    info = tarfile.TarInfo(f"{ROOT}/{name}")
    info.size = source.stat().st_size
    info.mode = source.stat().st_mode & 0o777
    info.uid = info.gid = info.mtime = 0
    info.uname = info.gname = ""
    with source.open("rb") as content:
        archive.addfile(info, content)


def main():
    args = parser()
    prefix = args.prefix.resolve()
    output = args.output.resolve()
    worker = args.worker.resolve()
    if not prefix.is_dir():
        raise RuntimeError(f"Python prefix does not exist: {prefix}")
    if within(output, prefix):
        raise RuntimeError("output archive must not be inside the Python prefix")
    require_regular(worker, f"worker is not a regular file: {worker}")
    python = prefix / "bin/python3"
    if not python.exists() or not os.access(python, os.X_OK):
        raise RuntimeError("prefix must contain executable bin/python3")
    files = collect(prefix)
    if "python/bin/python3" not in files:
        raise RuntimeError("bin/python3 must resolve to a regular file inside the prefix")
    if not args.skip_dependency_check:
        check_dependencies(prefix, str(python))
    manifest = f"schema={SCHEMA}\ntarget={args.target}\npython=python/bin/python3\n"
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{output.name}.", suffix=".tmp", dir=output.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as zipped:
            with tarfile.open(fileobj=zipped, mode="w", format=tarfile.PAX_FORMAT) as archive:
                add_bytes(archive, "manifest", manifest.encode(), 0o644)
                add_bytes(archive, "worker/lama_worker.py", worker.read_bytes(), worker.stat().st_mode)
                for name, source in sorted(files.items()):
                    add_file(archive, name, source)
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)
    print(f"Packaged {len(files)} Python files for {args.target}: {output}")


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print(f"package-python-runtime.py: {error}", file=sys.stderr)
        raise SystemExit(2)
