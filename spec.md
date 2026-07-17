Below is a development-ready specification. It interprets “soft/hard zoom” as smooth versus nearest-neighbor rendering and treats Nautilus ordering as a best-effort integration.

# Application Specification: High-Performance GNOME Image Viewer

**Working title:** Image Viewer
**Platform:** Linux desktop, optimized for GNOME and Wayland
**Implementation:** Rust, GTK 4, libadwaita
**Document status:** Initial product and engineering specification

## 1. Product Summary

The application is a fast, native GNOME image viewer with focused image-editing and comparison capabilities.

It must open images quickly, remain responsive with large files, support common image formats, and provide straightforward conversion to PNG and JPEG. Editing should be non-destructive until the user saves or exports the result.

The interface must follow GNOME conventions rather than resemble a cross-platform graphics editor. GTK-rs provides Rust bindings for GTK 4 and the GNOME stack, while libadwaita provides the adaptive widgets and styling expected of modern GNOME applications. ([gtk-rs.org][1])

## 2. Product Goals

1. Feel indistinguishable from a native GNOME application.
2. Open ordinary images nearly instantly.
3. Maintain smooth zooming and panning, including on high-resolution displays.
4. Support safe decoding of common and modern image formats.
5. Provide useful editing without becoming a full layer-based graphics editor.
6. Keep all processing local by default.
7. Make every primary operation available through the keyboard.
8. Preserve image quality, metadata, transparency, and color profiles where possible.

## 3. Non-Goals

The initial product is not intended to provide:

* Multi-layer document editing.
* Text, vector shape, or typography tools.
* Cloud storage or account synchronization.
* Batch photo-library management.
* RAW development controls comparable to a photography application.
* Advanced painting features such as brushes, blending modes, or layer masks.
* Video editing.

## 4. Technical Foundation

### 4.1 Core stack

* Stable Rust toolchain.
* `gtk4` through gtk-rs.
* `libadwaita-rs`.
* GLib and GIO for actions, settings, files, asynchronous work, and directory monitoring.
* Glycin as the preferred decoding and metadata layer.
* GTK/GSK rendering through a custom image-canvas widget.
* Meson for GNOME packaging and desktop integration, with Cargo responsible for Rust compilation.

Glycin supports sandboxed, modular image loading and exposes a native Rust API. Its current format coverage includes PNG/APNG, JPEG, GIF, WebP, AVIF, HEIF, BMP, TIFF, JPEG 2000, JPEG XL, SVG, QOI, ICO, EXR, PNM, TGA, XBM and XPM, with experimental support for several RAW formats. ([gnome.pages.gitlab.gnome.org][2])

### 4.2 Processing model

The GTK main thread must only perform UI state updates and lightweight rendering submission. Decoding, resizing, palette reduction, seam carving, AI inference and file export must run in cancellable worker tasks. GTK-rs documentation explicitly demonstrates moving blocking work off the main loop and returning results through asynchronous channels. ([gtk-rs.org][3])

Recommended worker groups:

* Decode queue: up to two concurrent operations.
* Image-processing pool: based on available CPU cores.
* Export queue: one active export per document.
* AI inference queue: one active inference session.
* Thumbnail and prefetch queue: low priority.

Opening another image or changing a processing parameter must cancel obsolete work.

## 5. User Interface

### 5.1 Main window

The primary window contains:

* A standard libadwaita header bar.
* A central image canvas.
* An optional bottom overlay containing zoom controls.
* A contextual editing toolbar shown only in edit mode.
* Toast notifications for non-blocking feedback.
* Dialogs for export, scaling, properties and destructive confirmations.

The header bar must contain only frequently used actions:

**Start side**

* Previous image.
* Next image.

**Center**

* File name.
* Modified indicator when unsaved operations exist.
* Optional dimensions or zoom percentage as secondary text.

**End side**

* Compare mode.
* Edit mode.
* Save As.
* Main menu.

Header bars should contain a limited number of contextual controls, with tooltips on icon buttons, in accordance with the GNOME Human Interface Guidelines. ([GNOME Developer][4])

### 5.2 Main menu

The main menu contains:

* Open.
* Save.
* Save As.
* Image properties.
* Preferences.
* Keyboard Shortcuts.
* Help.
* About.

### 5.3 Edit mode

Selecting Edit changes the window into a temporary editing state:

