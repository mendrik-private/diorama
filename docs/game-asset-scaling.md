# Game Asset scaling

Game Asset scaling reduces an illustrated asset by multiplying line art over a
foreground fill. It only downscales: targets must be positive and no larger
than the source. The source size returns the source unchanged.

1. **Line art.** The local FLUX.2 [klein] model draws grayscale line art of the
   untouched source (white is no ink), restored to the source dimensions. See
   [local line-art generation](../README.md#local-line-art-generation) for
   model setup and caching. The line art is used as generated.
2. **Fill.** BiRefNet removes the background. The extracted foreground is
   reduced with Lanczos3 in premultiplied linear light. Its alpha is the
   foreground's silhouette coverage at the target size times its intrinsic
   alpha, with a fixed 50% edge softness. Explicit source alpha remains
   authoritative.
3. **Bicubic.** The line art is reduced to the target size with Catmull-Rom
   bicubic resampling of its stored 8-bit values.
4. **Unsharp mask.** The reduced line art is sharpened like GIMP's Unsharp
   Mask with radius 1 and threshold 0:
   `clamp(x + amount · (x − blur(x)), 0, 255)`, where `blur` is a Gaussian with
   σ = 1 truncated at 3σ and clamped at the image edges. **Strength** 0–100
   (default 40) maps linearly to amount 0.5–3.0, so 40 gives 1.5.
5. **Multiply.** The sharpened line art multiplies the fill's encoded 8-bit
   sRGB channels like GIMP's Multiply mode:
   `rgb = round(fill_rgb · line / 255)`. The fill's alpha is kept, so
   transparent pixels stay transparent and the line art never extends the
   silhouette.

The shared implementation is `asset_scaler::LineArtSession`. It caches the
reduced fill and the reduced, blurred line art for the latest target size, so
a Strength change only re-sharpens and multiplies. Diorama generates the line
art and the foreground once per source; cancelled work never enters a cache.

The **Strength** spinner appears only for Game Asset scaling and is stored in
the `game-asset-strength` setting. A scale operation stores its strength, so
preview, Apply, undo/redo, and export produce the same result.

**Show line art** previews the line art as it is multiplied: the generated
line art at the source size, the bicubic and sharpened line art for the
current Strength otherwise.
