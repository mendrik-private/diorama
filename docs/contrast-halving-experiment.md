# Historical Game Asset: palette-quantized contrast halving

The runtime now uses [bicubic interiors with pixel-art contours](game-asset-hybrid.md).
The halving, final palette grid, and topology cleanup below are retained as
historical test-only experiments. Only the contour path stage remains active.

The prior ridge/wavelet implementation is checkpointed on
`experiment/game-asset-ridge-cleanup` at `1f62cae`. The production implementation
on `experiment/contrast-halving` is now `src/tools/scale/palette_halving.rs`.
Both the scaling-tool preview and document/export renderer use it. The old
wavelet module is compiled only for historical tests, with no runtime fallback.

## Algorithm

1. Build a source RGB palette from all pixels with nonzero alpha. Merge only
   when every pair in a group has CIEDE2000 distance **strictly below 2.5**.
   Use a fixed actual source colour as representative, favouring frequent
   colours. Never cap the palette, discard rare colours, or merge transitively.
   Conservative Lab bins may leave additional entries separate.
2. Quantize source RGB to its nearest eligible cluster representative, retaining
   each pixel's alpha. Cache this prepared source in the preview session.
3. Halve both dimensions exactly while neither axis would undershoot the target.
   Each 2×2 block supplies four NN candidates. Minimize alpha-weighted mean
   CIEDE2000 distance to that block, with a 0.2-weight contrast bonus against
   the surrounding 4×4 region. This selects a representative source colour
   without letting minority contour corners automatically consume whole cells.
   Hidden RGB cannot contribute. Equal scores prefer centre-aligned NN.
   Copy quantized RGB (nearest palette ΔE = 0), but average the block's alpha
   coverage instead of selecting its most opaque sample.
   A narrow exception preserves a supported thin line: matching neighbours
   (ΔE < 8) in both directions and unlike flanks on both sides. The flanks must
   agree on opaque/transparent status, distinguishing a filament from a broad
   silhouette edge. Such candidates retain contrast priority and their alpha.
4. Finish directly on the target grid, using exact source-pixel overlap areas.
   Group palette RGB by alpha-weighted area, then select a representative with
   the same 0.2-weight local contrast bonus. The context is 5×5 around the cell
   centre. For very large footprints, score the 16 highest-coverage colours
   against all footprint colours, bounding candidate work. No RGB blending or
   bicubic filtering occurs. Thin-line promotion requires the source sample's
   centre to belong to this target cell; overlapping fragments do not each get
   to claim a full neighbouring pixel.
5. Refine the completed target grid against the original quantized source:
   remove only redundant 8-connected L corners with less than 45% contour
   support and at least 40% supported replacement fill. Protect endpoints,
   disconnected arms, strong corners, and other narrow colour bands. Then
   consider one-pixel gaps along horizontal, vertical, and both diagonal axes.
   Endpoints must be narrow, opaque, and agree within ΔE 6; the gap must differ
   by ΔE 12 or transparency. Require at least 2% matching source coverage within
   0.35 target pixels of the connecting segment. Copy an actual source palette
   colour; never synthesize the contour from endpoints alone. Reject bridges
   that add a local 3/4 or 4/4 contour block. Snapshot eligibility and revalidate
   neighbours so edits cannot trigger an expanding chain of repairs. This runs
   once, after the final grid (including exact-halving targets), before alpha
   thresholding. It is a conservative local heuristic, not global path tracing.
6. Reconstruct qualified dark silhouette strokes as paths. The alpha boundary
   supplies an ordered search region, not the finished line. A small spatial
   Gaussian first/second derivative basis estimates a continuous normal; signed
   even/odd response plus brighter flanks locate a dark stroke centre and reject
   ordinary step edges. This is a derivative-based phase approximation, not an
   exact Hilbert/Gabor quadrature bank and not the old FFT pipeline. Min-sum
   chain inference chooses 8-connected pixels within 0.9 target pixels of the
   source centreline, carrying previous direction in its state. Turn penalties
   depend on source continuation, so supported corners are permitted. Each
   coherent run uses one actual source-palette ink colour. Restore source fill
   only in the adjacent inward strip where the selected shade fits the ink/fill
   blend; remove outward halo pixels only where the source is transparent.
   Confirmed ink is opaque. Interior strokes without silhouette seeds remain
   handled by the preceding grid and topology stages, not this path tracer.
7. Elsewhere the app keeps **coverage alpha**. The comparison also offers **binary alpha**,
   thresholded at 128 after selection, without changing RGB. Palette snapping
   is no longer needed: both versions already contain source palette RGB only.

800×800 → 128×128 runs **800 → 400 → 200 → palette-grid 128**. Odd dimensions cannot
be halved exactly, so any remaining reduction uses the area grid. Non-uniform
targets stop halving as soon as either axis would undershoot. Identity requests
return the original, and enlargement is rejected.

This is a local contrast heuristic, not contour tracing. It can emphasize noise
or thicken features. Coverage alpha retains partially transparent edges;
binary alpha is crisper but may lose weak details. Neither promises an
AI-redrawn result or perfect connectivity for arbitrary curves.

## Verify and compare

```sh
CARGO_PROFILE_DEV_OPT_LEVEL=2 cargo test --lib palette_halving
CARGO_PROFILE_DEV_OPT_LEVEL=2 \
  DIORAMA_GAME_ASSET_INPUT=/home/mendrik/Downloads/elf2-se.png \
  cargo test --lib elf_palette_halving_comparison -- --ignored --nocapture
CARGO_PROFILE_DEV_OPT_LEVEL=2 cargo build
```

The fixture creates a fresh `/tmp/diorama-palette-halving-*` directory containing
quantized source and half-size intermediates, 128px and 160px palette-grid PNGs
with coverage and binary alpha, 3× comparison sheets (coverage left, binary
right), and an 8× hood/hair crop. Timings are optimized-development measurements,
not controlled release benchmarks. Artistic quality requires visual review.

With `DIORAMA_COMPARE_BASELINE` pointing to a previous fixture directory, the
fixture also exports whole-image before/after sheets and
`128-cloak-before-after-8x.png` (old left, new right).

The path stage adapts the supplied Vision Book sections: *Local Amplitude and
Local Phase* and *Steerable Filters* in `spatial_filter_sets.qmd`, chain inference
in `graphical_models.qmd`, and good continuation in `taxonomy.qmd`. It does not
use ordinary sharpening (`derivatives.qmd`), which cannot assign line versus
fill membership. The synthetic tests distinguish a step from a dark ridge,
check connected corridor paths and genuine corners, preserve unoutlined
shading, and verify source-palette RGB plus reduction of a blended outline band.

Tests cover real production dispatch, palette-only final RGB, stage sizes,
isolates, pairwise palette bounds, original RGB representatives, alpha,
cancellation, cache reuse, odd sizes, area coverage, thin-line ownership, and invalid targets.
The earlier `halving_experiment.rs` remains a test-only historical trial without
palette quantization; it is not the app implementation.