* The header shows **Cancel** and **Apply**.
* A bottom tool rail shows Crop, Scale, Transform, Palette, Select Object and Pencil.
* Only controls relevant to the selected tool are shown.
* Escape exits the current tool or requests cancellation when edits would be lost.

## 6. Document and Editing Model

Each open image is represented by an immutable source plus an ordered operation stack.

Example stack:

1. Orientation correction.
2. Crop.
3. Rotation or flip.
4. Scale.
5. Palette reduction.
6. AI mask or cutout.
7. Pencil strokes.

Operations must remain non-destructive until Save or Save As.

The document model must support:

* Undo and redo.
* Operation removal and replacement.
* Cached intermediate results.
* Dirty-state tracking.
* Export from the complete operation stack.
* Restoration of the original image during the session.
* Atomic saving through a temporary file followed by replacement.

Saving must never leave a partially written output file.

## 7. Image Loading and Format Support

### 7.1 Required viewing formats

At minimum:

* PNG and APNG.
* JPEG.
* GIF.
* WebP.
* AVIF.
* HEIF/HEIC where the required decoder is installed.
* BMP.
* TIFF.
* SVG and compressed SVG.
* JPEG 2000.
* JPEG XL where available.
* QOI.
* ICO.
* EXR.
* PNM family.
* TGA.
* XBM and XPM.

### 7.2 Animated images

GIF, APNG, animated WebP and other supported animation formats must play automatically.

Controls:

* Play or pause.
* Previous and next frame.
* Animation loop indication.

Editing an animated image must display a choice:

* Edit the current frame.
* Flatten the currently displayed frame.
* Cancel.

Animation editing is outside the first release.

### 7.3 Color management and metadata

The application must:

* Apply embedded orientation metadata.
* Respect ICC and CICP color information where available.
* Render into the active display color space where supported.
* Preserve compatible EXIF, XMP and ICC metadata on export.
* Allow metadata removal from the Save As dialog.
* Display metadata without delaying the initial image preview.

## 8. Performance Requirements

Performance must be measured on a documented reference system.

### 8.1 Responsiveness targets

* Warm application startup: under 150 ms to an interactive window.
* Cold startup: under 400 ms on an NVMe-based reference system.
* First useful preview for a typical 24-megapixel JPEG: under 250 ms.
* Zoom and pan: 60 frames per second under normal desktop conditions.
* Pointer-to-render latency: target below one display frame.
* Palette slider preview: visible update within 100 ms.
* No blocking task longer than 8 ms on the GTK main thread.

### 8.2 Memory management

* Decode only the current image and a limited number of neighboring previews.
* Use downsampled previews when the display size does not require full resolution.
* Generate mip levels lazily.
* Use tiled rendering for extremely large images where the decoder allows it.
* Default decoded-image cache: the lower of 512 MB or 25% of available memory.
* Release full-resolution buffers when the window is minimized or memory pressure is reported.
* Avoid duplicate RGBA buffers between the document, preview and renderer.

### 8.3 Navigation prefetching

After displaying the current file:

1. Prefetch metadata for the previous and next files.
2. Decode screen-sized previews for both.
3. Cancel distant prefetching immediately when navigation direction changes.

## 9. Zoom and View Controls

### 9.1 Zoom modes

The image canvas supports:

* Fit to window.
* Fill window.
* Actual size, or 100%.
* Arbitrary zoom from 1% to at least 6400%.
* Pointer-centered wheel zoom.
* Pinch-to-zoom.
* Click-and-drag panning.
* Middle-button panning.
* Scrollbar panning when the image exceeds the viewport.

### 9.2 Soft and hard zoom

A single toggle switches the rendering filter:

**Soft zoom**

* Smooth interpolation.
* Default for photographs and downscaling.
* Uses linear filtering for interactive rendering.
* Higher-quality resampling is used during final export.

**Hard zoom**

* Nearest-neighbor sampling.
* Pixel edges remain sharp.
* Preferred for pixel art, icons and technical inspection.

The selected mode must be saved in GSettings and restored across application launches.

At exactly 100%, pixels should be aligned to physical device pixels when possible.

### 9.3 Transparency background

Required background states:

* Checkerboard.
* White.
* Gray.
* Black.

Because four states are required but the requested interface specifies three visible icons, the recommended compact implementation is:

1. Checkerboard icon.
2. Light-background icon with white and gray choices in a popover.
3. Black-background icon.

