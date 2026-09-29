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
   two images at that size from one model load: grayscale line art (white is
   no ink) and an opaque fill without ink contours. See [local line-art
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
4. **Unsharp mask.** The line art is sharpened like GIMP's Unsharp Mask with
   radius 1 and threshold 0:
   `clamp(x + amount · (x − blur(x)), 0, 255)`, rounded once, where `blur` is
   a Gaussian with σ = 1 truncated at 3σ and clamped at the image edges.
   **Strength** 0–100 (default 40) maps linearly to amount 0.5–3.0, so 40
   gives 1.5.
5. **Multiply.** The sharpened line art multiplies the fill's encoded 8-bit
   sRGB channels like GIMP's Multiply mode:
   `rgb = round(fill_rgb · line / 255)`. The result is straight alpha with the
   alpha from step 3, so the line art and fill never extend the silhouette.

The shared implementation is `asset_scaler::LineArtLayers`, which holds the
target-sized line art, fill and the line art's blur, and
`asset_scaler::LineArtComposer`, which caches the alpha of the latest target
size. Diorama keeps the layers of the latest target and the foreground per
source, so a Strength change only re-sharpens and multiplies; another size
generates again or reads the disk cache. Cancelled work never enters a cache.

The **Strength** spinner appears only for Game Asset scaling and is stored in
the `game-asset-strength` setting. A scale operation stores its strength, so
preview, Apply, undo/redo, and export produce the same result.

**Show line art** previews the line art as it is multiplied: at the target
size and sharpened at the current Strength.

While a preview generates, a progress bar next to the spinner shows the
fraction done and the estimated time left. Prompts without cached embeddings
are first encoded on the CPU by a worker process of its own; the generation
worker then reports its stages and each denoising step. Before the first step,
the estimate is fitted to measurements (`68.182·MP + 14.031·MP²` seconds per
image, MP the generation megapixels) plus start-up, model load and the prompt
encoding if needed, scaled by a calibration factor that smooths measured ÷
predicted run times of this computer. From the first step on, it extrapolates
the measured step time.
