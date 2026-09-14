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

## Biharmonic fill and composition

The source ink footprint is assigned to its nearest traces. Only ink belonging to
contours retained at the target size is removed. Discarded traces and ownership
ties involving a discarded trace keep their source samples. All retained ink is
removed before shrinking, including the color beneath partially opaque contours.

Repair solves the biharmonic equation on the masked source pixels in premultiplied
linear RGBA. Its operator is the square of the four-neighbor graph Laplacian, with
reflecting image boundaries. Known samples form fixed boundary conditions; known
transparent pixels contribute transparent RGBA, never their hidden RGB. The
original values of masked pixels are unavailable to the solver.

A sparse system with at most thirteen coefficients per row is solved by
Jacobi-preconditioned conjugate gradients. The Euclidean residual must be at most
`max(1e-12, 1e-12 * ||rhs||)` for each channel, checked with an explicit matrix
product before accepting convergence. Work is limited to 4,096 iterations per
channel. Missing boundary data or a failed solve produces an application error;
no alternative fill is silently substituted.

Repaired channels are clipped to the known source range, then into valid
premultiplied RGBA. All unmasked source samples remain unchanged. This follows
[scikit-image's biharmonic reference](https://scikit-image.org/docs/stable/api/skimage.restoration.html#skimage.restoration.inpaint_biharmonic),
including its channel-range clipping, with an additional premultiplied-alpha
constraint. Python and scikit-image are not application dependencies.

Two separable passes perform exact fractional rectangular area integration before
linear-light source-over composition with the unchanged opacity contours. Alpha
is repaired and projected together with color. Fully transparent output pixels
encode as transparent black. Game Asset has one fixed recipe and no settings
panel, persisted tuning keys, palette quantization or contour-halo alternative.

## Bounds and validation

Source analysis and target work check cancellation throughout Gaussian rows, curve
fitting, tracing, merging, thinning, sparse-system construction, solver iterations and area sampling. Cancelled
work is never committed as a target result. Heavy work stays outside the session lock.
A conservative one-GiB working-set envelope and a 500,000 detector-candidate limit reject
unbounded inputs with application errors instead of silently selecting another method.
The renderer processes one quadratic's flattened segments at a time.

The reviewed 800×800 source and 128/160/200 target images in `game_asset/fixtures/`
are production regression fixtures. The expected targets are the selected
biharmonic outputs of the independent Python comparison, preserved before this
Rust implementation was written. Tests require exact RGBA parity at all three
sizes. Separate numerical tests cover source/image boundary behavior against
scikit-image, cubic texture continuation, masked-source poisoning, transparency,
fractional area conservation, rectangular preview/document equivalence, cache
reuse, cancellation and dimensions.

Historical fill methods, settings UI, and comparison artifacts are preserved on
the `experiment` branch and its sibling `diorama-experiment` worktree. They are
not part of the application or this branch's source tree.
