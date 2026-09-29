# Save and export

Choose **Save** with <kbd>Ctrl</kbd>+<kbd>S</kbd> to overwrite the current PNG
or JPEG using the stored export options. If the current file is not a supported
PNG or JPEG, Diorama opens the save dialog instead. **Save As** with
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> always opens that dialog: choose a
filename ending in `.png`, `.jpg`, or `.jpeg`, then choose options and press
**Export**. The filename extension selects the output format. Diorama confirms
before discarding unsaved work and warns when a source file changed outside the
application.

Use the export controls when the output needs a particular format or image
policy:

- PNG offers compression, alpha transparency, and optional metadata and ICC
  profile preservation.
- JPEG offers quality and a white, gray, or black background for transparent
  pixels.
- You can preserve compatible EXIF, XMP, and ICC data, remove it, or convert
  a PNG's colour profile to sRGB.

[![Diorama export options shown for annotated fantasy-character artwork](assets/screenshots/export-options.png)](assets/screenshots/export-options.png)

*Export options make the format, transparency background, and metadata choices
explicit. Select the image to view it at full size.*

Exports write atomically, so a failed or cancelled export does not replace the
destination. Long renders and exports show progress and can be cancelled.
