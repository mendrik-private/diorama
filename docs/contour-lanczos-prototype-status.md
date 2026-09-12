# Contour Lanczos reference implementation

Implementation branch: `experiment/contour-lanczos-v1`.
Previous bicubic Game Asset hybrid checkpoint:
`experiment/bicubic-contour-hybrid`, commit `bc9a839`.

This is a test-harness-only experimental method, not a replacement for Game
Asset or the ordinary RGBA8 Lanczos scaler. Its module is behind `cfg(test)`.
Neither the original [v1 specification](contour-preserving-lanczos.md) nor
[Amendment 01](contour-preserving-lanczos-amendment-01.md) has been rewritten.
Production/UI promotion is deliberately withheld pending the acceptance gates.

## Implemented

- Scalar f64 linear-light, premultiplied Lanczos-3 with widened reduction
  support, individual clamped taps, normalized coefficients, unclipped
  intermediates, and final straight-alpha sRGB encoding.
- Four-channel, five-scale finite-difference edge/ridge detector with subpixel
  positions, full source provenance, adaptive scores and binned deduplication.
- Positive destination splats, inverse-transpose normals, eight bounded slots,
  eligibility before ownership, cross-bin suppression, compatible-component
  hysteresis, ridge-flank association, and deterministic final ownership.
- Exact source-pixel replacements; no palette quantization, alpha normalization,
  halo removal, skeletonization, dilation, or invented connecting strokes.
- Immutable source session with lazy conversion/detection and one target-cache
  entry. Settings and source identity cannot change within a session. New sizes
  use original source candidates, never a previous target mask.
- Explicit dimension/settings/numerical/resource/cancellation errors. Preflight
  uses checked arithmetic and a conservative allocation envelope. Candidate
  storage has a hard configured limit; exceeding it errors rather than changing
  scales. Source preparation publishes only complete caches. Sorting uses
  1024-item runs and cancellable merges.
- Amendment 01 tensor confidence from the already smoothed four source planes.
  Raw score and confidence evidence survive deduplication and destination
  rejection in proposal diagnostics. The optional gate caps uncertain effective
  scores at 0.75 **before** every score-dependent selection stage. Candidate
  normals, positions, source colors and polarity are unchanged.
- Separate evaluation-only S/B/M/O artifacts. S includes projected weak source
  proposals before alpha eligibility/NMS. Repeated splats of one source ID are
  counted once in S matching; slot provenance remains available separately.
- Exact rounded-pixel counts and maximum-cardinality, minimum-squared-distance
  one-to-one matching at 0.5/1.0 pixels, with kind/orientation restrictions,
  stable-ID traversal, N/A denominators, and displacement percentiles.

There is no profile adapter in this prototype. `new_srgb` explicitly requires
already oriented, genuinely sRGB bytes. The elf fixture has an sRGB PNG chunk.
Do not route arbitrary decoded document RGB through this API or relabel an ICC
profile: application color conversion and preview/export metadata consistency
remain prerequisites for integration.

## Frozen experiments and evaluation limits

Default settings retain v1 scoring and do not compute confidence. The ablation
compares exactly two variants: confidence measured with unchanged scores
(`control`), and confidence measured with the uncertain-score cap (`gate`).
Scales and all numerical thresholds remain the specified defaults. No artwork
parameter tuning was performed. The confidence parameter version is Amendment
01, 2026-09-12; a different setting requires a new source session.

The evaluator is not the production detector. It uses fixed per-channel
Sobel/8 boundaries and signed directional ridge profiles at radii 1 and 2,
four normal directions, an absolute 0.03 response floor, and parabolic local
response localization. Color conversion is shared; detector/selection code is
not. It has a two-pixel image-frame measurement margin. Its unsupported artwork
responses are **unsupported by the detector**, not proven invented structure.
It is a diagnostic instrument with its own directional and scale limitations.

Synthetic scenes use analytic segments and region labels. Point-center samples
and 4×4 regular quadrature of a pixel's box footprint are separate datasets;
the latter is an approximation to area integration, not an exact integral or
an unspecified antialiasing filter. Destination geometry is constructed
independently from the segments and includes ridge centers and side boundaries
as separate kinds. Conflicting normals at a shared geometry pixel are labeled
as junctions and reported separately. Round end caps are rendered but the
current geometry annotations describe segment centers and parallel sides, not
complete cap arcs; endpoint boundary scores need that limitation considered.

Fixtures cover source widths 1/2/3/4/8/10, dark/bright strokes, vertical/diagonal/
shallow directions, phase 0/.25/.5/.75, resolvable/dense fences, alternating
color stripes, checkerboards, fine H text, T/X junctions, equal-luminance
boundaries, conflicting color-channel directions, flat regions and weak noise.
A polygonal curve family is held out from axis-line development. Explicit
targets are 37×37, 31×31, 20×20, 10×10, 4×4, 13×27, and 41×19 from 41×41.
No low-pass-prefiltered dataset is claimed.

