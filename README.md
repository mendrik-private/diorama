<p align="center">
  <img src="data/icons/hicolor/scalable/apps/io.github.mendrik.Diorama.svg" width="128" height="128" alt="Diorama application icon">
</p>

<h1 align="center">Diorama</h1>

<p align="center">
  <strong>See every pixel. Compare every detail. Make the edit.</strong>
</p>

<p align="center">
  A fast, native image viewer and focused editing workspace for GNOME.
</p>

Diorama brings browsing, inspection, comparison, annotation, and practical
image editing into one uncluttered desktop app. Move through a folder without
breaking your flow, inspect pixels and metadata, compare related images, mark
up exactly what matters, and export the result—all without opening a full
graphics suite.

Built with Rust, GTK 4, and libadwaita, Diorama feels at home on GNOME and keeps
image processing local. Edits are stored as reversible operations during the
session, so the source pixels remain untouched until you choose to save or
export.

> [!NOTE]
> Diorama is under active development. File-format support and editing behavior
> may change before the first stable release.

## Made for visual work

- **Browse quickly.** Open one image, a hand-picked sequence, or an entire
  folder, then move with the arrow keys while Diorama prepares neighboring
  images.
- **Inspect precisely.** Use true 100% zoom, a pixel lens, selectable
  transparency backgrounds, metadata details, and copyable color values.
- **Compare confidently.** Place two images side by side or one above the other,
  synchronize their views, and inspect corresponding pixels through a
  cross-image lens.
- **Edit without clutter.** Crop, scale, rotate, flip, reduce colors, draw, and
  annotate through compact tools that appear only when needed.
- **Export safely.** Produce PNG or JPEG files with explicit quality, metadata,
  color-profile, and transparency choices.

## Feature guide

### Browse images as a sequence

- Open a single image, multiple explicitly ordered files, or every supported
  image in the current folder.
- Move to the previous or next image with the left and right arrow keys,
  Page Up/Page Down, header buttons, or the mouse.
- Follow Nautilus folder sorting when available, with natural filename ordering
  as the fallback, so `image2` appears before `image10`.
- Navigate regular-file symlinks and remember the last folder, window size,
  window state, and zoom mode.
- Prefetch neighboring images for faster navigation.
- Monitor the active folder for additions, removals, renames, and external file
  changes. If the current image disappears, Diorama continues with the next
  available image.
- Open the current image in another application, copy the full image, enter
  fullscreen, or permanently delete a file after confirmation.

### View at the right scale

- **Fit** scales an image up or down to use as much of the viewport as possible
  while preserving its aspect ratio.
- **Fill** covers the viewport, while **100%** maps one source pixel to one
  physical display pixel—even with HiDPI or fractional display scaling.
- Choose 25%, 50%, 75%, or 100%–900% presets, or zoom continuously from 1% to
  6400%.
- Switch between smooth interpolation and hard, pixel-perfect rendering.
- Zoom around the pointer with Ctrl+scroll, use pinch gestures, pan with the
  middle button, navigate with scrollbars, or drag the minimap on enlarged
  images.
- Display transparency over a checkerboard, automatic contrasting shade,
  white, gray, or black background.
- Play and pause animated images or step through their frames manually.

### Inspect pixels, colors, and image details

- Move a configurable 4× inspection lens over an image without changing the
  main zoom level. The lens can remain active while drawing or annotating.
- Pick any pixel and copy its value as Hex, RGB(A), OKLab, or HSL.
- View the rendered dimensions, file location, detected format, and the
  presence of EXIF, XMP, and ICC metadata.
- Select an exact, pixel-aligned rectangle with eight resize handles, then zoom
  to it, crop to it, or copy it to the clipboard.

### Compare two images

- Open a second image in an adaptive split view: landscape images are placed
  side by side and portrait images are stacked to make better use of the window.
- See each image's location and resolution directly in the comparison view.
- Synchronize pan and relative zoom even when the two images have different
  dimensions or aspect ratios.
- Reveal the matching area of the other image through a cross-image detail
  lens. Shift+scroll changes the lens size; Alt+scroll changes magnification.
- Navigate comparison folders by matching normalized filenames, even when the
  extensions or separators differ.
- Track external changes and renames of the comparison image.
- Draw with the Pencil on either comparison panel and right-click to sample a
  color without leaving stray marks.

### Make reversible image edits

Diorama keeps an immutable source image plus an undoable operation history.
Processing runs away from the interface and can be cancelled, keeping the
window responsive while larger edits render.

- Undo and redo edits throughout the session.
- Rotate 90° clockwise or counterclockwise and flip horizontally or vertically.
- Crop to a selected region, or detect likely content bounds from transparency
  or a uniform opaque background and confirm the suggested crop.
- Scale by exact width or height, pixels or percent, with an optional locked
  aspect ratio.
- Preview scaling fitted to the window or at actual output-pixel size, and hold
  a control to compare against the original.
- Choose nearest-neighbor, linear, bicubic, or content-aware seam-carving
  scaling.
- Reduce an image to 2–256 colors, optionally apply dithering, and preserve
  isolated accent colors.

### Draw and annotate

