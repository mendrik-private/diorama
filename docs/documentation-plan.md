# Documentation and screenshot plan

## Status

`build-aux/build-docs.sh` builds the GitHub Pages guide from
`docs/user-guide/`. The Pages workflow validates it for pull requests to
`main` and `develop` and for pushes to those branches. A direct push to `main`
deploys the static site. Releases use the same read-only documentation build
workflow before publication.

The guide's twenty-six screenshot filenames are part of the build contract. Their
source directory is `data/screenshots/guide/`; the documentation build copies
them into an isolated temporary book and never writes generated assets into
the checkout. Keep every required capture versioned with the guide; verify
deployment through the workflow and release checklist after merging.

## Reader structure

The published guide is task-oriented: opening and browsing; pixel and metadata
inspection; comparison; selection and cropping; transformations; background
removal and clipboard objects; drawing; whole-image and game-asset scaling;
preferences; saving; and troubleshooting. Existing engineering documents in
`docs/` remain separate from the user guide.

## User-facing capability coverage

This matrix is the release-maintenance inventory. The source reference is the
authority for label and reachability; the page is the reader-facing place that
must be changed when behavior changes. Review it beside `main_menu()` in
[`src/window/mod.rs`](../src/window/mod.rs) and `SHORTCUTS` in
[`src/application.rs`](../src/application.rs), rather than treating a README
feature list as authoritative.

| Reachable capability | Source reference | Guide page |
| --- | --- | --- |
| File: Open, Open With, Copy Image or Selection, Copy Filepath, Save, Save As, Print | `src/window/mod.rs`: `main_menu`, `install_actions`, clipboard and print handlers | [Get started](user-guide/src/getting-started.md), [Save and export](user-guide/src/save-and-export.md), [Background removal](user-guide/src/background-removal.md) |
| Sequence browsing, supported formats, folder sort, delete, normal fullscreen, image preview, zoom/pinch/minimap, fit, hard/soft zoom | `src/window/mod.rs`: navigation, minimap, `image-preview`, zoom actions; `src/navigation/directory.rs`; `src/application.rs`: `SHORTCUTS` | [Get started](user-guide/src/getting-started.md), [Preferences](user-guide/src/preferences.md), [Shortcuts](user-guide/src/shortcuts-and-troubleshooting.md) |
| Animated-image play/pause and frame stepping | `src/window/mod.rs`: `start_animation`, `toggle_animation`, `step_animation` | [Get started](user-guide/src/getting-started.md), [Shortcuts](user-guide/src/shortcuts-and-troubleshooting.md) |
| Select Region; region zoom, crop, copy, cutout, and background fill | `src/window/mod.rs`: region toolbar, `start_selection_preparation`, `activate_prepared_selection`, `fill_selected_region_with_background` | [Crop and select](user-guide/src/cropping.md), [Background removal](user-guide/src/background-removal.md) |
| Compare Images and Magnifying Lens | `src/window/mod.rs`: compare/lens actions and comparison controls | [Compare](user-guide/src/compare.md), [Inspect](user-guide/src/inspect.md) |
| Measure, Color Picker, Pencil, Highlight, Arrow, Text, and object editing | `src/window/mod.rs`: tool actions/header widgets; `src/window/annotation.rs` | [Inspect](user-guide/src/inspect.md), [Draw and annotate](user-guide/src/drawing-and-annotations.md) |
| Rotate, whole-image flips, Scale | `src/window/mod.rs`: transform actions and scale panel; `src/window/scale.rs` | [Transform](user-guide/src/transformations.md), [Scale](user-guide/src/scaling.md) |
| Square Up and Canvas Resize | `src/window/canvas_resize.rs` | [Transform](user-guide/src/transformations.md) |
| Warp Mesh, grid guides, nodes, preview, clear, apply, mesh-local undo/redo | `src/window/mesh.rs`; mesh toolbar in `src/window/mod.rs` | [Transform](user-guide/src/transformations.md), [Preferences](user-guide/src/preferences.md) |
| Crop to Content | `src/window/mod.rs`: `crop_to_content` | [Crop and select](user-guide/src/cropping.md) |
| Image Properties, Preferences, Keyboard Shortcuts, About | `src/window/mod.rs`: `show_properties`, `show_preferences`, `show_shortcuts`, `main_menu` | [Inspect](user-guide/src/inspect.md), [Preferences](user-guide/src/preferences.md), [Shortcuts](user-guide/src/shortcuts-and-troubleshooting.md) |
| Preference rows: hard zoom, display background, lens size, anti-aliasing, copied-colour format, pasted-image resampling, mesh grid spacing/offsets | `src/window/mod.rs`: `show_preferences`; `src/settings.rs` | [Preferences](user-guide/src/preferences.md) |
| Export format, PNG compression/metadata/ICC, JPEG quality/background, cancellation, external-change behavior | `src/window/mod.rs`: save/export dialog and export worker | [Save and export](user-guide/src/save-and-export.md) |
| Game Asset model configuration and resampling strength; local BiRefNet, LaMa, and FLUX setup | `src/window/mod.rs`: scale panel; `src/tools/scale/game_asset/`; local worker modules | [Scale game assets](user-guide/src/game-asset-scaling.md), [Set up local models](user-guide/src/model-setup.md) |
| Reduce Palette action | `src/window/mod.rs`: `show_palette_dialog` and `win.palette` | [Shortcuts](user-guide/src/shortcuts-and-troubleshooting.md): deliberately marked unavailable because `main_menu()` and visible toolbars contain no entry. Do not document it as a workflow until a reachable control exists. |