Image-level CSV rows retain phase, sampling model, size, kind and junction
status. Pooled exact counts are also separated by fixture family; they are not
an overall quality grade. Shape diagnostics count 8-connected components,
filled 2×2 blocks and unmatched geometry samples. Known-normal profiles use
0.125-pixel bilinear samples within ±4 pixels, a fixed 0.03 contrast floor and
half-height visible width. Mask thinness is asserted independently. These
profiles expose residual base strokes; the overlay does not remove them.

Matching and feature extraction are never executed by `Reference::resize`.
The evaluation harness has explicit 30,000-sample / 250,000-edge graph limits;
it fails rather than silently switching to dilated overlap or greedy matching.

## Reproduce

```sh
CARGO_PROFILE_DEV_OPT_LEVEL=2 cargo test --lib contour_lanczos
cargo test --release --lib frozen_confidence_ablation -- --ignored --nocapture

# Known-sRGB PNG artwork; independent diagnostics plus timing report.
DIORAMA_CONTOUR_INPUT=/home/mendrik/Downloads/elf2-se.png \
DIORAMA_CONTOUR_CONFIDENCE=control \
cargo test --release --lib artwork_reference_comparison -- --ignored --nocapture
# Repeat with DIORAMA_CONTOUR_CONFIDENCE=gate for the frozen ablation.
# `none` is the v1 default without the optional tensor work.
```

Every export creates a fresh `/tmp/diorama-contour-*` directory. Base, final,
replacement mask, NMS/hysteresis layers, collision map, component/owner
provenance, proposal records, S/B/M/O and disagreement CSVs are retained.
The three-column comparison is **base / replacement mask / final**; orange
marks ridge owners and blue marks boundary owners. It is not an opacity mask.

For render-only release measurements, set `DIORAMA_CONTOUR_BENCH_ONLY=1` and
run the already built test executable under `/usr/bin/time -v`; do not include
Cargo compilation in the peak-memory measurement. This suppresses image,
proposal and matching exports. Reports separate coefficient setup (included in
target time), source preparation, warm-source reconstruction, cached-target
retrieval and cold total latency. Ten samples use the nearest-rank p95 (the
maximum at this sample count); these are small-sample measurements, not a
service-level percentile guarantee. Source files/decoding precede the reported
algorithm timings. Diagnostic evaluation time, including its file I/O, is
reported separately in non-benchmark runs.

Provisional review budgets declared before the release comparison on the
800×800 elf: source preparation median ≤5 s, warm-source target median ≤75 ms,
peak process RSS ≤512 MiB. Reference execution uses one CPU worker; no GPU or
parallel speedup is claimed. These budgets do not waive visual/contour gates.

## Initial finding

At 128 and 160 pixels the unmodified elf has **zero eligible replacement
owners** under v1: its main nonzero alpha values are 253, 252 and 251, not 255.
All 265,776 proposed splats in the first v1 run failed alpha eligibility.
Consequently the final image equals the reference base byte for byte. This is
contract compliance, not contour-recovery success. The source was not flattened
or made opaque to manufacture a favorable comparison.

Promotion additionally needs reviewed held-out artwork, complete
endpoint/branch annotations, a color-managed integration adapter, and a measured
interactive cancellation budget. Worker-schedule/SIMD/GPU/tiled equivalence is
not claimed: those optimized implementations do not exist in this prototype.

## Recorded results — 2026-09-12–13

The frozen ablation completed **3,024 rendered cases**: 27 scene families ×
4 phases × 2 sampling models × 7 targets × 2 scoring variants. Output/mask
contracts passed for every case. Full image-level exact/matched metrics,
per-family pooled counts, profile measurements and shape measurements are in
`/tmp/diorama-contour-ablation-aC7NEP`. A successful harness run means that it
completed and the hard contracts held, not that the artistic gates passed.

Selected **pooled exact-pixel ridge-mask** results (not tolerance scores or
rendered-image quality scores):

| Family | Control precision / recall | Gate precision / recall |
| --- | --- | --- |
| Dense fence | 0.8043 / 0.4130 | 0.8309 / 0.1457 |
| Resolvable fence | 0.4517 / 0.6805 | 0.4958 / 0.6618 |
| Shallow line | 0.6256 / 0.9029 | 0.7460 / 0.9010 |
| Diagonal | 0.4131 / 0.7802 | 0.4328 / 0.7740 |
| T junction, non-junction-normal subset | 0.6722 / 0.8382 | 0.8628 / 0.8401 |
| X junction, non-junction-normal subset | 0.5036 / 0.8071 | 0.5491 / 0.8037 |

