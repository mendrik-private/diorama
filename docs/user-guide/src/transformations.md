# Transform an image

Open **Transform** in the main menu for whole-image geometry changes. These
commands act on the rendered image, including its current annotations where
the operation requires flattening, and are undoable before saving.

## Rotate and flip

Choose **Rotate Clockwise** (<kbd>R</kbd>) or **Rotate Counterclockwise**
(<kbd>Shift</kbd>+<kbd>R</kbd>) for a 90-degree turn. Choose **Flip
Horizontally** (<kbd>H</kbd>) or **Flip Vertically** (<kbd>V</kbd>) to mirror
the whole image. Use these for the canvas itself. A cutout has on-canvas move,
resize, and rotate handles, but not an independent flip command.

[![A fantasy character after clockwise rotation and horizontal flip](assets/screenshots/flip-and-rotate.png)](assets/screenshots/flip-and-rotate.png)

*The capture shows the combined whole-image result after a clockwise rotation
and horizontal flip.*

## Square Up and Canvas Resize

**Square Up** increases the shorter canvas dimension to match the longer one,
keeps the image centered, and fills added pixels with Diorama's detected image
background. It does nothing when the image is already square and refuses a
square that would exceed the image memory limit.

**Canvas Resize…** is more general. Enter the output **Width** and **Height**
in pixels, inspect the preview, and choose **Resize**. The existing image stays
centered at its original pixel size; it grows the canvas by adding space.
Added space uses the detected background. Its dimensions cannot be smaller than
the current image; use [Crop and select](cropping.md) when the canvas must be
smaller. The dialog disables **Resize** for the original dimensions or an image
above the memory limit. This differs from
[scaling](scaling.md): scaling changes pixel dimensions and resamples content;
canvas resize leaves the content's pixel size alone.

[![Canvas Resize dialog previewing added space around a fantasy character](assets/screenshots/canvas-resize.png)](assets/screenshots/canvas-resize.png)

*Canvas Resize keeps the image centered and previews only the added canvas.*

## Warp Mesh

Choose **Transform → Warp Mesh** or press <kbd>W</kbd>. Click to add control
nodes while the mesh is unwarped. Select one of the optional **square**,
**2:1 dimetric**, or **isometric** grid buttons to display a guide; the grid is
only a guide. Click **Warp Mesh**, then drag a target node to deform the image.
The preview updates while it is valid. **Clear Nodes** resets the nodes; use
undo/redo while the session is open to step through node changes. Click
**Apply mesh** to commit the preview, or press <kbd>Escape</kbd> to leave it
without applying.

The image corners are fixed by the mesh; add one or more interior control
nodes. Moves that would fold or collapse the mesh are rejected, and a failed
preview restores the original display.
Mesh application rasterizes the current result, so treat it as a finishing
operation. PNG/JPEG output cannot later restore individual annotation objects;
apply the mesh last, or undo it while the current session is still open.

[![Warp Mesh controls with an isometric grid and editable nodes over a character](assets/screenshots/mesh-warp-grid.png)](assets/screenshots/mesh-warp-grid.png)

*The grid aligns visual work; node movement defines the actual deformation.*
