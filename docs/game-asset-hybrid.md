# Game Asset: bicubic interiors, pixel-art contours

The production Game Asset scaler now renders the original source directly to
the target size with the existing Catmull–Rom bicubic implementation. It then
repaints only source-supported dark silhouette contours and their immediate
blend/halo bands. Preview and document/export rendering share this pipeline.

The source palette is still quantized with a strict pairwise CIEDE2000 < 2.5
merge threshold, preserving rare accents and original representative RGB.
This prepared image is used for contour evidence and ink only. It is **not**
the bicubic input: interior shading remains unquantized bicubic RGB and alpha.

Contour reconstruction uses the existing small spatial Gaussian derivative
basis, signed even/odd ridge test, continuously estimated normal, and min-sum
connected path inference. Source continuation controls turn penalties, so a
genuine corner is not forbidden merely for forming an L. Coherent paths use
actual source-palette ink. Only adjacent supported contour blends are replaced
with fill; only source-transparent outward halo pixels are cleared. Confirmed
ink is opaque; bicubic alpha remains elsewhere. The optional binary-alpha
comparison thresholds final alpha without changing RGB.

The contrast-selected half-size passes, palette-only target-grid selector, and
local topology cleanup are historical test-only code. They do not run in the
hybrid. The runtime log says `Game Asset bicubic-and-contour hybrid complete`,
and the scaling status says `Bicubic + contours`.

This is not a global palette-only output: bicubic intentionally introduces
interior colours. The source-palette guarantee applies to repainted ink/fill.
The path tracer currently covers outlined silhouettes, not every interior
stroke. Soft unoutlined regions are left untouched.

## Validation and visual comparison

```sh
CARGO_PROFILE_DEV_OPT_LEVEL=2 cargo test --lib palette_halving
CARGO_PROFILE_DEV_OPT_LEVEL=2 \
  DIORAMA_GAME_ASSET_INPUT=/home/mendrik/Downloads/elf2-se.png \
  DIORAMA_COMPARE_BASELINE=/tmp/diorama-palette-halving-tttqAP \
  cargo test --lib elf_hybrid_comparison -- --ignored --nocapture
CARGO_PROFILE_DEV_OPT_LEVEL=2 cargo build
```

The fixture creates `/tmp/diorama-hybrid-*`. It exports 128px/160px hybrid and
plain-bicubic PNGs; method comparison sheets show previous Game Asset / plain
bicubic / hybrid. It also exports old/hybrid cloak crops and coverage/binary
alpha comparisons. Tests verify direct bicubic equivalence for unoutlined
images and interior texture, actual outline repainting with palette ink,
preview/export routing, identity, cancellation, and downscale-only dimensions.

See [the prior experiments](contrast-halving-experiment.md) for the evolution
and the supplied Vision Book sections behind the contour inference approach.
