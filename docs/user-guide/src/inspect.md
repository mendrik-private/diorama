# Inspect an image

## Read a pixel without losing context

Press <kbd>L</kbd> to turn on the inspection lens and move the pointer over
the area you want to examine. The main view stays at its current zoom. The
lens can remain active while you draw or annotate.

[![Pixel inspection lens magnifying a character face](assets/screenshots/inspect-pixel-lens.png)](assets/screenshots/inspect-pixel-lens.png)

*The lens magnifies the character's face while the rest of the illustration
stays visible. Select the image to inspect the full-size capture.*

Activate an annotation tool such as **Pencil** (<kbd>P</kbd>) to show the
bottom annotation palette, then choose its eyedropper beside the colour swatch
and click a pixel to copy its value. The picker returns to the prior annotation
tool after a sample. **Preferences → Drawing → Copied color format** selects
Hex, RGB(A), OKLab, or HSL. Right-clicking while a drawing tool is active
samples its drawing colour instead of copying a value.

Choose **Info → Image Properties** to see rendered dimensions, file location,
detected format, and whether EXIF, XMP, or ICC metadata is present. These are
inspection details; they do not alter the file.

## Work with an exact region

Press <kbd>C</kbd> and drag to select a pixel-aligned rectangle. Resize it
with its handles, then use it to zoom, crop, copy, fill, or prepare a cutout.
See [Crop and select](cropping.md) for the result of each action. Press
<kbd>Escape</kbd> to clear the selection or leave the active tool.
