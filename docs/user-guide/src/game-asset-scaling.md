# Game Asset scaling

**Game Asset** is Diorama's specialised downscaler for illustrated characters and other game art. A local FLUX.2 [klein] model creates a clean line-art layer and a colour fill based on the image; Diorama then restores local colour, sharpens the lines, draws them as black ink over the fill, and uses a BiRefNet cutout for the fill's transparency. The output is designed to remain readable at a smaller size.

That makes it useful for a 1024-pixel fantasy character reduced to a 256-pixel sprite or portrait. It also means the output is a new interpretation of the art, not a mathematically exact reduction. Fine ornaments, facial details, thin straps, and the silhouette can change. Always inspect the preview before applying it; choose **Lanczos** when preserving source detail matters more than generating cleaner lines.

## Make a Game Asset preview

1. Complete the **BiRefNet** and **FLUX.2 [klein]** sections of [model setup](model-setup.md) once. Game Asset needs both runtimes; ordinary scale methods do not.
2. Open the artwork, press <kbd>S</kbd>, and select **Game Asset**. Diorama begins warming the worker while this method is selected.
3. Set target dimensions. Keep **Aspect** enabled for a normal character, then enter one dimension or use the percent slider. Game Asset only shrinks: both target dimensions must be no larger than the source.
4. Wait for the line-art and cutout progress bar, then inspect at **Preview → Actual Pixels**. Hold **Hold Original** to compare source and preview at the same display footprint.
5. Start at **Strength 40%**, the default. Lower it when line work feels too strong; raise it when outlines are disappearing. Toggle **Show line art** to inspect the sharpened line layer by itself.
6. Choose **Apply Scale** only when the character's outline, face, wings, and transparent edges look right. The selected strength is kept with the scale edit, so undo/redo and later export reproduce that result.

[![Game Asset scale controls on a fantasy character, with a 512-pixel target and Strength control](assets/screenshots/game-asset-options.png)](assets/screenshots/game-asset-options.png)

*Game Asset has the ordinary size and preview controls plus Strength and line-art inspection. Select the image to view it at full size.*

[![Completed 512-pixel Game Asset preview of a fantasy character](assets/screenshots/game-asset-result.png)](assets/screenshots/game-asset-result.png)

*This completed 512-pixel preview has not been applied yet. Compare it with the source and inspect the face, wings, silhouette, and transparent edges before applying.*

