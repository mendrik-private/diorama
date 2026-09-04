# Diorama

Diorama is a fast, native GNOME image viewer built with Rust, GTK 4, and
libadwaita. It combines everyday image browsing with focused, non-destructive
editing and comparison tools.

> [!NOTE]
> Diorama is under active development. File-format support and editing behavior
> may change before the first stable release.

## Features

### Image formats

- Sandboxed Glycin decoding for PNG/APNG, JPEG, GIF, WebP, AVIF, HEIF/HEIC,
  TIFF, SVG/SVGZ, BMP, JPEG XL, and JPEG 2000.
- Folder browsing also recognizes QOI, ICO, OpenEXR, Netpbm, TGA, XBM, and XPM
  images when a compatible system decoder is available.
- Automatic EXIF orientation and bounded decoding to protect against images
  whose dimensions or decoded memory exceed safe limits.
- EXIF, XMP, ICC profile, format, dimensions, location, and modification-time
  awareness.

### Browsing and viewing

- Open one image, several explicitly ordered images, or continue through every
  supported image in the current folder.
- Previous/next navigation with natural filename ordering, Nautilus folder sort
  preferences, regular-file symlink support, and neighboring-image prefetching.
- Live folder monitoring that follows renames, reloads externally changed
  images, refreshes added or removed files, and finds the next image after a
  deletion.
- Open With integration, full-image clipboard copying, confirmed permanent
  deletion, fullscreen viewing, and remembered window, folder, and zoom state.
- Fit-to-window, fill, 25%–900% presets, smooth zoom, and pixel-perfect hard
  zoom that stays aligned on HiDPI and fractionally scaled displays.
- Pinch zoom, pointer-anchored Ctrl+scroll zoom, middle-button panning,
  scrollbars for enlarged images, and an interactive
  click-and-drag minimap.
- Checkerboard, automatic contrast, white, gray, and black transparency
  backgrounds.
- Animated-image playback with play/pause and manual previous/next frame
  controls.

### Inspection and comparison

- A movable 4× pixel-inspection lens for a single image, with configurable lens
  size.
- Adaptive side-by-side or stacked comparison based on image shape, with file
  location and resolution labels for both images.
- Optional synchronized pan and relative zoom between differently sized
  comparison images.
- A cross-image detail lens that reveals the corresponding location in the
  other image; Shift+scroll changes its size and Alt+scroll its magnification.
- Comparison-folder navigation that follows matching filenames and monitors the
  comparison image for external changes or renames.
- Persistent, pixel-snapped measurements with Excalifont labels and automatic
  gap markers between neighboring measurements.
- A color picker that updates the active annotation color and copies Hex, RGB(A), OKLab, or
  HSL values.
- A persistent region selector with animated high-contrast dashes, eight
  resize handles, and contextual zoom, crop, and copy actions.

### Non-destructive editing

- An operation-based document model: edits leave the source pixels untouched
  until export and support multi-step undo and redo.
- Crop through the shared region selector, with precise pixel-aligned bounds.
- Clockwise and counterclockwise rotation plus horizontal and vertical
  flipping.
- Live image scaling with exact width and height fields, aspect-ratio locking,
  pixel or percentage sliders, and an in-progress indicator for slower methods.
- Scaling previews that can show the original while held, display output pixels
  at actual size, or stay fitted to the window as the target resolution changes.
- Nearest-neighbor, bilinear, bicubic, and content-aware seam-carving scaling.

### Pencil and pixel editing

- Configurable RGBA color, 1–128 px brush size, optional anti-aliasing, and
  mouse, pen, or touch input that draws instead of scrolling in pencil mode.
- Freehand drawing with pointer-speed-adaptive smoothing and incremental stroke
  previews that remain responsive during long strokes.
- Pixel-perfect raster paths for hard-edged drawing, plus smooth anti-aliased
  paths when requested.
- Modifier shapes while drawing: Ctrl for connected lines, Shift for
  rectangles, and Alt for circles.
- Right-click color sampling directly from the image and drawing on either
  comparison panel.

### Vector annotations

- Highlight, curved-arrow, measurement, and curved-text tools share a compact
  annotation palette with a red default swatch.
- Annotations remain editable: select, move, resize, bend, rotate, recolor,
  delete, and nudge them with the keyboard.
- Annotation creation and edits participate in the document's undo/redo
  history and are composited into PNG and JPEG exports.
- The bundled Excalifont keeps text and measurement labels consistent without
  relying on fonts installed on the system.

### Saving and GNOME integration

- PNG export with compression control, optional metadata and ICC preservation,
  and optional conversion to sRGB.
- JPEG export with quality control, metadata preservation, and selectable white,
  gray, or black compositing for transparent pixels.
- Atomic writes that do not replace the destination after a failed or cancelled
  export, plus cancellable background rendering and export progress.
- External-change protection before overwriting, unsaved-edit confirmation when
  opening or closing, and dirty-state tracking against the last successful save.
- Native GTK 4 and libadwaita controls, adaptive layouts, accessible canvas
  labels, persistent preferences, menu accelerators, and a built-in keyboard
  shortcuts reference.

## Install a release

Download `Diorama.flatpak` from the repository's latest GitHub Release, then run:

```sh
flatpak install --user ./Diorama.flatpak
flatpak run io.github.mendrik.Diorama
```

The bundle uses the GNOME 50 runtime. Flatpak will offer to install the runtime
from Flathub if it is not already available.

## Build from source

Run the regular test suite with `cargo test`. Graphical GTK tests must each run in
their own process because GTK initialization is bound to one thread:

```sh
build-aux/run-graphical-tests.sh
```

The Flatpak build is the recommended development environment because it provides
the expected GNOME SDK and Rust toolchain:

```sh
flatpak remote-add --user --if-not-exists flathub \
  https://flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --install --force-clean \
  build build-aux/io.github.mendrik.Diorama.Devel.json
flatpak run io.github.mendrik.Diorama
```

For a native build, install Rust 1.92 or newer, Meson 1.3 or newer, Ninja,
GTK 4.20 or newer, libadwaita 1.9 or newer, and their development headers. Then:

```sh
meson setup build
meson compile -C build
./build/diorama
```

Run the checks with:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Contributing and GitFlow

Contributions are welcome. By participating, you agree to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

This repository uses GitFlow:

- `main` contains released code and is tagged with `vX.Y.Z` versions.
- `develop` is the integration branch for the next release.
- `feature/*` branches start from and merge back into `develop`.
- `release/*` branches start from `develop`, accept release-only fixes, and merge
  into both `main` and `develop`.
- `hotfix/*` branches start from `main` and merge into both `main` and `develop`.

Open pull requests against `develop` for features and ordinary fixes. Keep
commits focused, add tests for changed behavior, and ensure the automated checks
pass before merging.

### Publishing a release

1. Create `release/X.Y.Z` from `develop`.
2. Update the version in `Cargo.toml`, `meson.build`, and
   `data/io.github.mendrik.Diorama.metainfo.xml.in`; add the release date to the
   AppStream entry.
3. Merge the release branch into `main`, then merge it back into `develop`.
4. Tag the release commit on `main` and push the tag:

   ```sh
   git tag -s vX.Y.Z -m "Diorama X.Y.Z"
   git push origin vX.Y.Z
   ```

The release workflow verifies that the tag matches all three version fields,
builds and tests the Flatpak, and publishes `Diorama.flatpak` plus its SHA-256
checksum to a GitHub Release. Configure `main` and `develop` as protected branches
in GitHub so changes must pass the `Build and test` check from the Flatpak
workflow.

## License

Diorama is available under the [GNU General Public License v3.0](LICENSE).