Across 1,512 paired ridge cases, 128 gained mask components and 142 lost
geometry coverage at 1-pixel one-to-one tolerance; 157 gained coverage. More
components alone are not necessarily broken wanted lines (they may be extra
isolates), so the raw geometry/shape rows must be inspected together. A concrete
regression is the one-source-pixel bright horizontal stroke, phase 0, point
sampling, 41×19 target: components increase 1→2 and unmatched geometry samples
increase 0→2. These results fail the gate's no-regression promotion criterion.
The confidence gate remains opt-in; defaults are unchanged.

Residual width is observable: the 8-source-pixel dark horizontal stroke at
phase .25, 4×4 box quadrature, 20×20 target retains a median visible half-height
width of **4 destination pixels in both base and output**. This is not counted
as one-pixel visible stroke reconstruction.

The opaque dragon reference produces a mask, but inspection at 128 pixels
shows excessive textured/speckled replacements compared with its base. The
confidence variant does not establish a reviewed visual improvement. This
supports withholding the entire prototype from the production dropdown, not
just withholding the confidence gate. The transparent elf remains exactly the
base in both variants; neither result establishes useful elf contour recovery.

### Release measurements

AMD Ryzen AI MAX+ PRO 395 / Radeon 8060S machine, 16 cores / 32 logical CPUs;
one scalar worker, GPU unused. Rust 1.94.1, repository release profile
`opt-level=3`, thin LTO, one codegen unit. `/usr/bin/time -v` wrapped the built
test binary, not Cargo. Ten samples per mode/size, small-sample nearest-rank
p95, ordinary workstation load rather than CPU affinity/frequency isolation.
These differences are not sufficient to claim a speedup between modes.

| Mode | Preparation median / p95 | Warm 128 median / p95 | Warm 160 median / p95 | Peak RSS |
| --- | --- | --- | --- | --- |
| v1, no tensor | 1.031 / 1.090 s | 27.20 / 31.10 ms | 24.47 / 28.35 ms | 237.5 MiB |
| Control, tensor measured | 1.388 / 1.396 s | 22.51 / 24.74 ms | 23.91 / 26.11 ms | 303.7 MiB |
| Confidence gate | 1.438 / 1.498 s | 22.77 / 25.53 ms | 27.02 / 29.28 ms | 305.2 MiB |

Cold total 128-pixel median/p95: v1 1.055/1.113 s, control 1.410/1.417 s,
gate 1.458/1.525 s. Identical base-only warm medians in these runs were
6.84–8.87 ms; coefficient setup, included in target time, had medians
0.149–0.195 ms. Whole-target cache hits were below 1.4 µs at p95. Tensor
preparation adds work and memory; passing the provisional resource budgets
does not compensate for failed quality gates.

Render-only reports: `/tmp/diorama-contour-lanczos-1zboMN` (v1),
`/tmp/diorama-contour-lanczos-Kx9isu` (control),
`/tmp/diorama-contour-lanczos-DS3PlU` (gate). Matching/PNG/proposal export was
disabled for these measurements. Peak RSS includes the test harness and its
retained comparison session, not just a single source cache. Conservative
preflight envelopes are larger than actual RSS and are not presented as
measured peaks.

### Evaluation solver correction

The phase sweep exposed a residual-cost rounding cycle in the first f64
assignment solver. Its outputs were not accepted as the final sweep. The
corrected solver retains f64 geometry and squared distances but sums their
binary costs exactly with a bounded signed integer accumulator. Reverse arc
costs therefore cancel exactly. This is evaluation-only; it changes no
resampling/detector arithmetic or parameters. Regression tests cover exact
cancellation down to subnormal costs, augmenting paths, stable-ID ties,
doubled lanes and exhaustive small-assignment minimum-cost comparisons.
No epsilon, relaxed contour tolerance or greedy fallback was introduced.

Final diagnostic image sets using the corrected matcher:

- Elf control: `/tmp/diorama-contour-lanczos-BoaLW0`.
- Dragon control: `/tmp/diorama-contour-lanczos-VHaiWM`.
- Dragon confidence gate: `/tmp/diorama-contour-lanczos-rQsWLO`.

For example, `128-comparison-3x.png` shows base / mask / final. These are
temporary review artifacts, not committed reference goldens. The corrected
128-pixel dragon evaluation took 3.76 s (control) / 4.28 s (gate), including
extraction, exact assignment and file I/O, in optimized development builds.
Those are diagnostic overhead observations, not release resize benchmarks.

Final checks: 261 library tests passed, 69 ignored (display/manual fixtures);
17 focused contour tests passed; the separate 3,024-case frozen sweep and
artwork export tests completed; Clippy and scoped formatting checks passed.
No GUI method was added, so no preview/export equivalence or GUI quality pass
is claimed for this experimental reference.
