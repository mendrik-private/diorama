# Save and export

Choose **File → Save** with <kbd>Ctrl</kbd>+<kbd>S</kbd> to overwrite the current
PNG or JPEG using the stored export options. If the current file is not a
supported PNG or JPEG, Diorama opens the save dialog instead. **File → Save
As…** with
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> always opens that dialog: choose a
filename ending in `.png`, `.jpg`, or `.jpeg`, then choose options and press
**Export**. The filename extension selects the output format. Diorama confirms
before discarding unsaved work and warns when a source file changed outside the
application.

Use the export controls when the output needs a particular format or image
policy:

- PNG offers compression from 0 through 9 (default 6), alpha transparency, and
  optional metadata and ICC profile preservation. Compression trades file size
  against export time; it does not change the rendered pixels. Its **Convert
  color profile to sRGB** control writes sRGB rather than preserving an ICC
  profile.
- JPEG offers quality from 1 through 100 (default 92) and a white, gray, or
  black background for transparent
  pixels. JPEG cannot retain transparency: transparent pixels are composited
  onto the selected background.
- **Preserve compatible metadata and color profile** applies to both formats:
  it keeps available EXIF, XMP, and ICC data. Turn it off to remove compatible
  metadata; PNG alone can instead convert its colour profile to sRGB.

[![Diorama export options shown for annotated fantasy-character artwork](assets/screenshots/export-options.png)](assets/screenshots/export-options.png)

*Export options make the format, transparency background, and metadata choices
explicit. Select the image to view it at full size.*

Exports write atomically, so a failed or cancelled export does not replace the
destination. Long renders and exports show progress and can be cancelled. A
completed export marks only the operations it contained as saved; if you make a
new edit while an export is running, Diorama keeps that newer edit unsaved.

Saving writes the current combined image. To preserve a cutout separately,
select the cutout object, use **Copy Image or Selection**, then paste it into a
new image-capable application or document before exporting. A selected cutout
copies its own transformed raster, including transparent pixels.
