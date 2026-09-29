# Crop and select

Press <kbd>C</kbd>, or choose **Tools → Select Region**, then drag across the
pixels to keep. Drag an edge or corner handle to correct the rectangle. The
selection is pixel-aligned, so it is suitable for sprites and exact exports.

The small toolbar beside an active selection has five separate actions:

1. **Zoom to Selected Region** changes only the view; it does not edit the
   image.
2. **Crop to Selected Region** removes everything outside the rectangle.
3. **Copy Selected Region** places only that rectangular raster on the system
   clipboard.
4. **Remove Background from Selected Region** prepares a cutout that can be
   moved; see [background removal](background-removal.md).
5. **Fill Selected Region with Background** replaces the entire rectangle with
   the image's detected background colour. If no consistent border background
   is detected, its fallback is transparent black. It does not try to separate
   a subject from the rectangle.

[![A pixel-aligned character selection, with the region toolbar ready for crop, copy, cutout, or fill](assets/screenshots/edit-selection.png)](assets/screenshots/edit-selection.png)

*Choose the crop action when the selected rectangle is exactly the output you
want. Open the image for a full-size view of the controls.*

## Crop to detected content

Choose **Edit → Crop to Content** when a subject has uniform surrounding space.
Diorama calculates bounds, shows their `x`, `y`, width, and height, and waits
for **Apply**. On transparent images it follows visible alpha; on opaque images
it tries to identify a border background. It can decline with “The background
could not be identified with enough confidence.” Cancel or adjust a manual
selection in that case.

[![Crop to Content confirmation showing detected bounds around a fantasy character](assets/screenshots/crop-detected-content.png)](assets/screenshots/crop-detected-content.png)

*Review the detected coordinates before applying. The operation waits for an
explicit confirmation rather than cropping immediately.*

## What changes and how to recover

Cropping, detected-content cropping, and background fill are document edits.
They can be undone with <kbd>Ctrl</kbd>+<kbd>Z</kbd> and redone with
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> until the document is closed.
Press <kbd>Escape</kbd> to clear a selection without changing the image.
Nothing reaches the source file until you save or export.
