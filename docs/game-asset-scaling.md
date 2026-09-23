# Game Asset scaling

The shared [`asset-scaler`](https://github.com/mendrik-private/asset-scaler) crate
owns the production algorithm. `src/tools/scale/game_asset` adapts application
cancellation and errors: live previews use its cached `Session`, while document
rendering calls the same algorithm through the borrowed `resize` API. Diorama
first prepares contour analysis from the untouched source, then asks BiRefNet
for a foreground estimate. It scales that background-removed foreground as the
fill, then paints the original-source contour geometry last with foreground
donor colors. A live preview session
caches its successful foreground estimate across AA changes; document rendering
takes the same sequence. Its separate BiRefNet run can vary slightly from the
preview model output, so the two results are not promised to be byte-identical.
BiRefNet's foreground alpha is multiplied by explicit source alpha, which stays
authoritative.
The **Show contours** inspection preview at source size draws bounded fitted
cubic source traces, so long curves are shown as their cleaned geometry without
running foreground inference. The existing target rasterizer adaptively
approximates those cubics with quadratics before drawing. At reduced sizes the
preview instead shows the foreground-supported target core used by Game Asset
rendering.
For an originally opaque source, AA 0% is a final hard half-coverage cutout:
it clears lower model-alpha pixels and makes retained pixels opaque. AA 100%
is the full soft-alpha render. Diorama renders and caches those two endpoints
for the active target size, then every intermediate integer percentage blends
their premultiplied linear RGBA values. The endpoints remain byte-exact, while
each one-percent change is gradual rather than restoring the model's full soft
edge at 1%. This happens after contour painting, without resampling, and
the AA 0% retained pixels keep their RGB, so the hard foreground remains
halo-free and its contours retain their Bresenham pixel edge. Intermediate
settings blend RGB together with alpha. Sources with explicit transparency
are never binarized at AA 0%; their endpoint alpha and intermediate blending
remain fractional.

For the Game Asset foreground-ink path, AA 0% also reaches a contour out by at
most one target pixel where an original core touches the four-connected
foreground silhouette rim. The added pixel inherits its adjacent original
contour's foreground donor; it never samples exterior background. Existing
core coverage, contour owners, colors, and the finished silhouette remain
unchanged. The full-AA endpoint has no added rim coverage, so the cached
endpoint blend progressively tapers this AA-0 edge reach as AA increases.
When changing the Git pin, update `build-aux/cargo-sources.json` for offline
Flatpak builds. The scaler accepts positive dimensions no
larger than the source, including rectangular images and independent axis reductions.
Identity scaling preserves the source exactly. Ordinary Nearest, Bicubic and
Lanczos resampling remain separate methods.

## Source analysis

Five Gaussian scales and four scan directions find dark ridges. Supported local
samples fit quadratic patches. Source rasterization and topology-preserving thinning
provide a trace graph. Direction-compatible continuations merge across small junction
loops; supported endpoints bridge small gaps. Ambiguous branches remain separate.
Each ordered trace is then fit as bounded cubic geometry with shared tangent directions
at joined pieces; real corners and shared junctions remain fixed fit anchors. Target
rasterization adaptively converts that geometry to quadratics.

Source ink is the detected shoulder-supported footprint plus the thinned centerline.
Each visible detector sample near a trace measures the contiguous ink span along its
normal in quarter-pixel steps, capped at 32 source pixels on each side. The mean of
all supported measurements gives that contour's source thickness. Analysis and widths
are cached once per immutable source. A live Diorama session also caches one successful
BiRefNet foreground estimate, keyed by that immutable source. It keeps two completed
renders for the active target size and Darken setting at AA 0% and 100%; changing
AA blends that pair without rerunning contours, fill, or BiRefNet. The existing preview debounce and
cancellation discard superseded work. Changing Darken rebuilds that endpoint pair
for its new contour RGB, while retaining the prepared contours and cached foreground.

## Target contours

A contour must project to more than three distinct target Bresenham pixels; one,
two, and three projected pixels are rejected. The same source trace can therefore
drop as the target becomes smaller. Rejection removes its explicit contour redraw
and its halo evidence, while its original pixels can still contribute to the ordinary
resampled Lanczos fill. Target coordinates use pixel-center alignment independently on each axis.
The fitted cubic traces preserve shared tangent directions, then are adaptively
approximated as quadratics for the existing rasterizer. Each retained source contour
is rasterized with canonical-direction Zingl/Bresenham quadratics and thinned in
its own target bounding box. Cleanup cannot replace a black contour connection
with a route through a different, green contour. Only digital core construction
rounds coordinates; geometric AA samples retain the subpixel curve coordinates.
At crowded reductions, source strength and width rank competing parallel detail while
outer alpha edges retain priority, including holes and gaps in the background-removed
foreground, keeping the silhouette readable before interior detail.

Rasterization assigns contour ownership before color lookup. Core pixels always
win over another contour's fringe. At a genuine core overlap, the greater intrinsic
strength (using the contour-local core length), then greater local core length,
then lower source ID wins. This is explicit occlusion, not a claim that two colors
can occupy the same pixel. Fringe overlaps choose the greater weighted coverage.
Colors come from the nearest visible original-source donor of the selected contour,
never from an unconstrained nearest neighbor. Shared model assignments still choose
the longest retained source trace. Overlapping patches never stack opacity.

The foreground-ink rendering path can extend only a zero-coverage target pixel
that is both on the four-connected foreground rim and eight-neighbor adjacent to
an original supported core. It propagates that core's owner and supported
foreground donor. The normal AA-0 hard cutout must already retain the destination,
which prevents this bounded coverage change from enlarging the finished silhouette
or moving internal contours. This is a rendering coverage adjustment; it does not
alter detector samples, fitted curves, source widths, or color selection.

Let `L` be the contour's final owned core-pixel count and `T` its mean source width:

```text
opacity = 0.95 + 0.05 * sqrt(clamp((L - 4) / 28, 0, 1) * clamp((T - 1) / 5, 0, 1))
```

The result ranges from 95% to 100%, reaching full opacity at 32 final pixels and six
source pixels of average thickness. Without width measurements the contour uses 95%.
Intrinsic opacity multiplies AA coverage once, after final ownership and core-length
measurement. It is separate from the per-pixel AA allowance below.

That intrinsic weighting remains the full-AA contour behavior. In the Game Asset
foreground-ink path, AA 0% instead uses unit strength for owned core paint, then
smoothly restores the calculated 95%–100% intrinsic strength toward AA 100%. Thus a fully
covered AA-0 core is solid; previously its 255 coverage could still become soft
when the later intrinsic multiplier was below one.

### Core-preserving manual antialiasing

Inspired by section 7 of Inglis, Vogel and Kaplan's
[Superpixelator paper](https://doi.org/10.1145/2486042.2486044), a 0.75-target-pixel
stroke is sampled on an 8-by-8 subpixel grid. Per-pixel sample bit masks are unioned
across all patches of the same contour, so duplicates do not darken a stroke.
Local contiguous row/column coverage sums estimate apparent thickness; the smaller
sum normalizes each sample. A disconnected piece or the opposite side of a loop
does not contribute merely because it occupies the same row or column.

The **AA** and **Darken** numeric spinners appear together at the right of the
first control row, only for Game Asset. AA defaults to **50%** and Darken to
**20%**; neither uses a slider. Darken scales only painted contour RGB in
displayed sRGB: 0% retains the detected foreground donor color and 100% uses
black, without changing fill or alpha. At AA 0%, a fully covered painted core is
opaque, so Darken 100% produces solid black ink. Both values are captured in each scale
operation, so Apply, undo/redo, export, and replay do not depend on later
preference changes. One AA intensity controls both core attenuation and the
geometric outer fringe:

| AA setting | Minimum core coverage | Outer smoothing intensity |
|---|---|---|
| 0% | 100% | Off |
| 50% (default) | 95% | 50% |
| 100% | 90% | 100% |

For integer percentage `p`, the core floor is `255 - floor(255*p/1000)`.
AA coverage, not RGB colors, is quantized. Selection retains the original core
217/236/255 and fringe 0/43/85 thresholds. For selected core byte `c`, output is
`255 - floor((255-c)*p*255/38000)`; fringe byte `f` becomes `round(f*p/100)`.
Thus 100% outer smoothing means the full sampled fringe, not solid opaque pixels
outside the core. Fully opaque cores stay opaque at every setting. At default
50%, core levels **243, 249, 255** and fringe levels **0, 22, 43** remain byte-exact
with the preceding fixed recipe. The floor applies after quantization to every core pixel,
not the contour's average. Clean horizontal, vertical and 45-degree digital runs,
including their endpoints, stay at 255; only pixels near bends receive fringe AA.
Fringe is confined to the eight-neighbor core band and never repaints another core.

This is an adaptation, not a literal implementation of the paper's global opacity
normalization, shape realignment or partial sorting. The protected core and bounded
fringe can add apparent thickness; we do not dim the core below its agreed floor to
compensate. A full-strength contour has at most 10% AA opacity loss at maximum AA; deliberately
shorter or thinner contours retain their separate intrinsic weighting. The bound is on ink
coverage, not encoded RGB brightness or the alpha of an opaque composited image.

## Lanczos fill and composition

The pipeline first obtains BiRefNet's foreground estimate, then directly resamples
that isolated source once with Lanczos3. Contour geometry and source-width measurements
are prepared before foreground removal. The painted contours retain that original
geometry, while their colors come only from alpha-supported aligned foreground donors
and use the operation's displayed-sRGB Darken setting (80% brightness at the default
20% darkening). Contours without a supported donor are not painted, so transparent-model
background RGB cannot reappear as white fringe. The
fill converts straight linear RGBA to premultiplied linear RGBA before filtering, then
unpremultiplies only guarded target pixels. Transparent source pixels therefore
contribute no hidden RGB. The retained-ink mask is not subtracted from this base.

For an originally opaque source in the foreground-ink path, a narrow post-composite
fringe repair can replace RGB only on an unsupported, non-core target pixel whose
pre-repair foreground alpha is above zero and below 25%, and which is within two
target pixels of an original core donor. This reaches fringe just outside the
existing one-pixel rim while still using frozen original-core donors. It preserves every nonzero baseline alpha
byte, clears accidental RGB where that byte is zero, and leaves transparent originals
alone. This does not remove halos generally or change the fill, contour geometry,
ownership, or silhouette.

A bounded halo pass can tone down dark retained-ink bleed immediately adjacent to a
drawn contour core. It estimates nearby fill from non-ink samples with a positive
triangle filter; it never changes core pixels or alpha, only operates in the one-pixel
core neighborhood, and caps the encoded RGB correction at 12/255.

Explicit source alpha is authoritative: internal color ridges cannot erode
opaque source-supported parts of a transparent asset. For opaque flat backgrounds,
thin connected components without an eroded fill interior retain their endpoints.
These fix the clipped-limb and off-grid-line regressions shared with Sprite Studio.

Silhouette support and intrinsic alpha then compose the fill in linear light before
the unchanged opacity contours. Fully transparent output pixels encode as transparent
black. Game Asset has one recipe with adjustable AA, no RGB
palette quantization and no alternate fill path. The bounded `GameAssetAa` value
belongs to `Resampling::GameAsset`, so preview, Apply, export and undo/redo carry
the same setting. The last selected percentage is stored in `game-asset-aa`
when the installed GSettings schema supports it; older/missing schemas default
to 50%. Switching to another method hides AA without forgetting the window's value.

## Bounds and validation

Source analysis and target work check cancellation throughout Gaussian rows, curve
fitting, tracing, merging, thinning, source conversion, resampling, halo correction,
and composition. Cancelled
work is never committed as a target result. Heavy work stays outside the session lock.
A conservative four-GiB working-set envelope and a 500,000 detector-candidate limit reject
unbounded inputs with application errors instead of silently selecting another method.
The renderer processes one contour bounding box and one quadratic's flattened
segments at a time. The immutable topology lookup tables are shared across calls.

The reviewed 800×800 source in `game_asset/fixtures/` remains a regression input.
The older `elf-{size}.png` and `elf-aa-{size}.png` targets are historical comparison
artifacts from the prior fill and are not production-output oracles. Their provenance
and the drake check are recorded in [Manual AA validation](game-asset-aa.md).
Independent tests cover analytic horizontal coverage, a separate dense distance
oracle, the 243 core floor, half-opacity fringe thresholds, clean digital runs, duplicate/reversed patches,
neighboring-contour protection and locked color donors. Fill tests independently
reproduce premultiplied Lanczos sampling and cover transparent RGB, finite output,
halo locality/core/alpha/cap, rectangular preview/document equivalence, cache reuse,
cancellation and dimensions.

The Gaussian derivatives share identical vertical passes, use contiguous SIMD
loops and precompute reflected row indices. An exact 256-entry sRGB decode table
avoids repeated transfer-function evaluation. Scalar reference tests check the
Gaussian results bit for bit. Reproducible release
benchmarks and tradeoffs are documented in [Game Asset performance](game-asset-performance.md).

Historical fill methods, settings UI, and comparison artifacts are preserved on
the `experiment` branch and its sibling `diorama-experiment` worktree. They are
not part of the application or this branch's source tree.
