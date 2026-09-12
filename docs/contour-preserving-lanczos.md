# Contour-preserving Lanczos downscaling

Status: proposed algorithm, version 1, 2026-09-12. This document specifies a
prototype and its acceptance gates; it does not claim that the algorithm has
been implemented or visually validated.

## Intent and scope

Resize ordinary image content with Lanczos-3, while recovering selected thin
strokes and sharp boundaries from source-resolution evidence. Reconstruct the
protected features on the destination grid with hard pixel selection. Do not
skeletonize a binary source mask.

The [source conversation](https://chatgpt.com/share/6aa5b00e-dc8c-83eb-830a-d4cb85ee999b)
motivates separate edge and ridge channels, destination-space suppression, and
adaptive hysteresis. The detailed rules below are design decisions added here.
They are a proposed synthesis, not an established variant of Lanczos.

Version 1 supports positive destination dimensions no larger than the source
on either axis, including nonuniform reductions and one unchanged axis. Equal
dimensions return the original bytes. Enlargement is deferred: isolated source
feature samples would need continuous segment reconstruction to avoid gaps.

The output is a continuous-tone image with selected hard contours. It is not a
palette-preserving pixel-art conversion. The protected overlay has no coverage
antialiasing. Lanczos interiors and existing source transparency remain smooth.

## Corrections to the original proposal

| Problem | Decision |
| --- | --- |
| A threshold determines both survival and thickness. | Separate feature selection from spatial suppression. Thresholds control survival; destination geometry controls width. |
| A gradient of a thin stroke gives two boundaries. | Detect stroke centers with a ridge/valley detector. Associate and suppress their redundant edge responses. |
| Max pooling retains strength but loses position. | Transport strength together with subpixel position, normal, color, kind, and source identity. Use a localized positive splat. |
| Averaged angles or signed gradients can cancel. | Carry the winning candidate's attributes. Treat normals modulo pi for orientation tests, retaining the directed normal for side colors. |
| One NMS pass guarantees Bresenham output. | It does not. Define a local thinness invariant and test connectivity separately. No arbitrary gap filling. |
| Any source stroke can become visibly one pixel wide by drawing over Lanczos. | Only the replacement mask is constrained. A broad or blurred stroke can remain underneath. Strict stroke replacement needs background reconstruction. |
| Every contour survives at every reduction. | Impossible when features collide. Use deterministic priority and expose collision counts. |
| Every boundary has an ink color. | A boundary separates two regions; select a supported side color. A ridge has a center color. Never default all contours to black. |
| The same normal can be used after nonuniform scaling. | Transform normals by the inverse transpose of the scale matrix. |
| Local contrast alone is a noise threshold. | Include an absolute response floor; otherwise tiny noise in a flat region can pass. |

## Observable contract

1. Source pixels are immutable; output dimensions are exact.
2. With protection disabled, return the base defined below. With an empty
   accepted feature mask, output must equal that base byte for byte.
3. Outside the accepted replacement mask, output must equal the base byte for
   byte. Version 1 does not perform implicit halo removal or inpainting.
4. A selected ridge pixel uses a source-supported stroke color, not a blend of
   two unrelated feature colors. Region boundaries use a source-supported side.
5. Zero-alpha hidden RGB never affects the base, detector, priorities, or output
   visible colors. Identity is the exception: it preserves all original bytes.
6. Each destination pixel has at most one replacement owner. Repeated execution
   with the same algorithm version and settings is deterministic on the reference
   CPU path, including plateau and collision decisions.
7. For an isolated straight feature with a stable orientation, the accepted
   mask has no pair of adjacent pixels that compete across its normal, as
   defined in the destination suppression stage. This is a discrete thinness
   condition, not a claim of constant Euclidean stroke width at every angle.
8. Detection recall, connectivity, junction shape, and artistic quality are
   acceptance measurements, not universal guarantees. A 1x1 destination can
   retain one color, not every source feature.

### Meaning of “one pixel”

The target is an 8-connected, non-antialiased staircase for an isolated accepted
line. A diagonal pixel chain is acceptable. The algorithm must not dilate it or
paint both sides of the same detected ridge as parallel strokes. A junction may
occupy more than one local pixel; it cannot satisfy the isolated-line condition
in every direction simultaneously.

One-pixel **mask** thickness and one-pixel **visible** thickness are different.
For example, reducing an 8-source-pixel black bar by 2x leaves approximately a
4-pixel bar in the base. Adding a 1-pixel center cannot erase the other pixels.
This case must be reported as residual base width, not counted as successful
one-pixel stroke reconstruction.

## Input, coordinates, and numeric conventions

Input is straight-alpha RGBA8 in known sRGB, already orientation-normalized.
An integration adapter must color-convert tagged non-sRGB content first and
carry consistent profile metadata. Untagged input follows the application's
explicit untagged-color policy. Do not merely relabel non-sRGB bytes.

Let source dimensions be `Ws,Hs`, destination dimensions `Wd,Hd`, and
`sx=Wd/Ws`, `sy=Hd/Hs`. Integer coordinates denote pixel centers.

```text
source point q -> destination point T(q):
    ((qx + 0.5)*sx - 0.5, (qy + 0.5)*sy - 0.5)

destination center p -> source point U(p):
    ((px + 0.5)/sx - 0.5, (py + 0.5)/sy - 0.5)

source unit normal n -> destination unit normal nd:
    normalize(nx/sx, ny/sy)
```

All geometry and reference computations use f64. Pixel indexing and allocation
sizes use checked integer arithmetic. Reject zero dimensions, invalid buffers,
unsupported enlargement, nonfinite parameters, and allocation-budget overflow
with explicit errors. Cancellation must not return a partial successful image.

Use clamped source extension for convolution and bilinear sampling. Feature
classification requires both flank sample positions inside the source rectangle
`[0,Ws-1] x [0,Hs-1]`; reject candidates lacking that evidence. Thus the image
frame itself is not detected as an artificial contour. Border-touching features
may rely on the base alone.

Sort all equal-ranked items by increasing stable source ID: source row, source
column, detector scale index, scalar-channel index, then kind (`ridge`, `edge`).
Where a destination tie remains, use increasing row then column. Comparisons
use finite values without an approximate-equality epsilon. These rules can
introduce a deterministic half-pixel phase preference; do not promise exact
rotation/reflection equivariance on tied inputs.

## Stage 1: base resampling

Decode sRGB RGB to linear RGB `c`; alpha `a` is linear coverage. Form
premultiplied values `P=(a*c.r,a*c.g,a*c.b,a)` in `[0,1]`.

```text
sinc(t) = 1                         if t == 0
          sin(pi*t)/(pi*t)          otherwise
L(t)    = sinc(t)*sinc(t/3)          if abs(t) < 3
          0                        otherwise

hx = max(1, 1/sx)
wx(i,p) = L((U(p).x-i)/hx)
normalized_wx = wx / sum_i(wx)
```

Apply the analogous y kernel separably to all four channels of `P`. Enumerate
all integer taps within the support, clamping their source indices at borders;
repeated border indices retain their individual weights. Normalize each axis
after enumerating taps. Treat a nonfinite or near-zero denominator
(`abs(sum)<1e-12`) as a numerical error. Do not silently switch algorithms.

Use float intermediates, without clipping between horizontal and vertical
passes. At finalization clamp alpha to `[0,1]` and each premultiplied RGB
component to `[0,a]`, then unpremultiply when `a>0`. Encode sRGB and round each
channel with `floor(255*v+0.5)`. If quantized alpha is zero, output `(0,0,0,0)`.
Lanczos can still ring within the valid range; clipping is not halo removal.

Normalized Lanczos reconstruction and its constant-reproduction issue are
described by [Getreuer, Linear Methods for Image Interpolation](https://www.ipol.im/pub/art/2011/g_lmii/).
The widened reduction support and the RGBA contract here are explicit choices
for this algorithm. Do not assume an existing RGBA8 resizer implements them.

## Stage 2: source feature candidates

### Shared evidence and detector scale

Use four scalar channels, the components of `P`, so colored structures and
alpha structures are not lost through a luminance-only projection. A candidate
is proposed independently by a scalar channel, but its side/center colors are
always complete premultiplied RGBA samples. Channel responses are never summed
with signs, which would cancel opposing edges.

Default Gaussian scales in source pixels are `sigma={0.6,1.0,1.6,2.5,4.0}`.
For each scalar channel convolve with a separable normalized sampled Gaussian,
radius `ceil(3*sigma)`, with weights proportional to
`exp(-i*i/(2*sigma*sigma))`. Call the result `f`.

For a precise initial reference, compute derivatives from `f` using centered
differences: `fx=(f(x+1)-f(x-1))/2`, `fxx=f(x+1)-2*f(x)+f(x-1)`, and analogously
in y; `fxy` is the four diagonal samples with signs `+,-,-,+`, divided by four.
Use the same clamped extension. Optimized derivative-of-Gaussian kernels are a
later detector revision, not automatically equivalent to this definition.

This scale bank is bounded. It is intended to propose narrow structures at
several widths; it does not guarantee detection of every 1–10 pixel bar or
arbitrarily broad strokes. Width coverage must be measured on the fixture sweep.

### Edge channel: boundaries between regions

At each source center with nonzero gradient, set `n=normalize(fx,fy)` and
`m=hypot(fx,fy)`. Bilinearly sample the magnitude image at `q-n` and `q+n`.
Require `m>=m_minus` and `m>m_plus`. This localizes an edge response before
transport; it does not thin a binary region or alter topology.

Fit a three-point parabola along `n`:

```text
denom = m_minus - 2*m + m_plus
delta = 0.5*(m_minus-m_plus)/denom
q_edge = q + clamp(delta,-0.5,0.5)*n
response = sigma*m
```

If `denom>=-1e-12`, use `delta=0`. Retain the directed normal, samples of the
original `P` at `q_edge +/- d*n`, and their nearest source-pixel coordinates,
where `d=max(1,2*sigma)`. The normal points toward increasing detector-channel
value; this does not necessarily mean increasing perceived brightness.

### Ridge channel: thin strokes, both dark and bright

Form the Hessian `[[fxx,fxy],[fxy,fyy]]`. Choose eigenvalue `lambda_n` with the
largest absolute value (the algebraically larger value on a tie) and its unit
eigenvector `n`; the other is `lambda_t`.
Reject degenerate eigenvectors and `abs(lambda_n)<=1e-12`. Define

```text
anisotropy = 1 - abs(lambda_t)/abs(lambda_n)
delta = -dot((fx,fy),n)/lambda_n
q_ridge = q + delta*n
response = sigma*sigma*abs(lambda_n)*anisotropy
```

Require `anisotropy>=0.6`, `abs(delta*nx)<=0.5`, and `abs(delta*ny)<=0.5`.
Canonicalize the undirected ridge normal so `nx>0`, or `nx==0 && ny>=0`.
A positive `lambda_n` indicates a valley in this scalar channel; a negative
value indicates a ridge. This sign convention fixes a potential dark/bright
reversal in an implementation based on a negated second derivative.

Sample the original detector channel at `q_ridge` and `q_ridge +/- d*n`.
Require the two center-minus-flank differences to have the same nonzero sign,
consistent with the curvature: negative for a valley, positive for a ridge.
Their smaller absolute difference must be at least one quarter of the larger.
Also require center-to-each-flank distance in full `P` space to exceed the
flank-to-flank distance. These conservative gates reject many step responses;
they also reject asymmetric outlines. Record that loss rather than inventing
a stroke on ambiguous evidence.

Subpixel extrema from directional derivatives follow the general approach of
[Steger's curvilinear-structure work](https://mv.in.tum.de/_media/members/steger/publications/1996/fgbv-96-03-steger.pdf).
The finite-difference detector and flank gates here are a simpler proposal;
they do not implement Steger's full width estimation or asymmetry correction.

### Adaptive score, provenance, and source duplicates

For either kind, compute `C=max(center,minus,plus)-min(center,minus,plus)` from
the original scalar-channel samples at the candidate center and flanks. Set

```text
high = max(0.02, 0.25*C)
low  = 0.5*high
z    = response/high
```

Discard candidates with `z<0.5`. Values `z>=1` can seed hysteresis; weaker
values can only continue a seeded feature. The constants are prototype
defaults in normalized linear units, not calibrated perceptual thresholds.
The absolute low floor is `0.01`, including when `C==0`.

A candidate carries `{id, kind, q, n, sigma, channel, polarity, response, C, z,
center_rgba, minus_rgba, plus_rgba, source_color_coordinates}`. Do not replace
this record with a single edge-magnitude bitmap.

For each kind, greedily deduplicate candidates in descending `z`, then ascending
ID. A lower-ranked candidate is a duplicate when its source center is within
0.75 pixels of a retained candidate, their unoriented normals differ by at most
22.5 degrees, and their center `P` colors have Euclidean distance at most 0.10.
Keep distinct nearby colors and orientations as competing evidence. Use spatial
bins; do not compare every pair of candidates in the image.

## Stage 3: destination feature transport

Transform every candidate position and normal with `T` and the inverse-transpose
rule. Transform from the original source directly for every requested size.
Never detect on a previously resized output or repeatedly halve the feature map.

Maintain eight destination slots per pixel: two feature kinds times four
unoriented normal bins centered at 0, 45, 90, and 135 degrees. Assign the nearest
bin modulo 180 degrees; a bin tie chooses the lower bin index. The full normal
and all provenance stay in the record; the bin is only a bounded storage index.

Splat a candidate at `u=T(q)` to the at most four surrounding destination centers:

```text
a(p,u) = max(0,1-abs(px-ux))*max(0,1-abs(py-uy))
a_max  = maximum a over those in-bounds centers
Z(p)   = z*a(p,u)/a_max
```

Ignore zero weights. Normalization ensures at least one destination center
receives the full score even at a half-pixel phase. This is positive winner
transport, not Lanczos filtering, probability averaging, or coverage estimation.
It avoids negative lobes and opposing-polarity cancellation in the feature map.

For each slot keep the candidate maximizing `(Z, z, -distance_squared(p,u))`,
then use the source-ID tie rule. Keep its attributes, not separately pooled
colors or normals. Count displaced candidates for diagnostics. Finite slots
deliberately lose evidence in crowded footprints; a pooling rule cannot remove
that resolution limit.

## Stage 4: destination suppression and hysteresis

### Spatial suppression

Process ridge slots first. Drop slot entries with `Z<0.5`. Process the remainder
in descending `(Z,z,-distance_squared(p,u))`, then source ID and destination
row/column. Accept an entry unless it conflicts with an already accepted ridge.
Entries at the same destination pixel always conflict.

For entries at distinct 8-neighbor pixels, let `e` be their normalized pixel
displacement. They conflict if their normals differ by at most 22.5 degrees
modulo pi and

```text
max(abs(dot(e,n1)), abs(dot(e,n2))) >= cos(pi/4).
```

This tests all eight neighbors, including axial neighbors of diagonal contours;
only testing `p +/- (1,1)` would miss adjacent diagonal lanes. Suppression spans
normal bins. Color does not waive this conflict: two closely spaced parallel
features may not both fit. Junctions with incompatible directions are exempt
from this cross-normal rule, but still compete for ownership of the same pixel.

The greedy rule is a specified discrete alternative to interpolated Canny NMS.
It enforces the stated local invariant, but can break a curved path. Do not
describe it as exact Bresenham rasterization or a connectivity guarantee.

### Hysteresis graph

Connect accepted entries only when they occupy distinct 8-neighbor pixels,
their source colors differ by at most 0.15 in `P` space, and their source
positions are at most `2*max(1/sx,1/sy)+1` pixels apart. Also require each
entry's destination tangent to be within 60 degrees of the displacement, using
absolute dot products to ignore tangent sign. Apply these tests symmetrically.

Keep a graph component only if it contains an entry with `Z>=1`. A strong
isolated entry may survive; there is no arbitrary minimum-length deletion.
Traversal uses a snapshot of accepted entries. Connectivity never promotes a
suppressed pixel and never generates an unobserved bridge. Tight corners and
weak endpoints may therefore be lost; measure them explicitly.

The separation of suppression and strong/weak connectivity is informed by
[Canny's standard stages](https://docs.opencv.org/4.13.0/da/d22/tutorial_py_canny.html).
The compatibility tests and constants here are additional design choices.

### Avoid drawing a ridge twice

Before processing edge slots, compare their source positions to the surviving
ridge records. Suppress an edge record as a ridge flank when its normal agrees
within 22.5 degrees, its tangent displacement from the ridge is at most
`max(1,ridge.sigma)`, its absolute normal displacement is at most
`2*ridge.sigma`, and either edge side color is within 0.10 of the ridge center
color in `P` space. This is a bounded association heuristic, not segmentation.

Run the same destination suppression and hysteresis on remaining edge entries.
When edge and ridge results request the same pixel, the ridge owns it. Resolve
any remaining same-pixel edge conflicts with the transport ranking. Do not
rerun hysteresis to grow new alternatives after collision removal. Collision
resolution may break a previously connected edge component; report that loss.

## Stage 5: choose replacement colors

For a ridge, use the original RGBA source pixel nearest to `q_ridge`, with
`floor(q+0.5)` for nearest-center rounding. For an edge at destination pixel
`p`, compute `dot(U(p)-q_edge,n)`: choose the plus flank if positive and the
minus flank otherwise. Copy the original source pixel nearest that flank
sample. The tie selects minus. This sharpens a side of a boundary without
inventing an outline color or always favoring the darker region.

Full `P` samples support detection and compatibility; source integer coordinates
support exact color selection. Canonicalize a copied zero-alpha pixel to zero
RGBA. Never force partially transparent source pixels to become opaque.

By default, only replace a pixel when its selected source pixel and both flank
source pixels have alpha 255, and the base pixel has alpha 255. Otherwise leave
the base unchanged. Perform this eligibility check before final suppression and
hysteresis, once the base and destination positions are known, so invisible
proposals cannot seed or suppress renderable ones. Alpha-derived candidates are
useful diagnostics but cannot alone create a new opaque silhouette in version 1.

This conservative default protects coverage alpha and avoids unexplained
silhouette expansion. Supporting translucent protected strokes or binary
silhouettes requires a separately specified compositing mode; it is not silently
enabled by an alpha gradient.

Replace eligible pixels, do not source-over them: the source image is already
present in the base, and source-over would composite its opacity a second time.
There is no opacity slider in the reference contract. A future blend-strength
control would interpolate premultiplied values and relax the hard-color contract.

## Reference control flow

```text
resize_contours(source, dimensions, settings, cancellation):
    validate_input_and_memory_budget()
    check_cancellation()
    if dimensions == source.dimensions: return clone(source)

    P = decode_srgb_and_premultiply(source)
    base = lanczos3_linear_premultiplied(P, dimensions)
    if protection_disabled: return base

    candidates = detect_edges_and_ridges(P, scale_bank)
    candidates = adaptive_score_and_source_deduplicate(candidates)
    slots = transport_eligible_winners(candidates, dimensions, source, base)

    ridges = hysteresis(spatial_suppression(slots.ridge))
    edges = remove_associated_ridge_flanks(slots.edge, ridges)
    edges = hysteresis(spatial_suppression(edges))
    owners = resolve_collisions(ridges, edges)
    output = replace_selected_source_colors(base, owners)

    check_cancellation()
    return output, diagnostics
```

Inside `transport_eligible_winners`, apply color/alpha eligibility to each
proposed splat **before** selecting its slot winner. Otherwise an ineligible
winner could discard a valid runner-up. Compute `a_max` from geometric in-bounds
support before eligibility filtering; an eligibility failure must not move a
feature peak to another destination pixel.

## Residual base strokes and future strict replacement

Version 1 intentionally leaves the base outside the replacement mask. It can
restore a crisp core to an attenuated narrow line but may leave Lanczos ringing
or a wider line underneath. Diagnostics must include a side-by-side base,
replacement mask, and final image so this is visible.

If the requirement is that the entire visible stroke become exactly one pixel,
use a later strict-replacement algorithm with all of the following obligations:

1. Establish the source stroke's extent and both surrounding regions from
   evidence; a Hessian response alone is not a segmentation mask.
2. Reconstruct the removed stroke's background before filtering. If the two
   sides differ, preserve their boundary rather than averaging them together.
3. Include the full Lanczos influence of the removed pixels; an arbitrary
   one-neighbor cleanup ring cannot remove all filtered contributions.
4. Filter the reconstructed base and redraw a destination-space path once.
5. Decline strict reconstruction when background or topology is ambiguous.

These are additional inference problems, especially at crossings and overlapping
objects. No unconditional strict-width mode is specified or claimed here.

## Integration and resource behavior

The current `src/tools/scale.rs` already dispatches ordinary Lanczos through
`fast_image_resize`. Its RGBA8 path must not be assumed equivalent to this
linear-light reference. Add this prototype as a distinct experimental method
when implementation is requested; keep the existing Lanczos behavior stable.

The current [Game Asset hybrid](game-asset-hybrid.md) uses bicubic plus a
silhouette-oriented contour path. It is related work, not an implementation of
this specification. Do not replace its palette, ink, or alpha contracts under
the same method name. Preview and document/export must eventually call one
shared implementation of the new method.

Cache source conversion and candidates by immutable source revision, working
color space, detector version, scales, and detection parameters. Destination
dimensions, transport, eligibility, suppression, and hysteresis belong to the
target-size cache. Evict within a measured memory budget. Never reuse a previous
size's replacement mask as source evidence.

Let `Ns=Ws*Hs`, `Nd=Wd*Hd`, `S` be the number of detector scales, `r` their
maximum Gaussian radius, and `F` the candidate count. A separable reference has
detector work `O(S*r*Ns)`, plus sorting `O(F log F)` for source deduplication.
Transport is `O(F)` with at most four splats per candidate; destination ranking
is `O(Nd log Nd)` for eight fixed slots, and local suppression/graph traversal
is `O(Nd)`. Source-space association queries require bounded spatial bins and
may still be output-sensitive in crowded bins; instrument them.

The separable base has work
`O(Hs*Wd*Kx + Wd*Hd*Ky)`, with `Kx=O(1/sx)` and `Ky=O(1/sy)` for reduction.
Account for the intermediate image and coefficient tables in memory estimates.
Streaming per-channel/per-scale detector buffers avoids retaining every
derivative image; candidate storage remains `O(F)` and slots remain `O(Nd)`.

Check cancellation between source/destination rows, convolution passes,
detector scales, and bounded batches of sorting/graph work. Standard monolithic
sorting is not automatically cancellable; use bounded sorted runs and a
cancellable merge if its measured latency exceeds the cancellation budget.
The implementation must estimate peak bytes before allocation and report a
resource error instead of silently changing detector scales or target size.

No speed target is inferred from the current bicubic or GPU benchmarks. First
measure source preparation, target reconstruction, and total preview/export
latency separately in release mode. Report hardware, threads, median, p95, peak
memory, and cold/warm behavior. Compare against the identical base workload.

## Acceptance plan

Build a small scalar reference and independently authored fixtures before SIMD,
GPU, parallel reductions, parameter tuning on artwork, or production routing.

| Fixture/check | Required observation |
| --- | --- |
| Identity, including arbitrary hidden RGB | Exact input bytes. |
| Constant opaque colors at odd and even sizes | Exact constant RGBA8 output; empty feature mask. |
| Protection disabled / no eligible candidates | Exact equality with this specification's base. |
| Random hidden RGB under alpha zero | Identical visible output and feature decisions. |
| Horizontal/vertical dark and bright isolated strokes | One center response rather than paired flank strokes; count destination mask width. |
| Widths 1,2,3,4,8,10 source pixels | Report detector recall and residual visible base width separately. |
| Equal-luminance colored strokes | Detection via RGB channels when the contrast gates are met. |
| Single step between uniform opaque regions | Edge classification; no spurious ridge; selected color belongs to the chosen side. |
| Ramps and weak noise below the absolute floor | No protected pixels. |
| Diagonals, shallow slopes, curves, T/X junctions | Check cross-normal conflicts, 8-connectivity, gaps, and filled 2x2 blocks; review junction exceptions. |
| Parallel lines at decreasing separations | Deterministic owner when unresolvable; no mixed feature color. |
| Weak continuation with/without a strong seed | Only the seeded compatible component survives. |
| Translucent stroke and alpha silhouette | Base retained under the default eligibility policy. |
| 1xN, Nx1, 1x1 output; odd dimensions | Valid result, finite values, no indexing error; no universal feature-survival assertion. |
| Nonuniform reduction | Verify inverse-transpose normals against transformed tangent orthogonality. |
| Repeated runs, worker schedules, tiled execution | Reference-equivalent ownership and RGBA output. |
| Cancellation, invalid dimensions, resource limit | Explicit failure, no partial result published. |

Sweep reductions `0.9,0.75,0.5,0.25,0.1` and several unequal x/y ratios. Use
explicit integer target dimensions and derive the actual ratios from them.
Sweep line phase through a whole source pixel; include quarter-pixel synthetic
geometry and asymmetric backgrounds. Do not tune only axis-aligned examples.

Export detector proposals, winning provenance, NMS mask, hysteresis components,
collision map, eligible replacement mask, base, and final output. Inspect full
images at intended size and nearest-neighbor enlarged crops. Measure feature
recall, broken components, duplicate parallel lanes, color/alpha differences,
and pixels changed outside the final mask (must be zero). PSNR/SSIM alone cannot
certify sparse contour quality.

The numerical defaults, finite scale bank, corner continuity, noise behavior,
and residual halos remain empirical risks. If the straight-line connectivity
fixtures fail, revise transport/suppression using their evidence; do not hide
failure with dilation, unconditional Bresenham bridges, or a relaxed width test.

Draft sanity checks passed for 12,341 one-dimensional normalized Lanczos
sample positions (source lengths 1–41 and every smaller positive destination
length), inverse-transpose normal/tangent orthogonality in three nonuniform
cases, and peak-preserving splats at four pixel phases. A geometry-only check
of 24 ideal straight-line cases (six slopes, four phases, dense exact feature
positions) produced connected masks after transport and suppression. These
checks bypass source detection, color eligibility, and full hysteresis; they
are algebra/geometry checks, not the acceptance suite or a quality benchmark.

## Decision before production

The first prototype answers a narrow question: does source-supported ridge/edge
reconstruction improve downscaled contours enough to justify its artifacts and
cost? Promotion requires reviewed image comparisons and a documented parameter
set. Strict visible one-pixel width, enlargement, translucent hard contours,
global topology preservation, and universal source-palette output are outside
this version's contract.
