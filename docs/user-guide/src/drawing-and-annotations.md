# Draw and annotate

Annotations explain a feature without permanently changing the source until
you save or export. Each completed annotation change is undoable. Choose the
tool that created an annotation, or **Select Region** for an image object, then
click it to edit it. Selecting an object exposes its move, resize, bend, and
where supported rotation handles.

[![Fantasy character annotated with a cyan face marker, yellow wing marker, curved arrow, and curved text](assets/screenshots/annotate-character.png)](assets/screenshots/annotate-character.png)

*The face and wing use colours that contrast with the artwork. Open the image
to inspect the curved arrow and label at full size.*

## Make a visual callout

1. Press <kbd>O</kbd> for **Highlight**, choose a high-contrast colour and a
   thickness, then drag across the detail. For a red wing, choose cyan or
   yellow rather than red; its size control ranges from 1 to 128, and the
   marker is twice that value in image pixels (so 1 produces a 2-pixel mark).
2. Press <kbd>A</kbd> for **Arrow** and drag from the explanatory label toward
   the feature. Arrow stroke width is 1–128 image pixels. Select the arrow and
   drag its middle control handle to bend it; double-click that control to
   return it to a straight midpoint.
3. Press <kbd>T</kbd>, drag in the label's reading direction, type the text,
   and press <kbd>Enter</kbd>. Text is a single line of at most 256 characters.
   Its initial size is 6–512 image pixels. Drag its middle control handle to
   curve the text around a face or wing; drag either end handle to change its
   size and direction. Double-click existing text, or select it and press
   <kbd>Enter</kbd>, to edit its words.

The annotation palette is visible with these tools. It controls the current
colour and the current tool's size. Right-click the canvas to sample a drawing
colour. Use **Preferences → Drawing → Anti-aliasing** when smoother pencil
strokes and circles matter; leave it off for crisp pixel-art marks.

## Pencil shapes

Press <kbd>P</kbd> for **Pencil**. Draw normally for freehand. Hold
<kbd>Ctrl</kbd> for connected straight segments, <kbd>Shift</kbd> for a
rectangle, or <kbd>Alt</kbd> for a circle. Pencil stroke width is 1–128 image
pixels. A freehand stroke ends at the last point the pointer moved across;
lifting the pen does not add a separate segment, and clicking without moving
creates a dot. Existing pencil shapes can be selected and resized; hold
<kbd>Shift</kbd> while resizing to preserve an object's aspect ratio. Drag just
outside a rotatable object's corner to rotate it; hold <kbd>Shift</kbd> to snap
the rotation to 15-degree steps.

## Measure pixel distances

Press <kbd>M</kbd> for **Measure**, then drag mostly horizontally or vertically
to create an axis-aligned line. Diorama labels the length in `px`; measurement
lines always use a native one-pixel stroke, independent of the size control.
When parallel measurements overlap, it can also show the gap between them.
Measurements are annotation objects, so move or adjust their endpoints after
placing them.

## Move, delete, and cancel

Drag a selected object to move it. Use <kbd>Up</kbd> and <kbd>Down</kbd> to
nudge a selected annotation by one pixel, or hold <kbd>Shift</kbd> to nudge ten
pixels. Left and right remain image-navigation keys. Press <kbd>Delete</kbd> or
<kbd>Backspace</kbd> to delete a selected annotation; with a region selection,
<kbd>Delete</kbd> fills that region with the detected background instead.
<kbd>Escape</kbd> closes a text editor, cancels an active drag, or deselects an
object in that order.
