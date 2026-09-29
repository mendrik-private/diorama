# Game Asset scaling

Game Asset scaling reduces an illustrated asset by multiplying line art over a
fill without ink contours, both generated at the target size. It only
downscales: targets must be positive and no larger than the source. The source
size returns the source unchanged.

1. **Generation.** The target is scaled up until its shorter side is 512
   pixels (never down); each side of that is rounded up to a multiple of 16.
   If this would exceed 1024 × 1024 pixels, the scale is lowered until it
   fits. The white-composited source is resized to that size with Lanczos3
   and is the reference for the local FLUX.2 [klein] 9B model, which generates
   two images at that size with one denoising step each: grayscale line art
   (white is no ink) and an opaque fill without ink contours. A resident
   worker keeps the model loaded while the Scale tool has Game Asset
   selected. See [local line-art
   generation](../README.md#local-line-art-generation) for setup, limits and
   caching.
2. **Cutout.** BiRefNet cuts the generated fill out at the generation size,
   so the alpha follows the shapes FLUX drew rather than the source's. The
   source itself is never cut out. The mask is cached with the generated
   pair; all three must be cached for a hit, and a cached pair whose mask is
   missing only runs BiRefNet again. Targets that share a generation size
   and reference share the mask.
3. **Center crop and reduction.** The line art, the fill, the mask and the
   white-composited source at the generation size (the reference) are
   cropped to the scaled target at the same offset,
   `floor((generated − scaled) / 2)` per axis. A target of 512 pixels per
   side or more is then complete and never resampled; a smaller one is
   reduced to the target, the line art with bicubic (Catmull-Rom) and the
   others with Lanczos3.
4. **Alpha.** The result's alpha is the reduced mask. If the source has any
   alpha below 255, its alpha is resized, cropped and reduced the same way,
   and caps the mask: `alpha = min(mask, source alpha)`.
5. **Colour restoration.** The de-inked fill drifts brighter and more
   saturated than the original, so it is pulled back to the reference from
   step 3 by an edge-aware (joint bilateral) correction. The fill's
   background colour is the per-channel median of the fill where alpha is 0
   (white if there is none), and a fill pixel is valid where alpha ≥ 250 and
   its colour is at least 40 (Euclidean, 8-bit RGB) from that background.
   The weight `w` is 1 on valid pixels whose unsharpened line art stays above
   200 throughout a 3×3 neighbourhood (away from ink), else 0. For each
   pixel p, over the neighbours q within 3σ:
   `k(p, q) = exp(−|q − p|² / 2σ²) · w(q) · exp(−‖fill(q) − fill(p)‖² / 2·25²)`,
   the correction is `Σ k · (reference(q) − fill(q)) / Σ k`, and it is
   scaled by `clamp(Σ k / (0.15 · Σ exp(−|q − p|² / 2σ²)), 0, 1)` over the
   full spatial window. So a pixel only takes the correction of similar
   colours around it, and a thin feature without similar samples, such as a
   strap across a tunic, is not tinted by its surroundings. The correction is
   measured on a working grid whose shorter side is at most 128 pixels (the
   target itself if smaller), with σ = 3 · (shorter grid side) / 128, at
   least 1: the fill, reference and weight are reduced to it with Lanczos,
   and the correction is interpolated back bilinearly and added to every
   pixel. The image is mirrored at its borders. The result is clamped and
   rounded once. The restoration does not depend on Strength and is cached
   with the layers.
6. **Unsharp mask.** The line art is sharpened like GIMP's Unsharp Mask with
   radius 1 and threshold 0:
   `clamp(x + amount · (x − blur(x)), 0, 255)`, rounded once, where `blur` is
   a Gaussian with σ = 1 truncated at 3σ and clamped at the image edges.
   **Strength** 0–100 (default 40) maps linearly to amount 0.5–3.0, so 40
   gives 1.5.
7. **Multiply.** The sharpened line art multiplies the cleaned fill's
   encoded 8-bit sRGB channels like GIMP's Multiply mode:
   `rgb = round(fill_rgb · line / 255)`. The result is straight alpha with the
   alpha from step 4, so the line art and fill never extend the silhouette.

The shared implementation is `asset_scaler::LineArtLayers`, which takes the
four target-sized layers, restores the fill once and keeps it with the alpha
and the line art's blur. Diorama keeps the layers of the latest target per
source, so a Strength change only re-sharpens and multiplies; another size
generates again or reads the disk cache. Cancelled work never enters a cache.

The **Strength** spinner appears only for Game Asset scaling and is stored in
the `game-asset-strength` setting. A scale operation stores its strength, so
preview, Apply, undo/redo, and export produce the same result.

**Show line art** previews the line art as it is multiplied: at the target
size and sharpened at the current Strength.

Generation runs in a resident worker process that loads the model once and
reads one job per line on its stdin. It starts in the background when the
Scale tool opens with Game Asset selected or when Game Asset is chosen there,
and stops when the tool closes, another method is chosen, after 10 minutes
without a job, or when Diorama exits. A cancelled job is asked to stop at its
next step; a worker that does not stop within 20 s, crashes or runs out of
memory is replaced by the next job. Prompts without cached embeddings are
first encoded on the CPU by a one-shot process of its own, so its memory is
returned before generation.

While a preview generates, a progress bar next to the spinner shows the
stage ("Generating line art…", then "Cutting out…"), the fraction done and
the estimated time left. The worker reports its load, its readiness and each
image's denoising step. Before the first step, the estimate is fitted to
measurements (`2.054 + 7.784·MP + 6.625·MP²` seconds per image, MP the
generation megapixels), plus the worker's start-up and load unless it is
loaded, a warm-up for a size it has not generated yet, the prompt encoding if
needed, and 2 s for the cutout, scaled by a calibration factor that smooths
measured ÷ predicted run times of this computer, the cutout included. After
the first image's step it scales the model by the measured time. A cached
pair whose cutout is missing shows only the cutout stage.
