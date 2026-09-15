# Manual AA validation

The adjustable AA control added later uses **50% as its default**, preserving
the 95%-floor/half-fringe appearance and snapshots documented below. At 0% AA,
cores are opaque and the fringe is off; at 100%, the core floor is 90% and the
full geometric fringe is used. See the current [rendering contract](game-asset-scaling.md).
The original performance measurements below predate this UI control.

## Adjustable AA checks

- All 101 integer settings are checked for monotonic core attenuation/fringe gain,
  the per-pixel core floor, preserved opaque runs, and exact endpoint behavior.
- 50% remains byte-exact with all three existing reviewed elf snapshots; no
  snapshots were regenerated for the adjustable control.
- Same-size requests at 0/100/50/0 verify cache identity, repeated-request reuse,
  source-analysis reuse, preview/committed-document parity, and undo/redo.
- A separate graphical test exercises the numeric spinner (default 50%, no AA slider),
  conditional visibility, setting retention, cancellation of obsolete requests,
  preview parity, keyboard focus/Escape, and Apply capturing the selected value.
- Layout is tested at the existing Scale panel's measured native minimum width,
  then medium/wide widths, with AA pinned at the right of the first row. The control
  adds no hardcoded theme or accent colors.
- The graphical check and reviewed captures pass in dark, light and high-contrast
  modes. Spinner-only captures are under `/tmp/diorama-aa-control.tBC7fN/aa-spinner-*.png`.
  Screen-reader interaction, RTL and enlarged text were not separately exercised.
- `cargo test --all-targets`: 229 passed, 60 ignored; the new graphical test and
  the two existing Scale layout/control tests pass separately on headless Weston.
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, schema compilation
  with `glib-compile-schemas --strict`, and `git diff --check` pass. `cargo build`
  rebuilds the development executable; no existing user window is restarted.

To reproduce the graphical check, compile `data/` schemas into a temporary directory,
set `GSETTINGS_SCHEMA_DIR` to it and `GSETTINGS_BACKEND=memory`, and run
`cargo test --lib game_asset_aa_control_updates_preview_and_committed_operation -- --ignored --test-threads=1`
on a graphical display. `DIORAMA_AA_UI_CAPTURE` optionally names a PNG screenshot.

