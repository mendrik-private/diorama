# Scale images

Scaling changes the pixel dimensions of the working image. It is an undoable edit: Diorama does not change the file on disk until you [save or export](save-and-export.md). Use it for a required delivery size, a sprite asset, a thumbnail, or a larger working copy. Enlarging cannot recover detail absent from the original, so it may make an image look softer or less faithful.

## Scale an image to an exact size

1. Open the image and press <kbd>S</kbd>, or choose **Scale**.
2. Leave **Aspect** on to preserve the source proportions. Enter a value in **W** or **H**; Diorama calculates the other dimension, rounding to a whole pixel. Turn **Aspect** off only when you deliberately need to stretch or squash the image, then enter both values.
3. Select a slider unit. **Pixels** controls output width; **Percent** scales both source dimensions by the selected percentage. The W and H fields always show the exact output dimensions in pixels.
4. Choose a method from the table below and wait for its preview.
5. Inspect at **Preview → Actual Pixels**, or choose **Fit Pixels** to fit it in the window. Hold **Hold Original** to temporarily see the unscaled source at the same on-screen footprint.
6. Select **Apply Scale** or press <kbd>Enter</kbd>. The exact width, height, and method become one undoable edit. Press <kbd>Ctrl</kbd>+<kbd>Z</kbd> to undo it.

[![Scale controls showing dimensions, units, resampling method, comparison, and preview choices](assets/screenshots/scale-options.png)](assets/screenshots/scale-options.png)

*The Scale bar keeps requested dimensions, method, preview comparison, and the apply action together. Select the image to view it at full size.*

[![Nearest and Lanczos scaling previews of the same fantasy character at 200 percent](assets/screenshots/scaling-method-comparison.png)](assets/screenshots/scaling-method-comparison.png)

*This capture compares Nearest and Lanczos at 200% view zoom on the same asset. Use it to see the hard pixel blocks of Nearest against Lanczos's blended detail; Bicubic remains the separate general-purpose option in the table below.*

### Example: make a 1024-pixel image 256 pixels wide

For a square 1024 × 1024 character image, leave **Aspect** on, type `256` in **W**, choose **Lanczos**, and check the preview at **Actual Pixels**. The height changes to 256 and the label reports `1024 × 1024 → 256 × 256 (25%)`. Apply only after checking the silhouette and face details. A 1024 × 768 source at 256 pixels wide becomes 256 × 192. With **Aspect** off, you can set any positive width and height, which changes the image proportions.

## Choose a scaling method

| Method | What it does | Good use | Review before applying |
| --- | --- | --- | --- |
| **Nearest** | Copies the closest source pixel without blending. | Pixel art, tiles, masks, deliberately blocky sprites, and hard colour boundaries. | Jagged steps are more visible on painted or photographic images. |
| **Bicubic** | Uses Catmull–Rom bicubic interpolation. | General-purpose illustration and ordinary resize work. | It blends neighbouring pixels, so it does not preserve pixel-art blocks. |
| **Lanczos** | Uses the Lanczos3 filter. | Reducing detailed painted artwork, photos, and high-resolution illustrations with a conventional crisp resample. | Inspect fine outlines and high-contrast edges at actual size. |
| **Game Asset** | Generates line art and a colour fill, then builds a new smaller asset. | Illustrated game characters that need cleaner readable lines at a small size. | It is generative, needs local model setup, only shrinks, and can alter small details. |

Nearest, Bicubic, and Lanczos accept output dimensions from 1 pixel up to twice the source width and height. Diorama warns when you apply a larger size because enlarging may reduce perceived quality. Game Asset accepts only positive dimensions no larger than the source in either direction.

## Understand the controls

**W × H** is the exact output canvas size. With **Aspect** enabled, editing one field recalculates the other from the original image ratio. The slider follows the selected unit: in **Pixels**, it controls width; in **Percent**, it ranges from 1% to 200% for Nearest, Bicubic, and Lanczos, or 1% to 100% for Game Asset. The percent slider scales both dimensions together even when **Aspect** is off; use W and H for a non-proportional result.

**Preview** changes only how you inspect the proposed image. **Actual Pixels** maps one output pixel to one display pixel. **Fit Pixels** refits every newly generated preview to the available canvas. The initial preview keeps the source image's on-screen footprint, which makes a smaller result easy to judge in context. Neither choice changes output dimensions.

**Hold Original** is a press-and-hold comparison, not an undo action. Keep it pressed to see the source and release it to return to the current preview. It becomes available once a preview is ready. **Apply Scale** commits the exact dimensions and method shown. Closing the Scale tool without applying restores the source view. Changing dimensions or method replaces the pending preview; only the latest successful preview is displayed.

## Preview failures and responsive editing

Previews run away from the interface, so you can change a size while one is being calculated. Changing a value cancels the older request. If a preview cannot be made, Diorama shows the error as a notification and leaves the working image unchanged. A normal scale preview can also fall back from GPU scaling to the built-in scaler.

Game Asset previews take longer and show a spinner plus a progress bar. The bar identifies generation and cutout stages and gives an estimate that improves as work proceeds. Its special requirements, caching, cancellation behaviour, and visual trade-offs are described in [Game Asset scaling](game-asset-scaling.md).
