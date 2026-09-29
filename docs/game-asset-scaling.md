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
2. **Center crop and reduction.** Both images are cropped to the scaled
   target at the same offset, `floor((generated − scaled) / 2)` per axis. A
   target of 512 pixels per side or more is then complete and never
   resampled; a smaller one is reduced to the target, the line art with
   bicubic (Catmull-Rom) and the fill with Lanczos3.
3. **Alpha.** BiRefNet removes the background. The extracted foreground is
   reduced with Lanczos3 in premultiplied linear light; the result's alpha is
   the foreground's silhouette coverage at the target size times its
   intrinsic alpha, with a fixed 50% edge softness. Explicit source alpha
   remains authoritative. The reduction's colour is not used.
4. **Defringe.** The fill's background is white, so along the soft edge it
   would show as a light halo. Every pixel whose alpha is below 250 takes
   colour bled outward from the opaque pixels (alpha ≥ 250) by normalized
   convolution, per channel `G_σ(fill · w) / G_σ(w)` with `w` = 1 on opaque
   pixels and σ = 1.5 target pixels; where `G_σ(w)` is below 10⁻³, 3σ is used
   instead, and a pixel no opaque pixel reaches keeps its colour. The
   Gaussian is truncated at 4σ with mirrored borders. Opaque pixels are
   unchanged. It does not depend on Strength and is cached with the layers.
5. **Unsharp mask.** The line art is sharpened like GIMP's Unsharp Mask with
   radius 1 and threshold 0:
   `clamp(x + amount · (x − blur(x)), 0, 255)`, rounded once, where `blur` is
   a Gaussian with σ = 1 truncated at 3σ and clamped at the image edges.
   **Strength** 0–100 (default 40) maps linearly to amount 0.5–3.0, so 40
   gives 1.5.
6. **Multiply.** The sharpened line art multiplies the defringed fill's
   encoded 8-bit sRGB channels like GIMP's Multiply mode:
   `rgb = round(fill_rgb · line / 255)`. The result is straight alpha with the
   alpha from step 3, so the line art and fill never extend the silhouette.

The shared implementation is `asset_scaler::LineArtLayers`, which holds the
target-sized line art, fill and the line art's blur, and
`asset_scaler::LineArtComposer`, which caches the alpha of the latest target
size and the defringed fill of the latest layers. Diorama keeps the layers of the latest target and the foreground per
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
fraction done and the estimated time left. The worker reports its load, its
readiness and each image's denoising step. Before the first step, the
estimate is fitted to measurements (`2.054 + 7.784·MP + 6.625·MP²` seconds per
image, MP the generation megapixels), plus the worker's start-up and load
unless it is loaded, a warm-up for a size it has not generated yet, and the
prompt encoding if needed, scaled by a calibration factor that smooths
measured ÷ predicted run times of this computer. After the first image's step
it scales the model by the measured time.
