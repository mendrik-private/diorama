# Historical Game Asset wavelet scaling

The runtime Game Asset renderer has been replaced by
[bicubic interiors with pixel-art contours](game-asset-hybrid.md).
The implementation below is retained only for historical tests and on the
`experiment/game-asset-ridge-cleanup` branch; it is not called by the app.

The previous **Game Asset** entry in the scaling toolbar implemented the accompanying
[v0.1 specification](game-asset-scaling.md). It is available only at or below
the source dimensions. The existing Aspect control supplies the explicit
choice to allow non-uniform scaling; aspect preservation is otherwise the
default. The chosen method is persisted as `game-asset`.

## Processing

`src/tools/scale/game_asset/` separates source analysis, geometry, candidate
selection, and rendering. Its result contains an image, per-pixel source
coordinates and feature membership, effective options, and diagnostics.

1. Build the centre-sampled baseline and generate a deduplicated 4×4 grid of
   source sampling phases lazily for relevant output cells.
2. Analyse CIELAB colour, alpha occupancy, 8-connected foreground components,
   and 4-connected background regions and holes. An exact squared Euclidean
   nearest-foreground transform extends colours for analysis only, with
   deterministic coordinate ties.
3. Apply eight oriented, non-decimated log-Gabor quadrature filters across
   geometrically increasing wavelengths. FFTs use reflected padding. Lab
   channel energies share a robust median-based noise floor; directional strip
   medians and local continuation provide the specified colour prominence.
   Thin alpha filaments also have independent silhouette evidence.
4. Trace ridges and closed cell-boundary contours. A separate, bounded alpha
   thinning pass joins narrow appendages into colour-independent centre lines;
   broad interiors are not skeletonized or repainted. Its radius follows the
   cached analysis scale bank. Paths share source-coordinate
   junction anchors. Fit bounded-error piecewise straight segments, a
   conservative alternative to optimizing cubic control points; these need no
   further curve-flattening approximation. Explore at most eight placements,
   including free endpoint alternatives while retaining shared anchors.
5. For open paths, search the target-grid corridor and source labels jointly.
   The bounded shortest-path search can route around a cell without an eligible
   sample. It uses monotone source order, geometry/direction/colour costs, and
   at most four source-order states per cell. Shared anchors stay locked.
   This replaces the central placement when available; other placements and
   closed contours use canonical, reversible Bresenham tie-breaking. Choose source
   samples jointly by dynamic programming; enumerate the complete footprint
   if the sparse phases miss a compatible sample. Build complete corridor
   patches, including supported fill on the appropriate side of a displaced
   stroke, rather than drawing another line over the baseline.
6. Score both sample coverage and the longest uninterrupted supported run
   (wrapping closed paths), so disconnected fragments are not equivalent to a
   continuous line with the same pixel count. Select positive-gain patches with deterministic greedy ordering and up to
   two bounded local replacement sweeps. Preserve accepted paths and reject
   new unrelated joins, component losses, and hole losses. When no valid
   alternative exists, retain the best available representation and diagnose
   the unresolved feature.

The default baseline is centre-sampled nearest neighbour with binary alpha;
feature reconstruction repairs supported paths instead of replacing every cell
with a contextual fill. The experimental 5×5 colour-context selector is available
through `Options::grid_context` but disabled by default: it can over-promote dark
contours and suppress small highlights.

Source-palette clustering merges only colours whose complete-link CIEDE2000
distance is strictly below 2 (configurable with `Options::palette_delta_e`; zero
disables merging). Representatives are actual source colours, never averages.
There is no palette-size cap or rare-colour pruning. A conservative spatial
shortlist can leave close colours separate. Each output pixel records the local
footprint sample and, when its RGB changes, the palette representative's source
coordinate. Thus RGB provenance is source-exact, but the representative need not
lie inside the local footprint; its colour must be within the merge threshold
of the eligible local sample.

The downsampled renderer writes alpha 0 or 255 and stores transparent pixels as
`(0,0,0,0)`. Identity scaling returns the original RGBA values. There is no output
colour averaging, dithering, or final smoothing. Game Asset previews use hard
nearest-neighbour display without changing the saved viewing preference.

## Runtime and diagnostics

Analysis runs on the existing preview worker, checks cancellation throughout,
and is cached per scaling session and effective filter scale range. Document
operations use the same renderer. A dedicated Vulkan compute path handles
wavelet FFTs and filtering; the ordinary GPU image resizer still does not
intercept this method. The default estimated working-memory limit is 512 MiB; oversized
analysis reports the application's memory-limit error instead of silently
switching algorithms. The finite search budget is 20,000 proposal evaluations.

