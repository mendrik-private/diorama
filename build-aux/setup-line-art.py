#!/usr/bin/env python3
"""Set up Diorama's local FLUX.2 [klein] 4B line-art model (no system changes).

Run with the host Python that has a GPU-enabled PyTorch (ROCm or CUDA). The
script is idempotent. It:

1. creates a virtual environment with ``--system-site-packages`` (so it
   reuses that PyTorch) and installs pinned diffusers, transformers and
   accelerate into it, holding the existing torch version fixed;
2. downloads the pinned model revision (Apache 2.0) from Hugging Face;
3. records the environment's interpreter in ``line-art-runtime.conf``.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

REPOSITORY = "black-forest-labs/FLUX.2-klein-4B"
REVISION = "e7b7dc27f91deacad38e78976d1f2b499d76a294"
PINS = {"diffusers": "0.40.0", "transformers": "5.17.0", "accelerate": "1.15.0"}
# The diffusers pipeline layout only; the single-file checkpoint and sample
# images in the repository are not needed.
ALLOW_PATTERNS = [
    "model_index.json",
    "LICENSE.md",
    "scheduler/*",
    "text_encoder/*",
    "tokenizer/*",
    "transformer/*",
    "vae/*",
]
REVISION_MARKER = ".diorama-revision"
DOWNLOAD_FAILED_EXIT = 3

PROBE = """
import importlib.metadata as metadata, json, os, sys
report = {"pins": {}, "torch": None}
for name in sys.argv[1:]:
    try:
        report["pins"][name] = metadata.version(name)
    except metadata.PackageNotFoundError:
        report["pins"][name] = None
try:
    import torch
except Exception as error:
    report["torch_error"] = repr(error)
else:
    available = torch.cuda.is_available()
    report["torch"] = {
        "version": torch.__version__,
        # pip compares the distribution's metadata version, whose local label
        # can differ from torch.__version__ (ROCm wheels add ".lw").
        "distribution": metadata.version("torch"),
        "path": os.path.dirname(torch.__file__),
        "gpu": available,
        "device": torch.cuda.get_device_name(0) if available else None,
    }
    try:
        import accelerate, transformers
        from diffusers import Flux2KleinPipeline
    except Exception as error:
        report["pipeline_error"] = repr(error)
print(json.dumps(report))
"""

# accelerate's requirements without torch: the host's ROCm torch may pin
# dependencies (such as a ROCm triton build) that are not on PyPI, so pip must
# never re-resolve torch itself.
ACCELERATE_REQUIREMENTS = """
import json
from importlib import metadata
from packaging.requirements import Requirement
requirements = []
for text in metadata.requires("accelerate") or []:
    requirement = Requirement(text)
    if requirement.name.lower() == "torch":
        continue
    if requirement.marker is not None and not requirement.marker.evaluate({"extra": ""}):
        continue
    requirements.append(str(requirement))
print(json.dumps(requirements))
"""

# Hub, HTTP, and connection errors (httpx.HTTPError covers HfHubHTTPError and
# transport failures; OSError covers local and offline-cache errors) become a
# short message instead of a traceback. Anything else is a bug and propagates.
DOWNLOAD = """
import sys
import httpx
from huggingface_hub import snapshot_download
repository, revision, destination = sys.argv[1:4]
try:
    snapshot_download(
        repository,
        revision=revision,
        local_dir=destination,
        allow_patterns=sys.argv[4:],
    )
except (httpx.HTTPError, OSError) as error:
    print(f"{type(error).__name__}: {error}", file=sys.stderr)
    sys.exit(%d)
""" % DOWNLOAD_FAILED_EXIT


def cache_home():
    return Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")


def model_directory():
    configured = os.environ.get("DIORAMA_LINE_ART_MODEL")
    return Path(configured) if configured else cache_home() / "diorama/flux2-klein-4b"


def runtime_config_path():
    config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return config / "diorama/line-art-runtime.conf"


def parse_arguments(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog="The model goes to $XDG_CACHE_HOME/diorama/flux2-klein-4b "
        "(or DIORAMA_LINE_ART_MODEL); the runtime is recorded in "
        "$XDG_CONFIG_HOME/diorama/line-art-runtime.conf.",
    )
    parser.add_argument(
        "--venv",
        type=Path,
        default=cache_home() / "diorama/line-art-venv",
        help="virtual environment to create or reuse (default: %(default)s)",
    )
    return parser.parse_args(argv)


def write_atomically(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", encoding="utf-8", dir=path.parent, prefix=f".{path.name}.", delete=False
    ) as target:
        temporary = Path(target.name)
        target.write(content)
    try:
        temporary.replace(path)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def venv_python(venv):
    # Keep the venv path, not its resolved symlink target: the target is the
    # base interpreter, which lacks the packages installed here.
    return Path(os.path.abspath(venv)) / "bin" / "python"


def probe(python):
    result = subprocess.run(
        [str(python), "-c", PROBE, *PINS], check=True, capture_output=True, text=True
    )
    return json.loads(result.stdout)


def pins_satisfied(report):
    return (
        report["torch"] is not None
        and "pipeline_error" not in report
        and all(report["pins"].get(name) == version for name, version in PINS.items())
    )


def pip_install(python, constraints, *arguments):
    subprocess.run(
        [
            str(python), "-m", "pip", "install", "--disable-pip-version-check",
            "--constraint", str(constraints), *arguments,
        ],
        check=True,
    )


def install_pins(python, torch_distribution):
    """Install the pins while holding the host's torch distribution fixed.

    diffusers and transformers need torch only through extras. accelerate
    requires it directly, so it goes in without dependencies and its other
    requirements are installed separately; otherwise pip would re-resolve
    the host torch's own requirements.
    """
    with tempfile.TemporaryDirectory() as directory:
        constraints = Path(directory) / "constraints.txt"
        constraints.write_text(f"torch=={torch_distribution}\n", encoding="utf-8")
        pip_install(
            python, constraints,
            *(f"{name}=={version}" for name, version in PINS.items() if name != "accelerate"),
        )
        pip_install(python, constraints, "--no-deps", f"accelerate=={PINS['accelerate']}")
        requirements = json.loads(
            subprocess.run(
                [str(python), "-c", ACCELERATE_REQUIREMENTS],
                check=True, capture_output=True, text=True,
            ).stdout
        )
        if requirements:
            pip_install(python, constraints, *requirements)


def own_torch(venv, report):
    """A torch inside the environment shadows the host's GPU build."""
    torch = report["torch"]
    return torch is not None and Path(torch["path"]).is_relative_to(Path(os.path.abspath(venv)))