The shared annotation palette keeps color, stroke size, and text size close to
the canvas. Annotation edits belong to the same undo/redo history as image
operations and are composited into the exported image.

- Draw freehand with a mouse, pen, or touch input using a configurable RGBA
  color and 1–128 image-pixel stroke width.
- Choose crisp pixel-perfect raster paths or smooth anti-aliased edges.
- Use pointer-speed-adaptive smoothing and incremental previews for responsive
  long strokes.
- Click once to place a dot directly on the canvas without resize controls.
- Hold Ctrl for connected straight lines, Shift for rectangles, or Alt for
  circles.
- Right-click to sample a drawing color directly from the image.
- Add translucent highlights, curved arrows, persistent pixel measurements,
  and curved text.
- Select, move, resize, bend, rotate, recolor, delete, and keyboard-nudge
  annotations after placing them.
- Keep labels visually consistent with the bundled Excalifont, independent of
  fonts installed on the system.

### Export with control

- Export PNG with adjustable compression, alpha transparency, and optional
  metadata and ICC-profile preservation.
- Export JPEG with adjustable quality and a white, gray, or black background
  for transparent pixels.
- Optionally convert the color profile to sRGB.
- Preserve compatible EXIF, XMP, and ICC data, or remove it during export.
- Write atomically so a failed or cancelled export does not replace the
  destination.
- Cancel background rendering or export, and see progress for longer jobs.
- Receive a warning before overwriting a source that changed outside Diorama,
  and confirmation before discarding unsaved edits.

### Native GNOME experience

- Adaptive GTK 4 and libadwaita interface with native dialogs, menus, toasts,
  actions, and persistent preferences.
- Accessible canvas names, tool announcements, high-contrast overlays, and
  keyboard-driven region and annotation placement.
- A built-in keyboard shortcuts reference.
- Sandboxed image decoding through Glycin with limits on dimensions and decoded
  memory for safer handling of untrusted or exceptionally large files.

## Supported image formats

Diorama directly targets sandboxed Glycin decoding for:

`PNG` · `APNG` · `JPEG` · `GIF` · `WebP` · `AVIF` · `HEIF/HEIC` · `TIFF` ·
`SVG/SVGZ` · `BMP` · `JPEG XL` · `JPEG 2000`

Folder navigation also recognizes the following formats when a compatible
system decoder is available:

`QOI` · `ICO` · `OpenEXR` · `PBM/PGM/PPM/PNM` · `TGA` · `XBM` · `XPM`

Images are normalized for EXIF orientation while loading. Availability of some
modern formats depends on the codecs supplied by the installed GNOME runtime.

## Keyboard at a glance

| Task | Shortcut |
| --- | --- |
| Previous / next image | Left / Right |
| Open | Ctrl+O |
| Copy image or selection | Ctrl+C |
| Save / Save As | Ctrl+S / Ctrl+Shift+S |
| Undo / redo | Ctrl+Z / Ctrl+Shift+Z |
| Fit / actual size | 0 / 1 |
| Zoom 200%–900% | 2–9 |
| Zoom in / out | + / − |
| Toggle hard or soft zoom | X |
| Select region | C |
| Pencil / Highlight / Arrow | P / O / A |
| Measure / Text / Scale | M / T / S |
| Compare / Lens | D / L |
| Rotate clockwise / counterclockwise | R / Shift+R |
| Flip horizontally / vertically | H / V |
| Apply the active keyboard tool | Enter or Space, depending on the tool |
| Clear a selection or leave a tool | Escape |
| Delete the current image | Delete |
| Fullscreen | F11 |

Open **Keyboard Shortcuts** from Diorama's main menu for the complete in-app
reference.

## Install Diorama

Download `Diorama.flatpak` from the
[latest GitHub release](https://github.com/mendrik-private/diorama/releases/latest),
then run:

```sh
flatpak install --user ./Diorama.flatpak
flatpak run io.github.mendrik.Diorama
```

The bundle uses the GNOME 50 runtime. Flatpak will offer to install the runtime
from Flathub if it is not already available.

## Build from source

The Flatpak build is the recommended development environment because it
provides the expected GNOME SDK and Rust toolchain:

```sh
flatpak remote-add --user --if-not-exists flathub \
  https://flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --install --force-clean \
  build build-aux/io.github.mendrik.Diorama.Devel.json
flatpak run io.github.mendrik.Diorama
```

For a native build, install Rust 1.92 or newer, Meson 1.3 or newer, Ninja,
GTK 4.20 or newer, libadwaita 1.9 or newer, and their development headers:

```sh
meson setup build
meson compile -C build
./build/diorama
```

Run the standard checks with:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Graphical GTK tests must each run in their own process because GTK
initialization is bound to one thread:

```sh
build-aux/run-graphical-tests.sh
```

## Contributing

Contributions are welcome. Please read the [Code of Conduct](CODE_OF_CONDUCT.md),
open ordinary changes against `develop`, keep commits focused, and include tests
for behavior changes. The `main` branch contains released code and `vX.Y.Z`
tags; feature branches start from and return to `develop`.

## License

Diorama is available under the [GNU General Public License v3.0](LICENSE).
