# Game Asset scaling

Game Asset scaling vendors the `asset-scaler` baseline at `cff93b5`. This change
ports only the clean vector extraction from `b6bc785`, plus Diorama's local
contour-opacity and pen-AA integration. Diorama adapts cancellation and errors:
live previews use a cached `Session`, while document rendering calls the same
pipeline through `resize`. A scale operation stores both AA strength and contour
opacity, so Apply, undo/redo, and export retain the selected result. A separately
run foreground estimate can make preview and committed pixels differ slightly.

The pipeline prepares contours from the untouched source, obtains a foreground
estimate for the Lanczos fill, then composites the original-source contour
geometry over that fill. Explicit source alpha remains authoritative. Identity
scaling preserves the source exactly; Game Asset scaling otherwise accepts only
positive output dimensions no greater than the source dimensions.

## Clean vector extraction

Five Gaussian scales and four scan directions find dark ridges. Aligned ridge
samples joined by continuous ink collapse to the darkest representative, avoiding
parallel traces in a wide outline. Supported samples fit quadratic patches;
rasterization fills narrow overlapping slivers and topology-preserving thinning
yields a trace graph. Spurs up to four pixels are pruned unless they continue a
longer branch through a junction. Direction-compatible continuations merge across
small junction loops, and supported endpoints bridge small gaps. Ordered traces are
fitted as bounded cubics with shared tangents at joins, while real corners and shared
junctions remain fixed anchors. The target rasterizer adaptively converts the cubics
to quadratics.

Each source trace measures its contiguous ink span along the local normal in
quarter-pixel steps. The mean supported span gives its source thickness. Analysis,
widths, and one successful foreground estimate are cached for each immutable source.
An ordinary target contour needs more than two distinct Bresenham pixels. Foreground
canonicalization uses its own minimum of three pixels. Core ownership is resolved
before donor lookup, so overlaps do not stack coverage and contour colors come only
from supported original-source foreground donors.

## Contour opacity and AA

The **AA** and **Opacity** numeric spinners appear for Game Asset scaling. AA is
0–100% and defaults to 50%. Opacity is 0–100% and defaults to 100%. Opacity 0%
adds no contour paint; 100% composites the full detected contour over the existing
fill. It changes contour coverage, including alpha when compositing over partial
alpha, and never darkens or recolors a donor. The opacity setting is stored in
`game-asset-contour-opacity`; the former darkening preference is not read as
opacity.

AA uses the same softened edge curve as the pen tool. The AA percentage scales
that contour edge coverage: 0% gives hard contour edges and 100% gives the full
pen-style softened coverage. Diorama renders and caches the 0% and 100% AA
endpoints for one target size and opacity; intermediate AA values blend that pair
in premultiplied linear RGBA. Changing AA reuses the pair, while changing target
size or opacity rebuilds it. Cancelled work never enters the cache.

The contour inspection preview at source size shows the cleaned fitted traces
without foreground inference. At reduced sizes it shows the foreground-supported
target contour core.

## Verification scope

Tests cover vector topology, opacity 0/50/100 compositing, donor-color retention,
unrelated fill preservation, AA endpoints, bounded endpoint-cache reuse,
cancellation, dimensions, preview parity, and persisted operation options.
