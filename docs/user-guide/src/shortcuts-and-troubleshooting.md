# Shortcuts and troubleshooting

| Task | Shortcut |
| --- | --- |
| Open | <kbd>Ctrl</kbd>+<kbd>O</kbd> |
| Save / Save As | <kbd>Ctrl</kbd>+<kbd>S</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> |
| Undo / redo | <kbd>Ctrl</kbd>+<kbd>Z</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> |
| Fit / actual size | <kbd>0</kbd> / <kbd>1</kbd> |
| Toggle hard zoom | <kbd>X</kbd> |
| Previous / next image | <kbd>Alt</kbd>+<kbd>Left</kbd>/<kbd>Right</kbd> or <kbd>Page Up</kbd>/<kbd>Page Down</kbd> |
| Select / pencil | <kbd>C</kbd> / <kbd>P</kbd> |
| Highlight / arrow / text / measure | <kbd>O</kbd> / <kbd>A</kbd> / <kbd>T</kbd> / <kbd>M</kbd> |
| Compare / lens | <kbd>D</kbd> / <kbd>L</kbd> |
| Rotate / flip | <kbd>R</kbd>, <kbd>Shift</kbd>+<kbd>R</kbd> / <kbd>H</kbd>, <kbd>V</kbd> |
| Scale / Warp Mesh | <kbd>S</kbd> / <kbd>W</kbd> |
| Leave a tool or selection | <kbd>Escape</kbd> |
| Fullscreen image preview | <kbd>Space</kbd> |

Open **Keyboard Shortcuts** from Diorama's main menu for the complete reference.

## Common questions

### The image is too small or too large

Press <kbd>0</kbd> to fit it to the available viewport, or <kbd>1</kbd> to map
one source pixel to one display pixel. Ctrl+scroll adjusts zoom around the
pointer.

### I cannot edit the comparison image

This is expected: comparison keeps the second image read-only. Make edits on
the primary image, then save or export it.

### I need to keep transparency or metadata

Export PNG to keep alpha. In export options, choose whether compatible EXIF,
XMP, and ICC metadata is retained, and whether a PNG profile is converted to
sRGB.

### A cutout will not activate

Keep the selection around one foreground subject with some background around
it, then wait for preparation to finish. Background removal requires the
configured BiRefNet model and cannot continue if segmentation fails. A warning
about content-aware fill is different: Diorama can still create the cutout,
but moving it fills the old location with the detected background colour.

### What will Delete remove?

With a region selection, <kbd>Delete</kbd> fills that rectangle with the
detected background. With a selected annotation or cutout, <kbd>Delete</kbd> or
<kbd>Backspace</kbd> removes that object. With neither selected, <kbd>Delete</kbd>
opens the permanent file-deletion confirmation. Undo document edits before
saving; file deletion is not an undoable image edit.

### I cannot find Reduce Palette

The source contains a **Reduce Palette** dialog action, but the current main
menu and visible toolbars do not expose it. It is therefore not a supported
reachable workflow in this release; do not rely on documentation written for a
future palette-control entry.
