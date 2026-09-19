# Native cutout decision

**Status:** native BiRefNet cutout; legacy TorchScript LaMa fill (2026-09-19).

## Existing behaviour and constraints

The previous `src/tools/selection.rs` wrote the selected RGBA fragment to a
temporary PNG and launched `vision-cli birefnet`. The inspected GGUF artifact
is 440,372,864 bytes. The delivered path runs that same weight in Diorama's
native worker. Its mask is returned at the fragment's original dimensions and
is multiplied into the source alpha. Selection preparation remains cancellable,
bounded to one inference job, and compatible with Flatpak.

The legacy `big-lama.pt` artifact is 205,669,692 bytes. Its inspected worker sent a
float32 RGB NCHW tensor and binary N1HW removal matte to TorchScript; it scales
the context to at most 1024 pixels on its long edge, pads the bottom/right
symmetrically to a multiple of 8, then rescales the RGB result. Rust already
dilates the model matte by two pixels and copies only the selected original
pixels from the result. These preprocessing, padding, matte polarity and merge
rules are parity invariants.

Content-aware fill remains the checked legacy Python/TorchScript worker. It is
the current quality baseline and is configured by `build-aux/setup-lama.py`.

## What is viable

| Route | Native Rust / portable GPU | Model-quality evidence | Shipping consequence | Decision |
|---|---|---|---|---|
| Existing BiRefNet-Burn | Burn source, WGPU backend can use the same graphics abstraction as the app | Architecture is a BiRefNet reimplementation, but upstream marks optimization/production tuning incomplete and has no benchmarks | Must pin/fork and preconvert weights in maintainer tooling; do not download/convert at user runtime | Use only as an implementation reference. |
| CubeK GPU FFT for LaMa | CubeCL/CubeK exposes GPU FFT primitives | The public real FFT path requires power-of-two transform axes | LaMa requires exact non-power-of-two axes such as 120×120 | Reject for now; retain the verified TorchScript worker. |
| Smaller image model or tiny LLM/VLM runner | Unknown | No paired quality metric against the current LaMa workflow | New model selection and quality-evaluation work | Do not switch on speculation. |

## Locked architecture

Ship a native Rust inference subsystem, split by task but sharing image/tensor
conversion, cancellation boundaries, model discovery, and backend ownership:

1. **Cutout:** hand-port the compact inference-only BiRefNet/Swin path from the
   locally inspected `vision.cpp`, retaining the exact existing GGUF weights.
   This avoids an unvalidated alpha dependency and avoids substituting a
   different model/backbone. The GGUF parser and compute path are Rust/Burn;
   the app performs no Python conversion or model download at runtime.
2. **Fill:** retain the current TorchScript LaMa worker. Its preprocessing,
   padding, matte polarity and merge rules remain the quality invariants.
3. **Runtime backends:** pin Burn 0.20.1, compatible with Diorama's Rust 1.92
   MSRV, for native BiRefNet CPU and WGPU execution.

The native cutout worker runs as a mode of Diorama's own Rust executable. This
process boundary retains hard cancellation and timeout guarantees. The LaMa
worker remains a separate Python process with the same cancellation behavior.

BiRefNet-Burn is alpha software: its README declares
incomplete post-processing and missing production optimization/benchmarks.
That is why Diorama owns an adapter and validates it against current output,
rather than treating the dependency as a proven drop-in library.

## Delivered validation

BiRefNet uses the original GGUF weights and has retained model-free parser,
layout, roll, raw-NHWC bilinear-view, and STB-resize regressions. Against
vision.cpp, its 1024×1024 elf fixture measured alpha MAE 0.0681/255 and binary
IoU 0.999612; the 1024×1024 selection fixture measured alpha MAE 0.2457/255
and IoU 0.998450. The 1280×800 selection exercised arbitrary-size resizing on
CPU: MAE 0.3715/255, maximum difference 38, and IoU 0.996714. Native input
resampling differed from the STB reference by at most one 8-bit channel value
(mean 0.00229/255), and output mask resizing is checked against STB vectors
with a 1e-5 floating-point tolerance. Independent visual review accepted the
soft masks. Exact-size GPU runs passed; later GPU retries reported WGPU
allocation failures in this test environment.

The app supervisor has focused tests for cancellation before/during a worker,
queued shared-permit cancellation, timeouts and reaping, bounded logs,
dimension validation, and one GPU-to-CPU retry sharing the original deadline.

## Remaining measured limitation

* Runtime validation covers Linux CPU and WGPU; Metal and DX12 remain
  untested.

## Primary sources

* [Original BiRefNet repository and its Rust/GGUF extension links](https://github.com/ZhengPeng7/BiRefNet)
* [BiRefNet-Burn README and declared limitations](https://github.com/nusu-github/BiRefNet-Burn)
* [Locally inspected vision.cpp documentation](https://github.com/Acly/vision.cpp)