Actions can be disabled when an image is unavailable, still decoding, or cannot
be edited. A release that changes an availability rule must update the affected
procedure and troubleshooting text.

## Screenshot contract

Every guide screenshot is a real capture of Diorama and links to its full-size
PNG. Capture at a consistent window size and theme, with readable controls and
one task per frame. Required files are:

| File | Demonstrates |
| --- | --- |
| `overview.png` | Open image and main viewing workspace |
| `inspect-pixel-lens.png` | Pixel lens over a face |
| `compare-lenses.png` | Synchronized comparison and paired lenses |
| `edit-selection.png` | Pixel-aligned selection on the character |
| `annotate-character.png` | Highlight, curved arrow, and curved text |
| `export-options.png` | Export format and preservation choices |
| `scale-options.png` | Exact dimensions, resampling, and preview controls |
| `scaling-method-comparison.png` | Visual result of scaling-method choices |
| `game-asset-options.png` | Game Asset strength and preview controls |
| `game-asset-result.png` | Game Asset result at output size |
| `game-asset-line-art.png` | Generated line-art inspection |
| `game-asset-vs-bicubic-cave-spider-128.png` | Cave spider: Bicubic and Game Asset at 128 × 128 |
| `game-asset-vs-bicubic-cave-spider-180.png` | Cave spider: Bicubic and Game Asset at 180 × 180 |
| `game-asset-vs-bicubic-cave-spider-256.png` | Cave spider: Bicubic and Game Asset at 256 × 256 |
| `game-asset-vs-bicubic-goblin-128.png` | Goblin: Bicubic and Game Asset at 128 × 128 |
| `game-asset-vs-bicubic-goblin-180.png` | Goblin: Bicubic and Game Asset at 180 × 180 |
| `game-asset-vs-bicubic-goblin-256.png` | Goblin: Bicubic and Game Asset at 256 × 256 |
| `game-asset-vs-bicubic-dragon-128.png` | Dragon: Bicubic and Game Asset at 128 × 128 |
| `game-asset-vs-bicubic-dragon-180.png` | Dragon: Bicubic and Game Asset at 180 × 180 |
| `game-asset-vs-bicubic-dragon-256.png` | Dragon: Bicubic and Game Asset at 256 × 256 |
| `flip-and-rotate.png` | Whole-image rotation and flipping |
| `mesh-warp-grid.png` | Mesh nodes, guide grid, and deformation preview |
| `crop-detected-content.png` | Detected-content crop confirmation |
| `canvas-resize.png` | Canvas-growth preview |
| `preferences.png` | Viewer, drawing, and edit settings |
| `background-removal-cutout.png` | Movable cutout and repaired background |

The current expanded workflow captures are `scale-options.png`,
`scaling-method-comparison.png`, `game-asset-options.png`,
`game-asset-result.png`, `game-asset-line-art.png`, the nine
`game-asset-vs-bicubic-*.png` comparisons, `flip-and-rotate.png`,
`mesh-warp-grid.png`, `crop-detected-content.png`, `canvas-resize.png`,
`preferences.png`, and `background-removal-cutout.png`. Keep these in the
build contract with their Markdown references, so a deployed chapter cannot
silently lose visual evidence. The screenshot maintainer owns capture files;
the documentation maintainer owns matching alt text, caption, and procedure.

Use the fantasy-character files in
`/home/mendrik/Pictures/game-assets/characters-midwalk-v2/horror-variants/dnd-fantasy`
only when a maintainer reproduces these captures locally. Do not copy that
absolute path or the private sample artwork into reader-facing instructions.
For the annotation capture, identify a face and wing with a curved arrow and
curved text; use a contrasting highlight colour, never red on a red surface.

Capture provenance, including the app version, source revision, original-image
hashes, and compositor command, is recorded in [the guide screenshot
provenance](../data/screenshots/guide/README.md). Reproduce the captures with a
Wayland compositor running, then run:

```sh
bash build-aux/capture-guide-screenshots.sh
```

The nine `game-asset-vs-bicubic-*.png` monster comparisons are captured by a
separate harness. Pass the directory containing the supplied monster originals
as its optional first argument, or set `DIORAMA_SCALE_COMPARISON_SOURCE_ROOT`:

```sh
bash build-aux/capture-scale-comparison-screenshots.sh /path/to/monster-originals
```

Their exact source filenames, hashes, and reproduction command belong in the
guide screenshot provenance alongside the generated captures.

The guide build requires POSIX `sh`, mdBook `0.5.4`, and Ruby with its standard
library. Screenshot capture additionally requires Bash, Cargo/Rust, a Wayland
display, and the local sample images. Capturing Game Asset and
background-removal evidence also requires the installed local FLUX.2 [klein],
BiRefNet, and their Python/runtime dependencies described in
[Set up local models](user-guide/src/model-setup.md). The capture harness uses
real inference and fails if those previews cannot complete.

## Release maintenance

Before finalizing a release, compare every user-visible change against the
coverage matrix above, then update the guide whenever a user-visible feature,
shortcut, label, workflow, file-format claim, or screenshot has changed. Run
`sh build-aux/build-docs.sh`; it fails for a missing screenshot, link, or
image. Inspect the rendered guide at the repository Pages subpath
`/diorama/`, then include the documentation result in the release checklist.

When a capture changes, replace its PNG with a real app capture, keep its
documented filename unless the Markdown is updated in the same change, and
check the full-size link and alt text. Do not stage mockups or edited UI
screenshots as product evidence.