Implemented 2026-09-15 after review of Inglis, Vogel and Kaplan,
*Rasterizing and antialiasing vector line art in the pixel art style* (NPAR 2013),
[DOI 10.1145/2486042.2486044](https://doi.org/10.1145/2486042.2486044).
This borrows the paper's thin-stroke sampling, apparent-thickness normalization,
selective antialiasing and limited opacity levels. It does not implement its shape
realignment/partial sorting, globally quantize RGB, or claim to solve all conflicts
between distinct paths. See [the rendering contract](game-asset-scaling.md).

## Acceptance criteria and evidence

- At default 50% AA, every selected core pixel has coverage at least 243/255 (95%), after normalization
  and quantization; intrinsic contour strength is applied separately.
- Fringe opacity is halved after quantization, retaining the original selection
  thresholds. Nearest-byte rounding maps 0/43/85 to 0/22/43, at most half a byte
  from exactly 50%. The fringe footprint and core geometry are unchanged.
- A stronger contour's core survives contact with a weaker neighbor. Rasterization
  and cleanup retain per-contour identity. Donor lookup cannot change that identity.
- Clean horizontal, vertical and 45-degree digital runs stay fully opaque and do
  not acquire a fringe. AA uses only the one-pixel neighborhood of bends.
- Reversing or duplicating patches does not change sampled coverage or core pixels.
- Horizontal sample coverage agrees with exact pixel intersections. An independently
  written 64-by-64 distance oracle checks slopes, phases, caps, clipping and point
  strokes against the 8-by-8 sampler, with a preselected 1/8 coverage tolerance.
- Cancellation, transparent donors, empty contours, rectangular outputs, cache
  behavior, the numerical biharmonic solver and linear-light composition remain
  covered by separate tests.

The original targeted regression failed with `selected core has only 166/255 AA
coverage`. The same floor test now covers multiple slopes and fractional phases.
The follow-up 95%-floor/half-fringe tests were also run against the preceding
implementation and failed before the opacity tuning. Exhaustive byte inputs check
both palettes' unchanged selection thresholds, including rounding boundaries.
Tests additionally isolate the drake failure mechanism using synthetic intersecting
contours and a nearer green donor competing with an owned black core.

## Drake at 128 pixels

Input: `51-drake.png`, 1254 by 1254, opaque RGB8, SHA-256
`9c522511a93f7243de01c7c88e73e56b844e2cd09bf5bca2eb3555d49d6d470f`.

The outer wing trace (source ID 124 in this analysis) changes from five disconnected
owned-core components of sizes 18, 2, 11, 18 and 12 to one connected component of
66 pixels. Its measured source width remains 7.803822 pixels and its intrinsic
strength remains exactly 1.0.

| Target pixel (zero-based) | Before | After |
|---|---|---|
| (38,26) | Core removed; black ink only 89/255 fringe coverage | Black owned core restored, 243/255 coverage |
| (28,32) | Black core weakened to 166/255 | Black core at 243/255 |

The new outer trace has 24 core pixels at 255 and 42 at lower allowed levels; none
falls below 243. The source donor at (38,26) remains RGB (1,3,0), not the green of
the nearby short trace. Full images were inspected at 128/160/200 pixels and the
128-pixel wing at nearest-neighbor magnification. The source file is not modified
or recognized by the production renderer.

The 95% follow-up was compared pixel by pixel with the initial 85% implementation
on both drake and elf at 128/160/200 pixels. All six core masks are identical, no
core AA byte decreases, and every non-core AA byte equals half its prior value
rounded to the nearest byte. The wing remains one 66-pixel owned component with
full intrinsic strength. Reviewed outputs and masks are in
`/tmp/diorama-aa95.ECZqAl/{drake,elf}/`; the drake wing crop is `drake/crop-128.png`.

## Snapshot provenance

The original `fixtures/elf-{128,160,200}.png` images remain unchanged. They are the
independent Python-produced reference for the old rendering contract, not expected
outputs of the new AA. The regression still requires byte-exact parity with those
images outside a two-pixel neighborhood of the new core, checking over half of
each output's pixels. Separate numerical solver oracles remain unchanged.

The following **implementation-produced, visually reviewed** snapshots freeze the
new appearance. They are not used as evidence that the underlying math is correct;
that comes from the independent coverage, donor, opacity and topology invariants.
Review included native size and 4x nearest-neighbor views, with attention to the
elf's bow, face, cape outline, feet and transparent background.

Source `fixtures/elf.png` SHA-256:
`cc3e15e92312cd825feadba7fde6424cb22bda94a36c9467d71b024f0ed8289f`.

| Snapshot | PNG SHA-256 |
|---|---|
| `elf-aa-128.png` | `8d3527f3347af3de464c0cbba45663c3d9e62c17d915dc8ad35ab16bdbb32508` |
| `elf-aa-160.png` | `654c7e9a4c88bd9570e1745af42910e03050ff86f9c86e8462961c1d85b46b4b` |
| `elf-aa-200.png` | `e17f98ad1d9e69210f781f1e7e8d8dc4aa82d3c5f8565b2537337a86445446e3` |

The ignored `game_asset_visual_check` test exports native and magnified images,
core and AA masks, and a 128-pixel core CSV. Set `DIORAMA_SCALING_SOURCE` to an asset
at least 200 pixels on both axes (omitting it selects the elf), and
`DIORAMA_SCALING_ARTIFACTS` to an output directory, then run:

```sh
cargo test --release --lib game_asset_visual_check -- --ignored --nocapture --test-threads=1
```

The harness measures completed resizes before diagnostic exports and reports the
separate target-contour phase. It never selects alternative production code paths.

## Performance method

The performance measurements below describe the initial 85%-floor implementation,
before the follow-up tuning to a 95% core floor and half-opacity fringe. This
follow-up changes opacity constants and a few integer operations, not sampling or
solver work; no new performance claim is made.

CPU: AMD Ryzen AI MAX+ PRO 395; compiler: rustc 1.94.1, x86-64 Linux. Compare the
same release profile (`opt-level=3`, thin LTO, one codegen unit), solver dispatcher,
source image and target sizes. A saved pre-change executable supplies the baseline.
The visual contract changes deliberately; performance is not claimed as bit-exact
equivalence between old and new renderers.

Hypothesis: bounding supersampling to the contour band and reusing immutable
topology tables keeps the new AA's end-to-end overhead modest. A material median
latency increase beyond observed IQR would falsify that expectation. An exploratory
drake profile (including extra diagnostic contour renders) attributes about 1.2%
of samples to coverage rasterization; smoothing/spatial queries and the unchanged
biharmonic solve dominate. This is a profile observation, not a standalone AA timing.

Release comparison uses seven measured iterations per version in ABBA blocks of
3, 3, 4 and 4, with the benchmark's untimed warm-up at each invocation. No local
compilation runs during these measurements. Median and IQR, not one cold run, are
the acceptance evidence. Source preparation, first resize, and uncached target
resizes are reported separately; image decoding and diagnostic exports are excluded.


## Release results

Raw observations: [game-asset-aa-timings.csv](game-asset-aa-timings.csv). Each invocation used `DIORAMA_BENCH_CASE=elf DIORAMA_BENCH_SAMPLES=1`; blocks group independent invocations. The follow-up fixed affinity with `taskset -c 2`, using six samples per version in ABBA, BAAB, ABBA order. Aggregate medians and linearly interpolated quartiles are computed from the raw observations; all times below are milliseconds.

### Unrestricted affinity (seven samples per version)

| Stage | Before median (IQR) | After median (IQR) |
|---|---:|---:|
| analysis | 184.1 (9.7) | 185.2 (68.0) |
| first_resize | 906.2 (176.2) | 1018.3 (251.3) |
| target_128x128 | 722.1 (182.0) | 833.1 (268.7) |
| target_160x160 | 997.6 (507.0) | 1042.9 (340.2) |
| target_200x200 | 704.6 (317.7) | 1139.4 (267.8) |

### Fixed CPU affinity (six samples per version)

| Stage | Before median (IQR) | After median (IQR) |
|---|---:|---:|
| analysis | 188.2 (74.9) | 177.4 (8.0) |
| first_resize | 813.2 (898.9) | 802.7 (483.6) |
| target_128x128 | 629.9 (738.8) | 626.2 (485.2) |
| target_160x160 | 579.1 (544.5) | 672.9 (743.7) |
| target_200x200 | 563.9 (511.7) | 764.1 (413.1) |

The unrestricted 200-pixel median suggested a slowdown, prompting the fixed-affinity follow-up rather than dismissing it. Fixed affinity still exhibited large outliers, including in unchanged source analysis and the pre-change executable. The fixed-affinity 128-pixel first-preview medians were 813 ms before and 803 ms after; the larger-target median differences were smaller than their observed IQRs. These measurements do not establish a performance win or exclude a smaller regression. No speed claim is made; the measured fidelity improvement is the reason for this change.

## Verification

- `cargo test --all-targets`: 227 passed, 59 ignored (GUI/manual tests).
- Release Game Asset suite: 36 passed, 3 ignored.
- `cargo fmt --check`, `git diff --check`, `cargo check --all-targets`,
  `cargo clippy --all-targets -- -D warnings`: pass.
- Crusty validation: no new or worsened architecture findings.
- `cargo build`: normal development executable rebuilt. No running user application was restarted.