def ensure_environment(venv):
    python = venv_python(venv)
    if python.exists():
        report = probe(python)
        if own_torch(venv, report):
            raise SystemExit(
                f"{venv} contains its own PyTorch {report['torch']['version']}, which shadows "
                f"the host's GPU build. Remove {venv} and rerun this script."
            )
        if pins_satisfied(report):
            print(f"Line-art environment already satisfies the pins: {venv}")
            return python, report
    else:
        print(f"Creating line-art environment: {venv}", flush=True)
        subprocess.run(
            [sys.executable, "-m", "venv", "--system-site-packages", str(venv)], check=True
        )
        report = probe(python)
    if report["torch"] is None:
        raise SystemExit(
            f"PyTorch is not importable from {python} ({report.get('torch_error')}). "
            "Rerun this script with the host Python that has a GPU-enabled PyTorch."
        )
    torch = report["torch"]
    # Hold the host's torch distribution fixed so pip cannot replace a ROCm
    # build with a generic wheel inside the environment.
    install_pins(python, torch["distribution"])
    report = probe(python)
    if not pins_satisfied(report) or own_torch(venv, report) or report["torch"]["path"] != torch["path"]:
        raise SystemExit(f"The line-art environment at {venv} does not match the pins: {report}")
    return python, report


def report_torch(report):
    torch = report["torch"]
    if torch["gpu"]:
        print(f"PyTorch {torch['version']} sees GPU: {torch['device']}")
    else:
        print(
            f"Warning: PyTorch {torch['version']} sees no CUDA/ROCm GPU. Diorama refuses to run "
            "line art on the CPU unless DIORAMA_LINE_ART_DEVICE=cpu is set (very slow)."
        )


def model_complete(model_dir):
    marker = model_dir / REVISION_MARKER
    return (
        (model_dir / "model_index.json").is_file()
        and marker.is_file()
        and marker.read_text(encoding="utf-8").strip() == REVISION
    )


def ensure_model(python, model_dir):
    if model_complete(model_dir):
        print(f"FLUX.2 [klein] 4B revision {REVISION} already installed: {model_dir}")
        return
    model_dir.mkdir(parents=True, exist_ok=True)
    # A stale marker must not vouch for a partial or different download.
    (model_dir / REVISION_MARKER).unlink(missing_ok=True)
    print(f"Downloading {REPOSITORY}@{REVISION} (about 16 GB) to {model_dir}", flush=True)
    result = subprocess.run(
        [str(python), "-c", DOWNLOAD, REPOSITORY, REVISION, str(model_dir), *ALLOW_PATTERNS]
    )
    if result.returncode == DOWNLOAD_FAILED_EXIT:
        raise SystemExit(
            f"Could not download {REPOSITORY} from Hugging Face (see the error above). "
            "Check the network connection and disk space, then rerun this script; "
            "completed files are not downloaded again."
        )
    if result.returncode != 0:
        raise SystemExit("The model download failed; rerun this script to resume it.")
    if not (model_dir / "model_index.json").is_file():
        raise SystemExit(f"The download finished without model_index.json in {model_dir}")
    write_atomically(model_dir / REVISION_MARKER, f"{REVISION}\n")
    print(f"Installed FLUX.2 [klein] 4B revision {REVISION}: {model_dir}")


def runtime_configuration(python, library_path):
    content = "# Generated by Diorama's setup-line-art.py\n" f"python={python}\n"
    if library_path:
        content += f"library_path={library_path}\n"
    return content


def main(argv=None):
    arguments = parse_arguments(argv)
    python, report = ensure_environment(arguments.venv)
    report_torch(report)
    ensure_model(python, model_directory())
    config = runtime_config_path()
    write_atomically(config, runtime_configuration(python, os.environ.get("LD_LIBRARY_PATH")))
    print(f"Recorded line-art runtime: {config}")


if __name__ == "__main__":
    main()