The last selected state is remembered.

The transparency background is a viewing aid and must not alter exported pixels unless the user exports to JPEG, which cannot preserve alpha.

## 10. Keyboard Shortcuts

The application must use GActions so toolbar controls, menus and shortcuts invoke the same commands. Stateful GActions should be used for persistent toggles such as zoom filtering and background mode. ([gtk-rs.org][5])

Required shortcuts:

| Action                  | Shortcut           |
| ----------------------- | ------------------ |
| Open                    | Ctrl+O             |
| Save                    | Ctrl+S             |
| Save As                 | Ctrl+Shift+S       |
| Close window            | Ctrl+W             |
| Preferences             | Ctrl+,             |
| Keyboard Shortcuts      | Ctrl+?             |
| Undo                    | Ctrl+Z             |
| Redo                    | Ctrl+Shift+Z       |
| Zoom in                 | + or Ctrl++        |
| Zoom out                | - or Ctrl+-        |
| Actual size             | 1                  |
| Fit to window           | 2                  |
| Fill window             | 3                  |
| Toggle soft/hard zoom   | X                  |
| Previous image          | Left or Page Up    |
| Next image              | Right or Page Down |
| Rotate clockwise        | R                  |
| Rotate counterclockwise | Shift+R            |
| Horizontal flip         | H                  |
| Vertical flip           | V                  |
| Crop                    | C                  |
| Compare                 | D                  |
| Pencil                  | P                  |
| Object selection        | A                  |
| Fullscreen              | F11                |
| Exit active tool        | Escape             |

Standard GNOME shortcuts must take precedence where there is a conflict. GNOME recommends standard shortcuts for Open, Save, Save As, Undo, Redo, Preferences and the shortcuts dialog, and requires keyboard access to primary functionality. ([GNOME Developer][6])

## 11. Save As and Conversion

Save As must initially support:

### PNG

* Preserve transparency.
* Compression-level control hidden under Advanced.
* Preserve or remove metadata.
* Preserve embedded color profile.
* Optional conversion to sRGB.

### JPEG

* Quality slider, default 92.
* Chroma subsampling selection under Advanced.
* Background choice when the source contains transparency.
* Preserve or remove metadata.
* Embed sRGB or compatible source profile.

Any successfully decoded image can therefore be converted to PNG or JPEG.

Export must:

* Run in the background.
* Show progress for operations expected to exceed 500 ms.
* Be cancellable.
* Warn before overwriting.
* Use the GNOME file chooser portal under Flatpak.
* Remember the last export quality settings separately for PNG and JPEG.

## 12. Compare Mode

### 12.1 Entering compare mode

Selecting Compare opens a file chooser for the comparison image. Drag-and-drop onto the compare button is also supported.

The current image becomes **Image A** and the selected image becomes **Image B**.

### 12.2 Split orientation

The initial split is 50/50.

* Predominantly landscape images use a vertical divider and side-by-side panels.
* Predominantly portrait images use a horizontal divider and top/bottom panels.
* For mixed orientations, select the split that provides the larger combined display area.
* The divider can be dragged but is reset to 50/50 when compare mode is reopened.

### 12.3 Synchronized view

By default:

* Both panels use the same normalized zoom.
* Panning one panel pans the other.
* Scrollbars represent the shared viewport.
* Changing soft/hard zoom affects both.
* Background mode affects both.

A lock button can disable synchronized pan and zoom.

When dimensions differ, coordinates are mapped using normalized image-space coordinates. An optional pixel-coordinate mode may be provided for images with identical or nearly identical dimensions.

### 12.4 Comparison lens

Hovering over either image displays a circular lens on the opposite image.

Lens behavior:

* The lens center represents the corresponding normalized image coordinate.
* Default diameter: 180 logical pixels.
* Default additional magnification: 4×.
* The lens is clipped to a circle with a visible outline and center marker.
* The source cursor position is indicated by a subtle ring.
* Alt+mouse wheel adjusts lens magnification.
* Shift+mouse wheel adjusts lens diameter.
* The lens disappears when the pointer leaves the image.
* The lens must update at display refresh speed without triggering full-image reprocessing.

## 13. Palette Reduction

### 13.1 Controls

The Palette tool contains:

* Color-count slider from 2 to 256.
* Numeric color-count entry.
* Dithering toggle.
* Accent preservation toggle, enabled by default.
* Reset button.
* Optional protected-color swatches.

