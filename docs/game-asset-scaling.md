# Game Asset scaling

`src/tools/scale/game_asset::Session` is the single production implementation used
by the live preview and document rendering. It accepts positive dimensions no
larger than the source, including rectangular images and independent axis reductions.
Identity scaling preserves the source exactly. Ordinary Nearest, Bicubic and
Lanczos resampling remain separate methods.

## Source analysis

Four Gaussian scales and four scan directions find dark ridges. Supported local
samples fit quadratic patches. Source rasterization and topology-preserving thinning
provide a trace graph. Direction-compatible continuations merge across small junction
loops; supported endpoints bridge small gaps. Ambiguous branches remain separate.

Source ink is the detected shoulder-supported footprint plus the thinned centerline.
Each visible detector sample near a trace measures the contiguous ink span along its
normal in quarter-pixel steps, capped at 32 source pixels on each side. The mean of
all supported measurements gives that contour's source thickness. Analysis and widths
are cached once per immutable source; one target result is cached per session.

## Target contours

A contour must project to more than three distinct target Bresenham pixels. Target
coordinates use pixel-center alignment independently on each axis. Local robust
quadratic smoothing uses a 6.4-target-pixel sigma tapered toward zero at source scale;
for unequal scale factors it uses the smaller factor. Directional digital coverage,
topology-preserving thinning and tight AA provide one coverage value per target pixel.
No silhouette expansion or integer rounding of target curve controls is applied.

Every painted pixel uses one nearest smoothed-model sample for both contour ownership
and original-source ink color. Shared model assignments choose the longest retained
trace, with stable ID ties; overlapping patches never stack opacity.

Let `L` be the contour's final owned core-pixel count and `T` its mean source width:

```text
opacity = 0.6 + 0.4 * sqrt(clamp((L - 4) / 28, 0, 1) * clamp((T - 1) / 5, 0, 1))
```

The result ranges from 60% to 100%, reaching full opacity at 32 final pixels and six
source pixels of average thickness. Without width measurements the contour uses 60%.
Intrinsic opacity multiplies tight-AA coverage; the AA transition has a 65% minimum
core and a 35% maximum fringe before this multiplication.

## Median fill and composition

RGB is decoded to linear light once. The destination footprint uses scale-widened,
separable Catmull-Rom taps. Source alpha supplies color weights; median RGB is selected
independently per channel from the positive lobes. Alpha is the signed bicubic projection.

Only fill pixels within a one-pixel, eight-connected neighborhood of actual nonzero
paint coverage can remove ink samples. Fully opaque painted pixels need no correction;
partially painted cores may receive corrected fill beneath the contour. Source-mask
ownership is assigned to nearest traces. Discarded traces never subtract from lookup;
ownership ties involving a discarded trace preserve the source sample.

Outside that halo each pixel retains its original median projection exactly. If ink
removal leaves no positive fill support, use the original median. Fill alpha is never
changed by correction. Composition uses linear-light premultiplied source-over and
encodes fully transparent pixels as transparent black.

## Bounds and validation

Source analysis and target work check cancellation throughout Gaussian rows, curve
fitting, tracing, merging, thinning, footprint construction and sampling. Cancelled
work is never committed as a target result. Heavy work stays outside the session lock.
A conservative one-GiB working-set envelope and a 500,000 detector-candidate limit reject
unbounded inputs with application errors instead of silently selecting another method.
The renderer processes one quadratic's flattened segments at a time.

The reviewed 800×800 source and 128/160/256 target images in `game_asset/fixtures/` are
production regression fixtures. Tests require exact RGBA parity with the selected
median/halo/tight-AA/opacity recipe, cover rectangular preview/document equivalence,
cache reuse, cancellation, transparency, dimensions and contour continuity.
