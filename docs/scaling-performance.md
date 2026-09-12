# Scaling performance

Measured on 2026-09-12 with Rust 1.94.1, x86_64 Linux, AMD Ryzen AI MAX+ PRO
395, the default Cargo features and the repository's release profile
(`opt-level=3`, thin LTO, one codegen unit). No custom CPU flags were supplied.

The seam benchmark calls the same CPU `tools::scale::resize` used by document
rendering and as the live preview fallback. Each deterministic RGB noise image
is reduced by 20% in each axis. Image generation is outside the timed region.
Each case has one warmup followed by three measured iterations; timings include
the result's allocation but exclude display/upload and the preview's 50 ms
debounce.

| Seam-carving workload | Original median | Optimized median | Speedup |
| --- | ---: | ---: | ---: |
| 640×480 → 512×384 | 217 ms | 98 ms | 2.2× |
| 1280×960 → 1024×768 | 1,752 ms | 472 ms | 3.7× |

A second run measured 219/1,791 ms before and 109/444 ms after. Bicubic
remained around 3 ms for the larger case. These are local kernel measurements,
not a guarantee for arbitrary images, machines or full UI latency.

Filtered previews also have a Vulkan compute path. On the integrated Radeon
8060S GPU, scaling 3840×2160 to 2560×1440 took 3.2 ms for linear and 4.8 ms for
bicubic after the source texture and shader were warm, including GPU readback.
The same operations took 20.2 ms and 18.5 ms on the CPU. Initial device and
shader setup took about 48 ms and happens once per scaling session.

## Changes

- Retain the image, energy map and seam-parent buffers throughout each axis
  reduction, shifting row tails in place instead of allocating/copying a new
  image per seam.
- Recalculate energy only around removed seams, including their neighbors in
  adjacent rows. Preserve the original RGB gradient and deterministic tie order.
- Keep cumulative costs in two rows instead of a full-image cost buffer.
- Check cancellation between rows so abandoned previews stop without finishing
  a whole seam.
- Borrow the input buffer for nearest, linear and bicubic resizing. Bypass
  resampling when output dimensions already match the source.
- Upload the source once per scaling session and run large linear and bicubic
  previews in a Vulkan compute shader. Preserve alpha by filtering premultiplied
  colors, then return ordinary RGBA pixels to GTK.

Nearest-neighbor remains on the CPU: it measured 2.3 ms versus 5.4 ms for a
warm GPU pass at 2560×1440 because readback dominates such a simple operation.
Small previews and reductions beyond 2× also use the CPU, avoiding GPU setup
overhead and retaining the CPU resizer's wider antialiasing kernel. Systems
without a hardware Vulkan adapter fall back automatically. Document rendering
and export continue to use the exact CPU resizer.

Seam selection still scans the remaining image once per removed seam. Large
reductions on large images remain more expensive than conventional resampling.
No reduced-resolution approximation or change to seam-carving quality is used.

## Reproduce

```sh
cargo test --release --lib tools::scale::benchmark::scaling -- --ignored --exact --nocapture
cargo test --release --lib tools::scale::gpu::tests::benchmark_gpu_previews -- --ignored --exact --nocapture --test-threads=1
cargo test --lib tools::scale::tests
cargo test --lib tools::scale::gpu::tests
```

For a local performance gate, prefix the benchmark command with
`DIORAMA_SEAM_BUDGET_MS=800`. That budget failed on the original implementation
and passed after optimization here; it is deliberately opt-in, not a timed CI
assertion for other machines.

`src/tools/scale/reference.rs` keeps the original seam implementation solely
as a test oracle. The regression compares exact RGBA output for 1,800 cases:
small dimensions, shrinking either/both axes, unchanged dimensions, single-pixel
outputs, transparency, flat colors and low-color images with tied costs.