### 13.2 Real-time preview

While the slider is moving:

* Process the visible viewport or a reduced-resolution preview.
* Replace obsolete jobs instead of queuing every slider value.
* Update within 100 ms on the reference system.

After the slider stops:

* Render a full-resolution result in the background.
* Replace the preview without visual jumping.
* Cache recent palette sizes for quick backtracking.

### 13.3 Accent and isolate preservation

The quantizer should operate in a perceptual color space such as OKLab.

The algorithm must:

1. Identify highly saturated accent clusters.
2. Detect rare colors that occupy small but spatially coherent areas.
3. Reserve a limited number of palette entries for those colors.
4. Quantize the remaining image independently.
5. Avoid merging isolated interface indicators, eyes, highlights or logos into dominant background colors.
6. Allow the user to protect additional colors with an eyedropper.

Accent preservation must be deterministic for identical input and settings.

## 14. Flip, Rotate and Scale

### 14.1 Flip

* Horizontal flip.
* Vertical flip.
* Instant preview.
* Lossless in the internal operation stack.

### 14.2 Rotate

* 90° clockwise.
* 90° counterclockwise.
* 180°.
* Optional arbitrary-angle rotation in the advanced transform dialog.
* Transparent expansion or automatic crop for arbitrary angles.

### 14.3 Scale dialog

Controls:

* Width.
* Height.
* Percentage.
* Lock aspect ratio.
* Scale-up warning.
* Resampling mode.
* Estimated output dimensions and memory use.

Resampling modes:

**Nearest Neighbor**

* Pixel replication.
* Intended for pixel art and masks.

**Linear**

* Fast bilinear interpolation.
* Intended for previews and low-cost resizing.

**Bicubic**

* Default final-quality mode for photographs and general images.

**None**

* Available only when no pixel resampling is required, such as crop, flip and rotations in 90° increments.
* Disabled for arbitrary resizing.

**Seam Carving**

* Content-aware resizing.
* Initial implementation supports shrinking.
* Runs as a cancellable background operation.
* Provides a quick preview followed by a full-quality result.
* Warns when the requested reduction is likely to create severe distortion.
* May later support user-painted protect and remove masks.

## 15. Crop and Crop to Content

### 15.1 Manual crop

* Draggable corner and edge handles.
* Movable crop rectangle.
* Grid overlay.
* Free aspect ratio.
* Original aspect ratio.
* Common presets such as 1:1, 4:3, 3:2 and 16:9.
* Numeric position and dimensions.
* Arrow-key movement.
* Shift+arrow for larger movement increments.
* Enter applies the crop.
* Escape cancels it.

### 15.2 Crop to content

For images with alpha:

* Find the bounding rectangle of pixels above an adjustable alpha threshold.
* Default threshold: alpha greater than 1/255.

For opaque images:

1. Sample border and corner colors.
2. Estimate the dominant background color.
3. Flood-fill connected border regions within a tolerance.
4. Crop to the remaining content bounds.
5. Display the detected bounds before applying.

The tool must provide a tolerance slider and a reset option. It must not silently crop when confidence is low.

## 16. Nautilus-Based File Navigation

When an image is opened, the application enumerates supported images in the parent directory and builds the previous/next navigation sequence.

The application should read Nautilus/GVFS per-directory metadata when available, including persisted sort column and reversed-order values. Nautilus has used metadata keys such as `metadata::nautilus-list-view-sort-column`, `metadata::nautilus-list-view-sort-reversed` and icon-view sorting metadata. ([GitLab GNOME][7])

Supported sort mappings should include:

* Name.
* Modification date.
* Creation date where available.
* Size.
* File type.
* Access date where available.
* Reversed order.

Additional requirements:

* Use locale-aware natural filename comparison.
* Filter unsupported files after sorting rather than before when necessary to match visible Nautilus order.
* Monitor the directory with `GFileMonitor`.
* Add, remove or rename navigation entries without closing the image.
* Preserve the current image when the directory changes.
* Handle local and GIO-backed locations.

### Integration limitation

This feature is best-effort. The app must not rely on controlling or inspecting a live Nautilus window. Nautilus persists some view state through metadata, while recent Nautilus development has deprecated or removed older public file-operation interfaces; therefore, a stable live-window sort API cannot be assumed. This is an implementation inference from the available Nautilus metadata and interface history. ([GitLab GNOME][7])

