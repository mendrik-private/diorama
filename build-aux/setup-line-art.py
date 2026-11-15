#!/usr/bin/env python3
"""Set up Diorama's local FLUX.2 [klein] 9B line-art model (no system changes).

Run with the host Python that has a GPU-enabled PyTorch (ROCm or CUDA). The
script is idempotent and is also embedded by Diorama for first-use setup. It:

1. creates a virtual environment with ``--system-site-packages`` (so it
   reuses that PyTorch) and installs pinned diffusers, transformers,
   accelerate and gguf into it, holding the existing torch version fixed;
2. downloads the pinned pipeline revision from Hugging Face, without its bf16
   transformer weights. The model is gated under the FLUX Non-Commercial
   License: accept it on the model page and run ``hf auth login`` first;
3. downloads the pinned Q4_K_M GGUF transformer and verifies its SHA-256;
4. records the environment's interpreter in ``line-art-runtime.conf``.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

REPOSITORY = "black-forest-labs/FLUX.2-klein-9B"
REVISION = "92196c8e11f7b6cf2b7493e037d8c5345c559216"
PINS = {
    "diffusers": "0.40.0",
    "transformers": "5.17.0",
    "accelerate": "1.15.0",
    "gguf": "0.19.0",
}
# The diffusers pipeline layout without the bf16 transformer weights, which
# the GGUF replaces; the single-file checkpoint and sample images in the
# repository are not needed either.
ALLOW_PATTERNS = [
    "model_index.json",
    "LICENSE.md",
    "scheduler/*",
    "text_encoder/*",
    "tokenizer/*",
    "transformer/config.json",
    "vae/*",
]
REVISION_MARKER = ".diorama-revision"
GGUF_REPOSITORY = "unsloth/FLUX.2-klein-9B-GGUF"
GGUF_REVISION = "fde8634245fe6b749a221c25b34672b5b8fbd079"
GGUF_FILE = "flux-2-klein-9b-Q4_K_M.gguf"
# From the Hugging Face API (LFS metadata) of GGUF_REVISION.
GGUF_SHA256 = "5489463ed96056b0bb5472abb5d1bba7055e48d574e37877acb43b407465e26f"
GGUF_SIZE = 5_909_829_920
# Written only after the file's SHA-256 matched; Diorama checks it and the
# size instead of hashing 5.9 GB on every run.
GGUF_MARKER = f"{GGUF_FILE}.diorama-sha256"

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
        import accelerate, gguf, transformers
        from diffusers import Flux2KleinPipeline, Flux2Transformer2DModel, GGUFQuantizationConfig
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


def cache_home():
    return Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")


def model_directory():
    configured = os.environ.get("DIORAMA_LINE_ART_MODEL")
    return Path(configured) if configured else cache_home() / "diorama/flux2-klein-9b"


def gguf_path():
    configured = os.environ.get("DIORAMA_LINE_ART_GGUF")
    return Path(configured) if configured else cache_home() / "diorama/flux2-klein-9b-gguf" / GGUF_FILE


def runtime_config_path():
    config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return config / "diorama/line-art-runtime.conf"


def parse_arguments(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog="The pipeline goes to $XDG_CACHE_HOME/diorama/flux2-klein-9b "
        "(or DIORAMA_LINE_ART_MODEL), the GGUF transformer to "
        f"$XDG_CACHE_HOME/diorama/flux2-klein-9b-gguf/{GGUF_FILE} "
        "(or DIORAMA_LINE_ART_GGUF); the runtime is recorded in "
        "$XDG_CONFIG_HOME/diorama/line-art-runtime.conf.",
    )
    parser.add_argument(
        "--venv",
        type=Path,
        default=cache_home() / "diorama/line-art-venv",
        help="virtual environment to create or reuse (default: %(default)s)",
    )
    parser.add_argument("--model", type=Path, default=model_directory())
    parser.add_argument("--gguf", type=Path, default=gguf_path())
    parser.add_argument("--runtime-config", type=Path, default=runtime_config_path())
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--models-only", action="store_true",
                      help="download models using this interpreter; do not change the runtime")
    mode.add_argument("--environment-only", action="store_true",
                      help="install Python dependencies and record the runtime without downloading models")
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


def ensure_model(model_dir):
    if model_complete(model_dir):
        print(f"FLUX.2 [klein] 9B revision {REVISION} already installed: {model_dir}")
    else:
        install_model(model_dir)


def install_model(model_dir):
    model_dir.mkdir(parents=True, exist_ok=True)
    # A stale marker must not vouch for a partial or different download.
    (model_dir / REVISION_MARKER).unlink(missing_ok=True)
    print(f"Downloading {REPOSITORY}@{REVISION} (about 16.5 GB) to {model_dir}", flush=True)
    download(REPOSITORY, REVISION, model_dir, ALLOW_PATTERNS)
    if not (model_dir / "model_index.json").is_file():
        raise SystemExit(f"The download finished without model_index.json in {model_dir}")
    write_atomically(model_dir / REVISION_MARKER, f"{REVISION}\n")
    print(f"Installed FLUX.2 [klein] 9B revision {REVISION}: {model_dir}")


def download(repository, revision, destination, patterns, *, force=False):
    """Run in this process so cancelling Diorama also stops the download."""
    import httpx
    from huggingface_hub import snapshot_download
    from huggingface_hub.errors import GatedRepoError, HfHubHTTPError

    try:
        snapshot_download(
            repository, revision=revision, local_dir=str(destination),
            allow_patterns=patterns, force_download=force,
        )
    except (httpx.HTTPError, OSError) as error:
        gated = isinstance(error, GatedRepoError) or (
            isinstance(error, HfHubHTTPError)
            and getattr(error.response, "status_code", None) in (401, 403)
        )
        if gated:
            raise SystemExit(
                f"{repository} requires Hugging Face access. Accept the FLUX Non-Commercial "
                f"License at https://huggingface.co/{REPOSITORY}, then run "
                f"`{Path(sys.executable).with_name('hf')} auth login` with that account "
                "and retry in Diorama."
            ) from error
        raise SystemExit(
            f"Could not download {repository}: {error}. Check the network connection "
            "and disk space, then retry in Diorama; completed files are reused."
        ) from error


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        while chunk := file.read(16 * 1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def gguf_complete(gguf):
    marker = gguf.with_name(GGUF_MARKER)
    return (
        gguf.is_file()
        and gguf.stat().st_size == GGUF_SIZE
        and marker.is_file()
        and marker.read_text(encoding="utf-8").strip() == GGUF_SHA256
    )


def ensure_gguf(gguf):
    if gguf_complete(gguf):
        print(f"{GGUF_FILE} ({GGUF_REPOSITORY}@{GGUF_REVISION}) already verified: {gguf}")
        return
    # A stale marker must not vouch for a partial or different file.
    gguf.with_name(GGUF_MARKER).unlink(missing_ok=True)
    if not (gguf.is_file() and gguf.stat().st_size == GGUF_SIZE):
        print(
            f"Downloading {GGUF_FILE} from {GGUF_REPOSITORY}@{GGUF_REVISION} (5.9 GB) "
            f"to {gguf.parent}",
            flush=True,
        )
        gguf.parent.mkdir(parents=True, exist_ok=True)
        download(GGUF_REPOSITORY, GGUF_REVISION, gguf.parent, [GGUF_FILE])
        if gguf.name != GGUF_FILE:
            (gguf.parent / GGUF_FILE).replace(gguf)
    print(f"Verifying the SHA-256 of {gguf}", flush=True)
    actual = sha256(gguf)
    if actual != GGUF_SHA256:
        # A same-sized damaged file must not require a manual cache deletion.
        download(GGUF_REPOSITORY, GGUF_REVISION, gguf.parent, [GGUF_FILE], force=True)
        downloaded = gguf.parent / GGUF_FILE
        actual = sha256(downloaded)
        if actual != GGUF_SHA256:
            raise SystemExit(
                f"{downloaded} has SHA-256 {actual}, not the pinned {GGUF_SHA256}. "
                "Retry in Diorama to download it again."
            )
        if downloaded != gguf:
            downloaded.replace(gguf)
    write_atomically(gguf.with_name(GGUF_MARKER), f"{GGUF_SHA256}\n")
    print(f"Installed {GGUF_FILE} ({GGUF_REPOSITORY}@{GGUF_REVISION}): {gguf}")


def runtime_configuration(python, library_path):
    content = "# Generated by Diorama's setup-line-art.py\n" f"python={python}\n"
    if library_path:
        content += f"library_path={library_path}\n"
    return content


def main(argv=None):
    arguments = parse_arguments(argv)
    if arguments.models_only:
        # The inference worker sets these to 1, but provisioning needs network
        # access. Change only this setup process, never Diorama's environment.
        os.environ.pop("HF_HUB_OFFLINE", None)
        os.environ.pop("TRANSFORMERS_OFFLINE", None)
        ensure_model(arguments.model)
        ensure_gguf(arguments.gguf)
        return
    python, report = ensure_environment(arguments.venv)
    report_torch(report)
    if not arguments.environment_only:
        subprocess.run(
            [str(python), str(Path(__file__).absolute()), "--models-only",
             "--model", str(arguments.model), "--gguf", str(arguments.gguf)],
            check=True,
        )
    config = arguments.runtime_config
    write_atomically(config, runtime_configuration(python, os.environ.get("LD_LIBRARY_PATH")))
    print(f"Recorded line-art runtime: {config}")


if __name__ == "__main__":
    main()