The scaling bar reports retained, dropped, and unresolved features. Its tooltip
includes unsupported candidate gaps, conflicts, unrelated joins, connectivity,
component/hole losses, and an exhausted-search notice when applicable. Full
source-coordinate provenance and effective options remain in the result API;
debug logging records the detailed document-operation report. Feature counts
are detector-dependent, not a measured percentage of artistic detail retained.

This remains an experimental downsampler. Small grids cannot represent all
source topology, and a feature detector can miss or over-segment a real stroke.
The implementation does not claim complete topology preservation or temporal
stability across animation frames.

Alpha centreline extraction uses the two-subiteration conditions from
[Zhang and Suen, 1984](https://doi.org/10.1145/357994.358023), bounded to a narrow
boundary band. A capped Chebyshev distance transform identifies that band;
batched deletions avoid iteration-order-dependent connectivity changes. Only
long enough narrow graph paths become proposals. This does not merge arbitrary
colour strokes or authorize bridging a gap with no source support.

### Dedicated GPU wavelets

The existing wgpu/Vulkan stack runs bounded radix-2 complex FFTs on padded
power-of-two axes up to 2048 samples. Source spectra, FFT work buffers, and
per-scale energy buffers remain on the device across all orientations and
wavelengths. Each orientation batches its wavelengths before one energy
readback. The host computes the robust noise-floor median and final evidence
scores; colour conversion/contrast, contour tracing, fitting, conflict selection,
and source-RGB copying stay on the CPU. Colour contrast retains the 80%-of-threads
worker policy. No native VkFFT dependency or new unsafe boundary is introduced.

Device/pipeline setup and CPU-generated FFT twiddles are reused process-wide.
`Options::gpu_wavelets` defaults to true; false forces the CPU reference.
Small inputs, unsupported dimensions, software-only adapters, or insufficient
working-memory/device-buffer limits select CPU analysis. Runtime GPU errors
discard partial evidence and restart the whole analysis on the CPU; cancellation
does not trigger a retry. Readback waits check cancellation in short intervals.
`Scaled::gpu_wavelets` reports which path actually completed.

GPU arithmetic is not bit-identical to CPU arithmetic. Small score changes can
alter threshold decisions and feature selection. Exact source RGB and binary
alpha remain guaranteed by the unchanged CPU renderer, but CPU/GPU final-image
identity is not promised. See the [WGSL floating-point accuracy rules](https://www.w3.org/TR/WGSL/#floating-point-accuracy).
Hardware tests compare normalized wavelet scores with a maximum absolute error
of 0.001, check source-colour provenance on real assets, and check repeatability
on the same GPU. FFT twiddles are calculated in host f64 then stored as f32 to
avoid repeatedly evaluating trigonometric functions inside GPU butterflies.

## Verification and visual comparison

```sh
cargo test --lib tools::scale::game_asset::tests
DIORAMA_GAME_ASSET_INPUT=/path/to/asset.png \
  cargo test --release --lib tools::scale::game_asset::tests::asset_ablation \
  -- --ignored --exact --nocapture
```

The optional real-asset check writes PNGs to a newly created temporary directory
and prints its location. The enlarged comparison is, left to right: binary-alpha
nearest neighbour, full Game Asset, colour evidence without wavelets/fitting,
wavelets without fitting, and fitting without wavelets. Set
`DIORAMA_GAME_ASSET_WIDTH` to choose a target width (default 128).

Automated checks cover exact RGB provenance and footprint membership, alpha
thresholds, identity, invalid/enlarged dimensions, cancellation, determinism,
cached versus independent results, invisible-RGB contamination, Bresenham in
every octant, thin dark/bright/chromatic strokes, transparent filaments, slopes,
arcs, corners, gradients, incompatible parallel strokes, components and holes.
The graphical test exercises the dropdown, persisted selection, pixel and
percentage limits, preview generation, and hard preview display.

### Connected-feature quality regression

The optional `elf_visual_quality` test uses `elf2-se.png` (800×800, SHA-256
`cc3e15e92312cd825feadba7fde6424cb22bda94a36c9467d71b024f0ed8289f`).
It checks actual 8-connected output pixels in the unobscured bowstring, rather
than trusting the detector's feature counts. It also checks excessive thickness
and source-palette/footprint provenance. A highlight-retention check catches
excessive darkening of the quiver feathers. Fixture coordinates occur only in tests.

```sh
DIORAMA_GAME_ASSET_INPUT=/path/to/elf2-se.png \
  cargo test --release --lib elf_visual_quality -- --ignored --nocapture
```

The default sizes are 96, 128, and 160; `DIORAMA_GAME_ASSET_SIZES` accepts a
comma-separated override. Each run exports original-alpha NN, binary-alpha NN,
Game Asset, enlarged comparisons, and bowstring close-ups to a fresh temporary
directory. Set `DIORAMA_GAME_ASSET_BEFORE` to an earlier run's directory to
include the old Game Asset as the middle panel: **binary NN / old / new**.
Inputs are never modified. The test intentionally fails on a broken or thickened
string, or excessive highlight loss, after exporting all the comparison images.
`DIORAMA_GAME_ASSET_GRID_CONTEXT=1` enables the experimental contextual baseline
for comparison; the default uses the application's nearest-neighbour baseline.

Measured connected vertical spans on the fixture:

| Output size | Binary NN | Before | Updated | Checked rows |
| --- | ---: | ---: | ---: | ---: |
| 96×96 | 2 | 9 | 10 | 10 |
| 128×128 | 2 | 6 | 13 | 13 |
| 160×160 | 3 | 13 | 17 | 17 |

The original renderer failed all three. Adding target routing alone still
failed; adding the colour-independent alpha trace made all three continuous.
Visual inspection of the full sprites and close-ups confirms that improvement;
the broad silhouette remains stable. Additional checks at 64, 80, 112, 192,
and 256 pixels also produced continuous strings. This is evidence for thin
feature reconstruction, not proof that every image or internal colour contour
will look better, nor that wavelets alone improve pixel-art quality. Binary
alpha still produces harder edges than NN with partial transparency.

With the nearest-neighbour baseline and source-palette pass, the same three
connected spans remain 10/10, 13/13, and 17/17. Bright feather-pixel counts are
12, 27, and 43 versus binary NN's 14, 28, and 42. This guards against the
experimental contextual selector's darkening regression; it is not a general
perceptual-quality metric or a claim to reproduce AI-redrawn artwork.

Before source-palette clustering was added, the quality revision had a CPU cost:
a release-mode check on the same 800→128
fixture measured approximately 1.22 s cold / 0.18 s cached (five rounds), versus
1.05 s / 0.12 s for the earlier renderer (three rounds). These are local timings
with background-load variability, not a speedup claim. The dedicated GPU
wavelets and 80%-of-logical-threads colour pass are unchanged. CPU/GPU comparison
of that pre-palette 128×128 result found zero differing output pixels. These
timings and the image-identity result do not measure the later palette pass.

## Performance checks

### RGBA contour sampling experiment (not enabled in the app)

`sampling_experiment.rs` is test-only. It compares nine global sampling offsets
and a lightweight local dark-ridge selector against RGBA nearest neighbour on
the elf fixture at 128 and 160 pixels. The local selector requires two-sided
contrast and nearby continuation in one of four directions. A spatial penalty
limits movement inside each target cell. Contrast amount is only a colour
selection guide: the result copies an actual source RGBA sample in the cell,
with exactly the NN baseline's alpha. It does not run wavelets, quantize a
palette, trace paths, or bridge transparent lines.

This is a square-image prototype, not a new production algorithm. Its amount
0.5 candidate improves separation of the hood and blonde hair in the inspected
128-pixel fixture, but general artistic quality and phase stability are not
established. Stronger settings can over-darken details. No app default changes
follow from the experiment. Timing logs from development builds are not release
benchmarks.

```sh
DIORAMA_GAME_ASSET_INPUT=/path/to/elf2-se.png \
  cargo test --lib elf_sampling_offsets -- --ignored --nocapture
DIORAMA_GAME_ASSET_INPUT=/path/to/elf2-se.png \
  cargo test --lib elf_local_ridges -- --ignored --nocapture
```

The tests export to separate temporary directories. `128-before-after-3x.png`
and `128-head-before-after-10x.png` show NN on the left and amount 0.5 on the
right. `128-ridge-2.png` is the unscaled RGBA candidate. A synthetic regression
checks a missed dark separator, isolated-dot rejection, source sample membership,
unchanged alpha, and an unchanged opaque gradient.

#### Crisp contour cleanup trial

The test-only `sampling_experiment/cleanup.rs` starts from the selected ridge-2
candidate (amount 0.5). It tries three bounded passes: binary alpha in the
two-pixel silhouette band of broad shapes, snapping near-endpoint edge shades
to an eligible source colour, and replacing redundant diagonal elbows with
source-supported fill or transparency. Thin features retain their RGBA samples;
there is no whole-image alpha threshold, blur, or global palette reduction.

Elbow removal requires diagonal continuation on both arms, rejects straight
right-angle corners, and checks local foreground/background connectivity.
Edits are sequential so adjacent removals cannot rely on stale connectivity.
These are conservative local guards, not a global topology guarantee.
Colour selection now also simulates each eligible source sample in a bounded
9×9 patch. It rejects samples that introduce a detected elbow at a previously
elbow-free position in the affected 5×5 neighbourhood, then tries the next
closest source colour. If none is safe, the existing pixel stays unchanged.
For de-blurring, both locally supported edge colours are considered. Remaining
detected elbows rank ahead of colour closeness; switching to the farther colour
is allowed only when it strictly reduces elbows and passes the connectivity
guard. This avoids hardening a soft, already-present elbow merely because the
contour colour is numerically closer than the fill.
Shade cleanup reads the current output, so neighbouring edits cannot each rely
on the same stale contour. Both shade selection and thinning use this guard;
the initial alpha-band cleanup is unchanged.
Replacement RGB bytes come from the original target cell's source footprint;
alpha may be made binary in the cleanup band. The ordinary Nearest and Game
Asset application renderers are unchanged.

```sh
DIORAMA_GAME_ASSET_INPUT=/path/to/elf2-se.png \
  cargo test --lib elf_cloak_cleanup -- --ignored --nocapture
```

Outputs include `128-clean.png`, `before-after-3x.png`, and
`cloak-before-after-8x.png` (ridge-2 left, cleanup right). The four-panel
comparison also separates crisp alpha, shade cleanup, and elbow cleanup.
The fixture test verifies source-colour provenance, actual edits in the cloak,
and byte-identical preservation of the checked middle bowstring section.
Synthetic tests cover real corners, isolated dots, bridges, holes, thin RGBA
lines, fringe removal, small images, and choosing a second-best source colour
to avoid a new elbow in all four rotations. They also cover preferring supported
fill over hardening an existing elbow. The optional
`elf_deblur_does_not_create_stairs` regression checks the entire fixture after
shade cleanup and again after thinning. Set `DIORAMA_GAME_ASSET_CLEANUP_BEFORE`
to a previous cleanup directory to export revision-to-revision comparisons.
Appearance still needs human review;
the trial does not claim to remove every staircase or intermediate edge shade.

### Production solver

The solver ranks proposals once per accepted image revision. Rejected patches
do not change pixels or gains, so their successors can use that ranking. Every
accepted edit invalidates it, including previously rejected proposals. This
preserves the original ordering, tie-breaking and finite search budget without
recomputing all path scores after each rejection.

Source analysis reuses colour-strip storage and selects component-wise medians
without fully sorting each strip. Large images distribute independent colour
rows across up to 80% of available logical threads (rounded down, with a minimum
of one); a 32-thread machine uses up to 25 workers. Available rows and the
remaining working-memory budget also cap the count. Each scoped worker has a
2 MiB stack; small images and memory-constrained runs remain serial. Workers
check cancellation per row and finish before the next orientation, preserving
the original evidence update order. FFT columns are gathered in small adjacent
bands to avoid repeatedly fetching the same strided image cache lines; each
column still uses the original transform and normalization. These changes do not
reduce orientations, wavelengths, candidate counts, or fitting quality. Tests
compare medians and forward/inverse FFT results exactly against their original
implementations, and compare serial and parallel colour evidence exactly.

The opt-in latency test times three independent scaling sessions. Each session
measures a cold resize followed by a one-pixel-smaller cached resize. Decoding,
contract checks and fingerprints are outside the timed region. Optional gates
check median cold latency and combined pixel/provenance/diagnostic fingerprints:

```sh
DIORAMA_GAME_ASSET_INPUT=/path/to/asset.png \
DIORAMA_GAME_ASSET_WIDTH=128 \
DIORAMA_GAME_ASSET_ROUNDS=3 \
DIORAMA_GAME_ASSET_BACKEND=cpu \
DIORAMA_GAME_ASSET_MAX_SECONDS=5 \
DIORAMA_GAME_ASSET_EXPECTED_FINGERPRINTS=226b0784,263cbc4f \
  cargo test --release --lib tools::scale::game_asset::tests::asset_latency \
  -- --ignored --exact --nocapture
```

Those fingerprints belong to the 800×800 `elf2-se.png` fixture with SHA-256
`cc3e15e92312cd825feadba7fde6424cb22bda94a36c9467d71b024f0ed8289f`.
Omit fingerprint and timing gates when establishing a baseline on another
image or machine. No machine-dependent latency gate runs in ordinary CI.

Use `DIORAMA_GAME_ASSET_BACKEND=gpu` to require GPU execution in the same
latency harness (omit CPU fingerprints). `auto`, the default, allows fallback.
GPU validation and end-to-end image comparison can be run explicitly:

```sh
DIORAMA_GAME_ASSET_GPU_TEST=1 cargo test --release --lib \
  tools::scale::game_asset::analysis::tests::gpu_wavelet_scores_match_cpu_reference \
  -- --ignored --exact --nocapture
DIORAMA_GAME_ASSET_INPUT=/path/to/asset.png cargo test --release --lib \
  tools::scale::game_asset::tests::gpu_asset_comparison \
  -- --ignored --exact --nocapture
```

The image comparison times the complete cold resize, including initial GPU
setup, upload, compute, energy readback, CPU analysis, and final rendering. It
reports changed pixels and feature counts, checks repeatable GPU output, and
writes CPU/GPU PNGs and a side-by-side enlargement to a temporary directory.

Measurements use the unchanged release profile (`opt-level=3`, thin LTO,
one codegen unit) on an AMD Ryzen AI Max+ Pro 395, Linux x86-64, rustc 1.94.1.
Before/after binaries use the same fixture, options and timing harness. These
are three-run medians on a shared development host, not latency guarantees.
Source complexity and changes to the effective filter scale range still affect
runtime; changing that range requires fresh analysis.

With compilation stopped, the initial four-worker optimization of the 800×800
fixture measured:

| Resize | Original | Four workers |
| --- | ---: | ---: |
| Cold, 128×128 | 8.125 s | 3.305 s |
| Cached, 127×127 | 2.306 s | 0.119 s |

The cold runs were 8.125/9.629/7.693 s before and 3.305/3.126/4.125 s after;
cached runs were 2.320/2.290/2.306 s before and 0.119/0.118/0.284 s after.
The 5-second median cold gate fails before and passes after. Both output
fingerprints remain unchanged across all rounds.

The 900×900 `elf_se.png` fixture (SHA-256
`d5397be2a89e5fceecfd47b88b290055796c8c5be4f7846de6f53812a00f3b37`)
measured 12.912 → 6.846 s cold at 96×96 and 0.833 → 0.112 s cached at 95×95
with four workers. Cold observations ranged from 12.822–24.244 s before and
6.155–8.960 s after, illustrating shared-host variability. Its unchanged
cold/cached fingerprints are `b7ede971,1d9fb16b`.

### GPU round-trip result

Final paired runs used the 25-worker CPU policy and a Radeon 8060S integrated
GPU (RADV, Mesa 26.0.8). The 800×800 → 128×128 fixture measured:

| Backend | Cold resize median | Cached 127×127 median |
| --- | ---: | ---: |
| CPU wavelets | 2.297 s | 0.122 s |
| Vulkan wavelets | 1.071 s | 0.116 s |

CPU cold observations were 2.471/2.288/2.297 s; GPU observations were
1.318/1.071/0.960 s, including device/pipeline setup in the first GPU run.
Both cold and cached fingerprints matched the CPU reference in every round.
Cached resizing does not repeat wavelet analysis, so no substantial improvement
is expected there from GPU offloading alone.

The separate end-to-end comparison found **zero changed pixels** on both the
128×128 elf (16,384 pixels) and the 96×96 elf (9,216 pixels), with matching
retained/dropped/unresolved counts. The second fixture's single comparison was
2.871 s CPU versus 1.609 s GPU including setup and transfers. Repeated GPU
results also matched their own pixels, provenance and complete diagnostics.
Hardware score checks across five rectangular/square shapes and all eight
orientations, including both 2048-point axes, observed a worst normalized-score
error of approximately 0.000034. These observations do not establish bitwise
equivalence on other assets or GPUs.

RustFFT 6.4.1 supplies the FFTs. The log-Gabor construction follows the
mathematical building block described by
[Peter Kovesi's phase-symmetry reference](https://peterkovesi.com/matlabfns/PhaseCongruency/phasesym.m);
colour combination, candidate rendering and the solver follow the supplied
Game Asset design, not a claim of inheriting a grayscale method's guarantees.