When Nautilus metadata is missing or unsupported:

1. Use the application’s last selected folder sort.
2. Otherwise fall back to natural filename order.
3. Show the active order in the image menu.

## 17. AI Object Selection and Cutout

### 17.1 User interaction

Activating Select Object changes the cursor to a selection cursor.

Workflow:

1. The user clicks an object.
2. Local inference generates a candidate mask.
3. The mask appears as a translucent overlay.
4. Additional left clicks or strokes add foreground hints.
5. Right-clicks or modifier-assisted strokes add background hints.
6. The user adjusts edge feathering.
7. Apply converts the mask into a non-destructive selection operation.

Available actions:

* Cut the object, leaving transparency.
* Copy the object to the clipboard.
* Delete the selected region.
* Invert the selection.
* Save the selected object as PNG.
* Refine edges.
* Undo individual refinement prompts.

### 17.2 AI requirements

* Inference must be local.
* The core viewer must work without the AI model.
* The model should be distributed as an optional Flatpak extension or separately installable component.
* Model size, license and storage use must be disclosed before installation.
* CPU inference is mandatory.
* Hardware acceleration is optional.
* No image data may be uploaded without a separately designed and explicit online feature.

The model backend must be replaceable. A promptable segmentation model such as SAM 2 is a possible reference because it supports point- and box-prompted masks for static images, but the shipped model should be selected according to desktop CPU latency and package size rather than benchmark quality alone. ([GitHub][8])

An ONNX-based inference layer may be used to support different hardware execution providers while maintaining a common model interface. ([ONNX Runtime][9])

## 18. Pencil Tool

When Pencil is active:

* Left-button drag paints.
* Right-click samples the color beneath the pointer.
* Right-button drag continuously samples while moving.
* The context menu is disabled over the image while the tool is active.
* Escape exits the tool.
* Each continuous stroke is one undo operation.

Controls:

* Color.
* Width from 1 to 128 image pixels.
* Opacity from 1% to 100%.
* Hardness, default 100%.
* Optional pressure sensitivity for supported tablets.
* Pixel-aligned drawing in hard-zoom mode.

Rendering requirements:

* Display a live circular brush outline.
* Accumulate pointer events into a smooth stroke.
* Avoid repeatedly copying the full image during painting.
* Update only affected tiles.
* Preserve exact sampled RGBA values when possible.
* Show the sampled color in both hexadecimal and RGBA form.

## 19. Settings

Persist through GSettings:

* Soft or hard zoom.
* Transparency background.
* Window dimensions and maximized state.
* Last zoom mode.
* Default folder sort fallback.
* Metadata-preservation preference.
* PNG export settings.
* JPEG quality and transparency background.
* Palette dithering preference.
* Compare lens size and magnification.
* AI model installation state.

Per-image zoom and pan should not normally persist after closing the file.

## 20. Accessibility

* Every icon control has an accessible name and tooltip.
* Every action is keyboard accessible.
* Tool state is announced to assistive technologies.
* The application supports high-contrast mode.
* Selection and crop indicators do not rely solely on color.
* Focus order follows the visual control order.
* Sliders expose numeric values.
* Compare panels have distinct accessible labels.
* Keyboard-only operation is included in acceptance testing.

## 21. Error Handling

Use non-blocking toasts for recoverable errors and dialogs for errors requiring a decision.

Required error states:

* Unsupported file.
* Corrupt or incomplete image.
* Missing format decoder.
* Image dimensions exceed safe limits.
* Insufficient memory.
* File changed externally.
* File deleted or moved externally.
* Save permission denied.
* Export cancelled.
* AI model unavailable.
* AI inference failed.

Decoder error details may be shown in an expandable technical-details section but should not replace a plain-language message.

## 22. Security and Privacy

* Prefer Glycin sandboxed decoders.
* Use file chooser portals in Flatpak.
* Treat filenames and metadata as untrusted.
* Impose configurable dimension and decoded-memory limits.
* Reject arithmetic overflow when calculating image buffer sizes.
* Do not permit image decoders to initiate network requests.
* Run AI inference locally.
* Do not collect telemetry by default.
* Avoid writing thumbnails or recovered edits outside standard cache and state directories.

## 23. Suggested Module Layout