[![Game Asset line-art-only preview showing the character's face and wing outlines](assets/screenshots/game-asset-line-art.png)](assets/screenshots/game-asset-line-art.png)

*Show line art reveals the sharpened layer that is combined with the generated fill. Use it to spot missing or overly heavy contours before applying.*

## Compare Game Asset with Bicubic

These captures use the same unchanged 1024 × 1024 JPEG original for both sides
of each comparison. The left side is a **Bicubic** (Catmull–Rom) reduction; the
right side is **Game Asset** at the default **Strength 40%**. Each square output
is shown in Diorama at **200% hard zoom** over the same gray viewer background.
The 200% setting only doubles each output pixel on screen: it does not change
the 128 × 128, 180 × 180, or 256 × 256 output dimensions. Select any capture to
open it at full size.

[![Cave spider reduced from the same 1024-pixel JPEG to 128 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-cave-spider-128.png)](assets/screenshots/game-asset-vs-bicubic-cave-spider-128.png)

*Cave spider at 128 × 128: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

<details>
<summary>Cave spider at 180 and 256 pixels</summary>

[![Cave spider reduced from the same 1024-pixel JPEG to 180 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-cave-spider-180.png)](assets/screenshots/game-asset-vs-bicubic-cave-spider-180.png)

*Cave spider at 180 × 180: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

[![Cave spider reduced from the same 1024-pixel JPEG to 256 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-cave-spider-256.png)](assets/screenshots/game-asset-vs-bicubic-cave-spider-256.png)

*Cave spider at 256 × 256: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

</details>

<details>
<summary>Goblin at 128, 180, and 256 pixels</summary>

[![Goblin reduced from the same 1024-pixel JPEG to 128 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-goblin-128.png)](assets/screenshots/game-asset-vs-bicubic-goblin-128.png)

*Goblin at 128 × 128: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

[![Goblin reduced from the same 1024-pixel JPEG to 180 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-goblin-180.png)](assets/screenshots/game-asset-vs-bicubic-goblin-180.png)

*Goblin at 180 × 180: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

[![Goblin reduced from the same 1024-pixel JPEG to 256 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-goblin-256.png)](assets/screenshots/game-asset-vs-bicubic-goblin-256.png)

*Goblin at 256 × 256: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

</details>

<details>
<summary>Dragon at 128, 180, and 256 pixels</summary>

[![Dragon reduced from the same 1024-pixel JPEG to 128 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-dragon-128.png)](assets/screenshots/game-asset-vs-bicubic-dragon-128.png)

*Dragon at 128 × 128: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

[![Dragon reduced from the same 1024-pixel JPEG to 180 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-dragon-180.png)](assets/screenshots/game-asset-vs-bicubic-dragon-180.png)

*Dragon at 180 × 180: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

[![Dragon reduced from the same 1024-pixel JPEG to 256 pixels, with Bicubic on the left and Game Asset on the right](assets/screenshots/game-asset-vs-bicubic-dragon-256.png)](assets/screenshots/game-asset-vs-bicubic-dragon-256.png)

*Dragon at 256 × 256: Bicubic on the left and Game Asset on the right, both viewed at 200% hard zoom.*

</details>

The Bicubic side resamples the opaque JPEG, including its original background.
Game Asset instead asks FLUX to draw new line art and a new colour fill, then
uses a real BiRefNet cutout for transparency; the gray visible around that
subject is Diorama's viewer background. Cleaner small-scale shapes can come
with changes to facial features, ornaments, line placement, colours, or the
silhouette. Treat the right side as a generated interpretation rather than a
sharper version of the same pixels, and review important details before
applying it.

## What happens to a 1024 × 1024 image reduced to 256 × 256

The requested output is 256 × 256. For reliable line work, Diorama uses a 512 × 512 generation canvas rather than asking the model to draw directly at 256 pixels. It composites the source over white and resizes that reference with Lanczos3. FLUX generates a grayscale line-art image and an ink-free colour fill at 512 × 512, one after the other. BiRefNet cuts the generated fill out. The layers are centre-cropped consistently and then reduced to 256 × 256: line art with Catmull–Rom bicubic, fill and alpha with Lanczos3.

Diorama corrects the generated fill toward nearby source colours away from the ink, so a tunic, strap, or wing is less likely to borrow colour across a sharp boundary. It sharpens the line art and draws it as black ink over the fill. Generated background does not become an opaque rectangle: the fill uses the BiRefNet mask, and a source image's existing alpha caps that mask. An outer ink outline can remain visible outside the fill cutout, which protects a drawn contour but means you should inspect transparent edges before applying.

**Strength** controls final line-art sharpening from 0% to 100%; 40% is the default. Even 0% retains a light unsharp mask, while 100% is strongest. Changing only Strength reuses generated layers when available, normally avoiding another model run.

## Generation size and limits

The requested output is always the final size. Diorama chooses an intermediate generation canvas this way:

- It raises the target until the shorter side is 512 pixels, never reduces a target for generation, and rounds each generation side up to a multiple of 16.
- A target with both sides at least 512 pixels is generated at its own size, then centre-cropped if rounding requires it; it is not resampled after generation, provided its rounded generation size fits the area limit.
- The rounded generation canvas may not exceed one megapixel (1,048,576 pixels). If raising a very narrow target to a 512-pixel short side would exceed that area, Diorama uses a smaller intermediate scale instead. A requested target whose own dimensions round beyond that limit is rejected; for example, 1025 × 1024 rounds to 1040 × 1024 and cannot run. The individual long side can be greater than 1024 when the total rounded area fits.
- The generation canvas supports an aspect ratio up to 8:1. Targets outside that supported shape are rejected.
- A target equal to the source returns the source unchanged. Game Asset is for reduction, not enlargement.

For example, a 256 × 128 target generates around 1024 × 512 before reduction, while a 1024 × 768 target generates at its target size. Do not rely on the larger intermediate image to preserve a particular tiny feature: FLUX still generates new line art and fill.

## Waiting, cancellation, and caching

The first Game Asset preview can take time. Before generation, Diorama encodes two fixed prompts and caches their embeddings next to the model. This needs about 13 GB of system RAM on the documented configuration. The resident worker then keeps the transformer and VAE on the GPU and handles one job at a time. A 512-square generation was measured at about 7.3 GB GPU memory, with more needed at larger sizes.

The worker is warmed when the Scale tool opens with Game Asset selected, or when you choose the method. It unloads when you leave Scale or change to another method, after ten idle minutes, and when Diorama exits. Changing a size cancels the current generation at its next step. If a worker does not stop within 20 seconds, crashes, or runs out of memory, Diorama reports the error and replaces it for a later request. Close other GPU-heavy applications and retry after an out-of-memory failure. CPU generation is refused unless `DIORAMA_LINE_ART_DEVICE=cpu` is set; it is expected to be very slow.

Successful generated pairs and cutouts are cached under `$XDG_CACHE_HOME/diorama/line-art`. Repeating an identical request can skip inference, and small outputs with the same aspect ratio can share one generated pair. Diorama keeps the 64 most recently used pairs. Failed, rejected, and cancelled generations are never cached. A generation whose line art is solid or does not align with reference colour edges is rejected; sparse line art is allowed, so a flat image may use the fill with little or no visible ink.

## When not to use it

- You are enlarging an image: Game Asset refuses it.
- You need exact pixel values, a mask, or grid-aligned sprites: use **Nearest**.
- You need a normal resize of painted or photographic art: use **Lanczos** or **Bicubic**.
- The requested target is outside the supported 8:1 generation shape, rounds beyond one megapixel (for example, 1025 × 1024), or the model runtime is unavailable.
- A preview changes a face, wing, weapon, transparent edge, or other important detail: leave it un-applied and choose another method.
