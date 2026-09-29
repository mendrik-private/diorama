# Preferences

Open **Info → Preferences** or press <kbd>Ctrl</kbd>+<kbd>,</kbd>. Changes take
effect immediately and are remembered for future sessions.

[![Preferences dialog showing viewing, drawing, and editing choices](assets/screenshots/preferences.png)](assets/screenshots/preferences.png)

*Display settings affect the workspace immediately; they do not rewrite image
pixels until an edit is saved or exported.*

## Viewing

- **Hard zoom** uses nearest-neighbor display rendering, keeping pixel edges
  sharp. Turn it off for soft interpolated viewing. <kbd>X</kbd> toggles the
  same setting while viewing.
- **Transparency background** offers Checkerboard, Auto, White, Gray, and
  Black. It changes only the viewer and comparison backdrop; it does not add,
  remove, or export pixels.
- **Lens size** sets the pixel-inspection lens to Small, Medium, or Large.

## Drawing and editing

- **Anti-aliasing** smooths pencil strokes and circles.
- **Copied color format** chooses Hex, RGB(A), OKLab, or HSL for the Color
  Picker tool's clipboard value.
- **Mesh grid size**, **Mesh grid X offset**, and **Mesh grid Y offset** set
  the spacing and origin of Warp Mesh guides in image pixels. They do not move
  image pixels or nodes.
- **Pasted image scaling** selects Bicubic or Lanczos for resizing pasted image
  objects. It applies to pasted objects, not the whole-image Scale workflow.

The whole-image scaling method is selected inside the [Scale](scaling.md)
panel, so it can be chosen per operation.