```text
src/
  application.rs
  window/
  actions/
  document/
    model.rs
    operation.rs
    history.rs
  image/
    loader.rs
    metadata.rs
    color.rs
    animation.rs
  canvas/
    widget.rs
    viewport.rs
    renderer.rs
    tiles.rs
  navigation/
    directory.rs
    nautilus_sort.rs
    prefetch.rs
  tools/
    crop.rs
    scale.rs
    transform.rs
    palette.rs
    pencil.rs
    selection.rs
  compare/
    view.rs
    mapping.rs
    lens.rs
  ai/
    backend.rs
    model.rs
    mask.rs
  export/
    png.rs
    jpeg.rs
    atomic_write.rs
  settings.rs
  error.rs
```

The document and processing layers should not depend directly on GTK widgets. This allows headless tests for decoding, transformations and export.

## 24. Delivery Phases

### Phase 1: Viewer foundation

* GTK 4/libadwaita shell.
* Sandboxed image loading.
* Smooth and hard zoom.
* Transparency backgrounds.
* Keyboard shortcuts.
* Directory navigation.
* Nautilus sort metadata integration.
* Prefetch and cache.
* PNG/JPEG conversion.

### Phase 2: Core editing

* Non-destructive document model.
* Undo and redo.
* Flip and rotate.
* Manual crop.
* Crop to content.
* Scale with nearest, linear and bicubic modes.
* Metadata and color-profile preservation.

### Phase 3: Advanced interactive tools

* Compare mode.
* Synchronized scrolling.
* Comparison lens.
* Palette reduction.
* Pencil tool.
* Large-image and tile optimizations.

### Phase 4: Computational features

* AI object selection.
* Edge refinement.
* Seam carving.
* Optional hardware acceleration.
* Additional export formats.

## 25. Release Acceptance Criteria

The first stable release is acceptable when:

1. All required static formats open or produce a clear missing-decoder message.
2. PNG, JPEG, GIF, WebP, AVIF, HEIF, TIFF and SVG are covered by automated integration tests.
3. Soft/hard zoom is remembered across restarts.
4. All four transparency backgrounds are available and remembered.
5. PNG and JPEG Save As operations are atomic.
6. Keyboard-only navigation, zooming, editing and exporting work.
7. Moving to the next or previous prefetched image produces no visible blank frame.
8. Compare panels remain synchronized at different zoom levels.
9. The comparison lens maps corresponding coordinates correctly for unequal image sizes.
10. Palette reduction updates interactively and preserves protected colors.
11. Crop to content previews its result before applying.
12. Pencil right-click sampling returns the displayed source color.
13. AI object selection performs no network requests.
14. Cancelling a long-running scale, palette, AI or seam-carving operation restores the previous document state.
15. Corrupt and adversarial test images cannot allocate memory beyond configured limits.
16. The GTK main thread remains responsive during decoding and export.
17. The application passes keyboard, screen-reader and high-contrast smoke tests.
18. Unsaved edits cannot be discarded without explicit confirmation.

The next useful artifact would be an implementation backlog with epics, acceptance tests, and a proposed Cargo workspace and dependency layout.

[1]: https://gtk-rs.org/?utm_source=chatgpt.com "gtk-rs: Unlocking the GNOME stack for Rust"
[2]: https://gnome.pages.gitlab.gnome.org/glycin/ "Glycin – Safe image loading and editing"
[3]: https://gtk-rs.org/gtk4-rs/git/book/main_event_loop.html "The Main Event Loop - GUI development with Rust and GTK 4"
[4]: https://developer.gnome.org/hig/patterns/containers/header-bars.html?utm_source=chatgpt.com "Header Bars - GNOME Human Interface Guidelines"
[5]: https://gtk-rs.org/gtk4-rs/git/book/actions.html "Actions - GUI development with Rust and GTK 4"
[6]: https://developer.gnome.org/hig/reference/keyboard.html?utm_source=chatgpt.com "Standard Keyboard Shortcuts"
[7]: https://gitlab.gnome.org/GNOME/nautilus/-/issues/771?utm_source=chatgpt.com "Missing UI for changing and using the default sort order (#771)"
[8]: https://github.com/facebookresearch/sam2 "GitHub - facebookresearch/sam2: The repository provides code for running inference with the Meta Segment Anything Model 2 (SAM 2), links for downloading the trained model checkpoints, and example notebooks that show how to use the model. · GitHub"
[9]: https://onnxruntime.ai/docs/execution-providers/ "Execution Providers | onnxruntime"
