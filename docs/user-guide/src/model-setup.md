# Set up local models

Diorama keeps ordinary viewing, crop, rotate, flip, and Nearest/Bicubic/Lanczos scaling independent of AI models. Three separate local components support the features that need inference. Install only the component required by the feature you intend to use.

| Component | Used for | What Diorama needs |
| --- | --- | --- |
| **BiRefNet through vision.cpp** | Foreground cutouts and background-removal selections; the mask used during Game Asset generation. | A host `vision-cli` executable and a `BiRefNet-F16.gguf` model. |
| **LaMa** | Filling the background behind a moved or deleted extracted selection. | Python with `torch`, `numpy`, and `Pillow`, plus a TorchScript LaMa model. |
| **FLUX.2 [klein] 9B** | Game Asset line-art and fill generation. | GPU-enabled Python/PyTorch, the pinned FLUX pipeline, and its Q4_K_M GGUF transformer. |

These are different jobs. Installing LaMa does not configure Game Asset; installing FLUX does not supply the selection cutout runtime. All work locally after its model files are installed. The setup commands below are for a clone of Diorama: run them from its top-level directory unless a step says otherwise.

## BiRefNet: foreground cutouts and background removal

The currently shipped selection tool invokes [vision.cpp](https://github.com/Acly/vision.cpp)'s `vision-cli` on the host. Diorama does not download or bundle this runtime at launch. Its default locations are:

```
~/vision.cpp/build/bin/vision-cli
~/vision.cpp/models/BiRefNet-F16.gguf
```

Use those locations to avoid configuration. From your home directory, vision.cpp documents this build sequence:

```sh
cd ~
git clone https://github.com/Acly/vision.cpp.git --recursive
cd vision.cpp
cmake . -B build
cmake --build build --config Release
```

That build supports the `cpu` backend. To use Diorama's default `gpu` backend, vision.cpp documents a Vulkan build; install the Vulkan SDK first, then configure it with `cmake . -B build -D VISP_VULKAN=ON` before building. If you keep the CPU build, set `ASSET_SCALER_BIREFNET_BACKEND=cpu`.

Obtain `BiRefNet-F16.gguf` from vision.cpp's [BiRefNet model download](https://huggingface.co/Acly/BiRefNet-GGUF/tree/main) and place it in `~/vision.cpp/models/`. The vision.cpp documentation also describes release packages if you prefer not to build it.

If the executable or model is elsewhere, set exact paths before starting Diorama:

```sh
export ASSET_SCALER_VISION_CLI=/absolute/path/to/vision-cli
export ASSET_SCALER_BIREFNET_MODEL=/absolute/path/to/BiRefNet-F16.gguf
export ASSET_SCALER_BIREFNET_BACKEND=gpu
```

The backend must be `gpu` or `cpu`; `gpu` is the default. Diorama waits up to ten minutes for this cutout and includes the worker's log tail in a failure message. A missing executable or model produces a message asking for `ASSET_SCALER_VISION_CLI` and `ASSET_SCALER_BIREFNET_MODEL`.

In a Flatpak build the command is launched on the host, so those paths must exist there. A shell `export` on the host is not automatically an environment variable inside the sandbox. Use the normal `~/vision.cpp/...` paths, or pass overrides when launching the Flatpak:

```sh
flatpak run \
  --env=ASSET_SCALER_VISION_CLI=/absolute/path/to/vision-cli \
  --env=ASSET_SCALER_BIREFNET_MODEL=/absolute/path/to/BiRefNet-F16.gguf \
  --env=ASSET_SCALER_BIREFNET_BACKEND=gpu \
  io.github.mendrik_private.Diorama
```

## LaMa: repair a background after moving or deleting a selection

From the Diorama clone, run:

```sh
cd /path/to/diorama
python3 build-aux/setup-lama.py
```

Run it with a Python that has `torch`, `numpy`, and `Pillow`. It downloads and verifies the roughly 200 MB TorchScript model, then records the Python executable in `~/.config/diorama/lama-runtime.conf`. The normal model path is `$XDG_CACHE_HOME/diorama/big-lama.pt`, or `~/.cache/diorama/big-lama.pt` when `XDG_CACHE_HOME` is unset. In an unbundled Flatpak, Diorama can reuse the host interpreter and host model rather than download a second copy.

Use these overrides only when you intentionally maintain a separate runtime or trusted model:

```sh
export DIORAMA_LAMA_PYTHON=/absolute/path/to/python
export DIORAMA_LAMA_MODEL=/absolute/path/to/big-lama.pt
export DIORAMA_LAMA_DEVICE=cuda
```

LaMa defaults to CPU inference. `DIORAMA_LAMA_DEVICE=cuda` requests a supported PyTorch GPU. If its model is missing, Diorama names `python3 build-aux/setup-lama.py` and `DIORAMA_LAMA_MODEL` in its error. LaMa repairs the background; it does not create the foreground mask, which is BiRefNet's job.

## FLUX.2 [klein]: Game Asset scaling

Game Asset requires more storage and a capable GPU than the other local tools. It uses [FLUX.2 [klein] 9B](https://huggingface.co/black-forest-labs/FLUX.2-klein-9B) under its non-commercial licence, plus a 5.9 GB Q4_K_M GGUF transformer. First accept the licence on the FLUX model page. Then use the host Python with GPU-enabled PyTorch for ROCm or CUDA and run:

```sh
cd /path/to/diorama
python3 build-aux/setup-line-art.py
~/.cache/diorama/line-art-venv/bin/hf auth login
python3 build-aux/setup-line-art.py
```

The first run creates `~/.cache/diorama/line-art-venv` with system site packages. It reuses host PyTorch and installs pinned diffusers, transformers, accelerate, and gguf without replacing that PyTorch. Authenticate with the same Hugging Face account that accepted the gated FLUX licence, then rerun setup. The process is idempotent: rerunning it resumes incomplete downloads and keeps completed files.

The pipeline, about 16.5 GB, lives at `$XDG_CACHE_HOME/diorama/flux2-klein-9b`; the transformer lives at `$XDG_CACHE_HOME/diorama/flux2-klein-9b-gguf/flux-2-klein-9b-Q4_K_M.gguf`. Together they need about 22 GB. Setup verifies the GGUF hash and writes `~/.config/diorama/line-art-runtime.conf`. Diorama validates the setup revision marker, GGUF size, and hash marker before it runs; its worker never downloads models itself.

To use deliberately managed locations, set these before starting Diorama:

```sh
export DIORAMA_LINE_ART_PYTHON=/absolute/path/to/python
export DIORAMA_LINE_ART_MODEL=/absolute/path/to/flux2-klein-9b
export DIORAMA_LINE_ART_GGUF=/absolute/path/to/flux-2-klein-9b-Q4_K_M.gguf
```

Diorama requires a GPU by default. Set `DIORAMA_LINE_ART_DEVICE=cpu` only when you accept very slow CPU inference. A 512-square generation needs about 7.3 GB of GPU memory on the documented configuration, more for larger images; prompt encoding also needs about 13 GB of system RAM. If the worker reports out of memory, close other GPU-heavy applications and retry. A gated-download message means the licence has not been accepted or the account is not authenticated; rerun authentication and setup rather than placing incomplete files by hand.

For the feature's size limits, previews, cancellation, and quality trade-offs, see [Game Asset scaling](game-asset-scaling.md).
