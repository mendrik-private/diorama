# Diorama — Annotation Tools, Tool Model and Hard-Zoom Alignment
**Status:** Draft for review · **Target release:** 0.3.0 · **Supersedes:** spec.md §5.3 (tool rail), §17 (AI object selection), §18 controls list (partially)
**Baseline:** working tree after the window-module refactor (commit 71f180b plus uncommitted `src/window/*` split)

## 0. Scope

In scope: unified tool model and annotation palette; default colour; overlay styling; removal of object segmentation; removal of the transient measure tool; device-pixel-exact hard zoom; a vector annotation layer with five tools (Pencil, Highlight, Arrow, Measure, Text); selection, editing, deletion, undo/redo; Excalifont bundling.

Out of scope (explicit non-goals for 0.3): multiple selection, z-order controls, grouping, sidecar persistence of annotations after the file is closed, multi-line text, arbitrary imported vector paths, vector annotation tools inside Compare mode, copying annotations between images.

## 1. Terminology

| Term | Meaning |
|---|---|
| Image space | Pixel coordinates of the *rendered* raster (after all raster operations), origin top-left, y down, `f32`. |
| Widget space | Logical pixels of `ImageCanvas`. |
| Device space | Physical pixels of the GDK surface; `render_scale` = `gdk::Surface::scale()` (may be fractional). |
| Raster operation | `Operation` variants that change pixels: Crop, Rotate, Flip, Scale and Palette. |
| Annotation | An editable vector object of the annotation layer: Pencil, Highlight, Arrow, Measurement or Text. |
| Annotation tool | Pencil, Highlight, Arrow, Measure, Text. Each produces annotation nodes in the primary document. |
| Palette | The bottom-left floating toolbar for annotation tools. |
| Handle | A draggable control point of the selected annotation. |

## 2. Changes to existing features

### 2.1 Unified tool model (replaces seven toggle buttons)

A single state `Tool` replaces the independent `ToggleButton`s, the `pencil_active`/`lens_active` mirrors, the seven hand-written mutual-exclusion blocks and the three parallel priority lists (Escape chain, `active_keyboard_tool`, `no_tool_active`).

```rust
enum Tool { None, Pencil, Highlight, Arrow, Measure, Text, PickColor, Select, Crop, Scale }
```

Rules:
- Exactly one `Tool` is active. `set_tool(Tool)` is the only setter; it deactivates the previous tool, updates the stateful GAction `win.tool` (string state `"none" | "pencil" | …`), palette button states, cursors, sensitivities, and the accessible tool announcement.
- `win.pencil`, `win.highlight`, `win.arrow`, `win.measure`, `win.text`, `win.pick-color`, `win.crop`, `win.scale-preview`, `win.select` remain as toggle-style actions for shortcuts and menus. For an editable image, deactivating a temporary tool returns to `Select`; activating `Select` while it is already active leaves it active.
- **Lens is no longer exclusive.** `win.lens` is an independent boolean view toggle that may be combined with any tool. The lens keeps rendering the live pencil overlay and annotation preview.
- **Eyedropper (`PickColor`) is a sub-mode.** When entered from an annotation tool it stores `return_tool` and preserves the current annotation selection; a successful pick (left click) applies the colour and restores `return_tool`. Escape follows the common cancellation order, then restores `return_tool` without picking. Otherwise Escape returns it to `Select` for an editable image.
- Crop and Scale are modal edit tools with their own bottom-right controls (unchanged). Entering them sets `Tool::Crop`/`Tool::Scale`, which hides the palette and clears the annotation selection.
- All tool actions stay gated on `editable` (`update_action_states`). Tools are reset to `Tool::None` while a different image loads, then to `Select` once an editable decode is available.
- Escape (`win.cancel-tool`) resolves in this order, first match wins: (1) cancel zoom-rectangle drag; (2) cancel open text editor; (3) cancel in-progress annotation drag (restore original geometry); (4) deselect the selected annotation; (5) leave the eyedropper sub-mode; (6) return any temporary tool to `Select` with the existing "Active tool cancelled" toast. Escape with an idle `Select` does nothing.

### 2.2 Annotation palette (fixes "selecting a colour hides the pen toolbar")

Root cause: the eyedropper button is a child of the pencil toolbar, and activating it switched the pencil off, whose setter is the only code path that hides the toolbar.

Specification:
- One palette widget, bottom-left of the canvas overlay (`halign Start`, `valign End`, margins 26/26 as today), CSS classes **`toolbar osd`** (libadwaita overlay toolbar: dark translucent rounded scrim, white foreground, theme-independent).
- Content, left to right: `[Pencil][Highlight][Arrow][Measure][Text]` · separator · `[Colour swatch][Eyedropper][Lens]` · separator · `[Size spin]`. Tool buttons are toggle buttons bound to `win.tool`; all icon buttons have tooltips and accessible names.
- **Visibility rule:** the palette is visible iff `Tool ∈ {Pencil, Highlight, Arrow, Measure, Text}` or (`Tool == PickColor` and `return_tool.is_some()`). It is never hidden by the eyedropper, the lens, colour changes, or size changes. It hides when Crop/Scale is entered or the tool is cancelled.
- **Size spin** rebinds per tool: Pencil and Arrow share `pencil-size` (1–128, default 1); Highlight → automatic, insensitive effective width for the current image (§6.1); Measure → fixed, insensitive `1` native image pixel; Text → `annotation-text-size` (6–512, default 24). Activating Arrow never changes the selected line width. Tooltip changes to describe the active value. Values are image pixels.
- **Applies to selection:** changing the colour or size while an annotation is selected updates that annotation (one history entry). Otherwise the values become defaults for the next object.
- The palette and the bottom-right zoom controls share the bottom edge; when the window is too narrow for both plus margins, the zoom controls hide (precedent: they already hide in Crop/Scale).
- Icons: ship `pencil-symbolic.svg` (replaces the non-Adwaita `xsi-edit-symbolic`), `highlight-symbolic.svg`, `arrow-symbolic.svg`, `text-symbolic.svg` under `data/icons/hicolor/scalable/actions/` and register them in `data/meson.build`; reuse the existing `ruler-measure-symbolic.svg`.

### 2.3 Default colour

- `DEFAULT_ANNOTATION_COLOR: [u8; 4] = [255, 0, 0, 255]` is the single source of truth; the swatch is initialised from it (today black is hard-coded in two places).
- The colour is **not persisted**: every start of Diorama begins with red. Right-click sampling in Pencil and the eyedropper change the current colour for the session. Sampling also cancels any pending Pencil gesture or line-chain origin so a later left click cannot reuse a stale point.

### 2.4 Floating overlay styling ("brightening" → "darkening")

Root cause: the repo has no CSS; `.linked` buttons over the canvas use libadwaita's default button wash (`currentColor` at 10 %) — under a dark theme that is a white-ish icon on a faint white wash, invisible over white images.

Specification:
- Every floating toolbar on the canvas overlay uses **`toolbar osd`**: the palette, the zoom controls, the crop controls; the scale HUD already does. Foreground is white on a dark translucent scrim regardless of system theme and image content.
- Canvas-drawn transient overlays that must be visible on any background (measurement crosshair, selection outline, handles) are drawn with the existing `BlendMode::Difference` white technique or with the black/white contrast pattern of the crop border. No canvas overlay may consist of white alone on the plain canvas.
- Minimap unchanged.

### 2.5 Removal: object segmentation

Rationale: slow, poor contours, x86_64-only build, 156 MB model download, large dependency tree.

Remove entirely: `src/ai/` (including the alpha flood-fill helper — not salvaged), `AppError::AiModelUnavailable`/`AiInference`, the click-to-segment path of the Select tool (`object_click` gesture, `detect_and_select_object`, mask flash on the canvas, the "Segmenting object…" HUD, progress fields), `win.select-object` semantics reduce to rectangle Select & Copy (renamed `win.select`), Cargo dependencies `rlx-sam2` and `hf-hub` with the `[patch.crates-io]` entry, `vendor/rlx-sam2/` and the already-unused `vendor/rlx-cpu/`, Flatpak modules `onnxruntime` and `openblas` with their env vars and the `only-arches: ["x86_64"]` restriction (the app becomes architecture-independent), the GSettings key `ai-model-installed`, README lines about SAM 2. Regenerate `Cargo.lock` and `build-aux/cargo-sources.json`.

Behaviour after removal: with `Tool::Select`, dragging selects a rectangle for copy; a click without drag does nothing.

### 2.6 Removal: transient measure tool (replaced by §6.3)

The rectangle measure tool (drag → W×H labels, clipboard copy) is removed. Kept and reused: the `ruler-measure-symbolic` icon, `win.measure` + `M`, the menu and shortcuts-dialog rows (relabelled "Measure"), grid-intersection snapping (`pixel_boundary_at`, `clamped_pixel_boundary_at`, `snapped_normalized_at`), the snapped crosshair cursor, the keyboard-cursor driving block, and the label-placement clamping logic. Dropped behaviour: clipboard copy of measurements (values are now part of the image).

### 2.7 Delete key scoping

Today `Delete` is a global accelerator for `win.delete-file`. New behaviour:
- The `Delete` accelerator is removed from the application shortcut table (`win.delete-file` stays as an action and menu item; the shortcuts dialog keeps listing Delete under Viewing).
- The window's bubble-phase key controller handles `Delete`, `KP_Delete`, `BackSpace`:
  - an annotation is selected → delete it (one history entry), stop propagation;
  - otherwise, if `Tool == None` and the key is `Delete`/`KP_Delete` → activate `win.delete-file` (confirmation dialog unchanged);
  - otherwise no-op. Focused text widgets (spin buttons, the text editor) keep their own Delete/BackSpace because the controller runs in the bubble phase.
- Test: no accelerator in `SHORTCUTS` is `"Delete"`.

## 3. Device-pixel-exact hard zoom (fixes the 1-pixel offset)

Root cause (verified against GTK sources): GSK applies `ScalingFilter::Nearest` in node space. When the texture-scale node sits under any accumulated scale ≠ 1 (every HiDPI surface), GSK renders it to an offscreen at logical resolution and composites with the linear sampler. Diorama snaps the image origin to half-logical pixels in widget space, so on a 2× surface the block grid lands one device pixel off with blended seams; odd device scales (150 %, 250 %) alternate 1/2-logical runs. A second, now live, cause: `render_scale` is derived from the integer `scale_factor()` and rounded, which is wrong under GNOME fractional scaling (non-experimental since GNOME 50).

**Invariant.** In hard zoom the texture node is emitted in a coordinate system whose unit is one device pixel (accumulated GSK scale = 1); its origin in surface device pixels is an integer; its size is `image_w × pixel_scale` by `image_h × pixel_scale` device pixels with `pixel_scale = zoom × render_scale`. All overlays and hit-testing derive from that same rect divided by `render_scale`.

Specification:
- `ImageCanvas::snapshot()` (hard filter): `snapshot.scale(1/rs)`, then `append_scaled_texture(Nearest)` with device-pixel bounds; the rect origin is aligned so `surface_origin + x` is integral, where `surface_origin` = widget origin in surface coordinates (`compute_point` to the native + `surface_transform()`), and centring slack is absorbed by ±0.5 device px. Soft filter path unchanged.
- One shared `layout()` feeds `snapshot()` and `image_bounds_for_texture()` so `pixel_at`, `normalized_at`, `crop_display_bounds` and the overlays match the drawn rect exactly.
- `render_scale` uses `gdk::Surface::scale()` (f64) with `connect_scale_notify`; `normalized_render_scale` becomes a non-rounding `sanitized_render_scale` (finite, > 0).
- `aligned_hard_zoom` and `stepped_hard_zoom` evaluate their guards in device space (`zoom × rs`) with a floor of 1 device pixel per source pixel. Behaviour change to accept: the minimum hard zoom is a true 1:1 device mapping (at 2× a 75 % request becomes 50 %; at 1.25× the "100 %" key yields 80 % logical). Zoom < 1 and unaligned pinch states keep Nearest with a snapped origin (uneven blocks during the gesture are inherent; `connect_end` re-aligns).
- `preview_scale` multiplies `pixel_scale` before layout. `Overflow::Hidden` stays.
- Redraw on adjustment `value-changed` only when the device fraction of the surface origin changed (never fires at integer scales); Compare: `paned.connect_position_notify` → `queue_draw` on both canvases.
- Hairline overlays use a shared `snap_to_device(v, rs) = round(v·rs)/rs` and `1/rs` thickness (crop border, measurement crosshair, selection outline, handles). The measurement crosshair is white in the top child of a `GskBlendNode` using `BlendMode::Difference`, producing an XOR-style inversion against every image colour.

## 4. Annotation layer — document model

### 4.1 Types (`src/document/annotation.rs`, GTK-free)

```rust
pub struct AnnotationId(pub u64);                       // document-scoped, monotonic, never reused
pub struct Point { pub x: f32, pub y: f32 }             // image space
pub struct Rect  { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }  // normalised, width/height ≥ 0
pub struct StrokeStyle { pub color: [u8; 4], pub width: f32 }                  // straight RGBA, image px
pub enum Axis { Horizontal, Vertical }

pub enum PencilGeometry {
    Freehand(Vec<BrushPoint>),
    Line(Vec<Point>),
    Rectangle(Rect),
    Ellipse(Rect),
}

pub enum Shape {
    Pencil      { geometry: PencilGeometry, style: StrokeStyle, anti_aliasing: bool },
    Highlight   { rect: Rect, seed: u64, style: StrokeStyle },
    Arrow       { start: Point, end: Point, control: Point, style: StrokeStyle },
    Measurement { axis: Axis, from: f32, to: f32, at: f32, style: StrokeStyle, label_size: f32 },
    Text        { anchor: Point, angle: f32, font_size: f32, bend: f32, text: String, color: [u8; 4] },
}
pub struct Annotation { pub id: AnnotationId, pub shape: Shape }
pub enum AnnotationEdit { Create(Annotation), Set(Annotation), Delete(AnnotationId) }
```

Field semantics: `PencilGeometry::Freehand` retains sampled pressure; the other Pencil geometries retain their editable construction geometry and are converted to the established Pencil rasterizer only while rendering. A circle is stored as an `Ellipse` with equal width and height so later non-uniform image scaling remains representable. `Highlight.rect` is the user's rectangle; the ellipse is derived. `Arrow.control` is absolute; when it equals the chord midpoint the arrow is straight. `Measurement`: the line runs along `axis` from `from` to `to` at perpendicular coordinate `at`; `from ≤ to`. `Text.anchor` is the baseline start; the baseline end is **derived** as `anchor + advance(text, font_size)·dir(angle)`; `bend` is the signed perpendicular offset of the quadratic control point from the chord midpoint (0 = straight). Text is one line, 1–256 Unicode scalar values, no newlines.

### 4.2 Operation and history

- One new variant `Operation::Annotate(AnnotationEdit)`. `History<Operation>`, `saved_operations`, dirty tracking (`PartialEq`), `restore_original` and export stay unchanged.
- The annotation set is **folded** from the active operation prefix: `fold_annotations(source_dims, ops) -> Vec<Annotation>` applies `Create`/`Set`/`Delete` and transforms geometry through every raster geometry operation encountered (§4.4). Objects mapped fully outside the image are kept (unreachable, but restored by undo of the crop).
- Undo/redo move the history cursor and refold. Granularity: one entry per completed drag (create, move, resize, rotate, bend), per text edit commit, per colour/size change on a selection, per delete. Arrow-key nudges within one "nudge session" (same object, no other apply, no redo branch) amend the last `Set` entry via `History::replace_last` / `Document::amend_annotation`.
- `Document::allocate_annotation_id()` hands out ids; the counter is not part of history.
- Creating and then deleting an object leaves two entries and marks the document dirty; accepted.

### 4.3 Rendering and compositing

- `Document::render()` replays raster operations from the best cached prefix, skipping `Annotate` entries (they never enter the render cache), then composites `fold_annotations(...)` on top of the final raster. `render_excluding(id)` renders with one annotation omitted (drag previews).
- Compositing is headless and deterministic: **`tiny-skia`** (paths, strokes and fills) into a transparent pixmap covering the union bounding box of the objects to draw, then straight-alpha "over" into the `RgbaImage` via the existing `pencil::blend`. Cancellation is checked per object. Pencil nodes render through the existing Pencil rasterizer and retain their own anti-aliasing setting. Highlight, Arrow and Text are anti-aliased independently of that setting. Measurement lines, ticks, derived gap markers and measurement labels are always rendered without anti-aliasing.
- Palette reduction applied after annotations exist does **not** quantise them (they stay live on top). Documented; a "Flatten annotations" action is a possible later addition.
- Export sees only `Document::render()` output → all annotations, including Pencil nodes, are flattened into PNG/JPEG while remaining editable in the open document.

### 4.4 Geometry transforms during the fold

Track the current dimensions `(W, H)` through the fold. Every point maps through the operation's affine map `T`; widths and font sizes scale by `s = sqrt(sx·sy)`, except that measurement strokes remain one native image pixel after every transform.

| Operation | Point map (from W×H) | Width / font | Axis / angle | Text specifics |
|---|---|---|---|---|
| `Crop{x,y,..}` | `(px−x, py−y)` | ×1 | – | – |
| `Rotate(CW90)` | `(H−py, px)` | ×1 | axis swaps; `angle += 90°` | – |
| `Rotate(CCW90)` | `(py, W−px)` | ×1 | axis swaps; `angle −= 90°` | – |
| `FlipHorizontal` | `(W−px, py)` | ×1 | axis kept; `from/to` re-normalised | `anchor' = T(end)`, `angle' = 180°−angle`, `bend' = −bend` (same footprint, glyphs stay readable) |
| `FlipVertical` | `(px, H−py)` | ×1 | same | `anchor' = T(anchor)`, `angle' = −angle`, `bend' = −bend` |
| `Scale{w,h}` | `(px·sx, py·sy)` | `× sqrt(sx·sy)`; Measurement stays `1` | `angle' = atan2(sy·sin, sx·cos)` | `font' = font·‖(sx·cos, sy·sin)‖` so the derived end lands on `T(end)`; glyphs never squash |
| `Palette` | identity | | | |

`Rect` is re-normalised after mapping. Arrow `control` maps as a point. Measurement `at` maps with the perpendicular coordinate; `from/to` are sorted after mapping.

### 4.5 Save, reload and session semantics

- Save/Save As export the flattened raster and mark the operation list as saved; the in-memory document keeps its vector annotations editable until the image is closed, reloaded or replaced.
- Reopening a saved file shows flattened pixels; there is no sidecar (non-goal).
- Undo after save works as today (prefix replay), including annotation entries.

## 5. Common interaction model

### 5.1 Selection

- Single selection, stored in the window as `Option<AnnotationId>`. Only available while an annotation tool (incl. Pencil) is active; cleared on tool cancel, Crop/Scale entry, image change, or when the object no longer exists after undo/redo.
- With any annotation tool active: pressing on an existing annotation's body or handle selects it (and starts the corresponding drag) even if a creation tool is active; pressing on empty image space starts creation with the current tool.
- Selected object shows: outline (Pencil: stored path/shape; Highlight: its rect; Arrow: chord; Text: the baseline curve), handles, and a hot-handle highlight on hover. Measurement uses its non-antialiased one-pixel rendered line as the outline and adds only the endpoint handles, so selection never changes its apparent width or pixel coverage.

### 5.2 Handles, hit-testing, cursors (`src/tools/annotation/hit.rs`, pure)

| Object | Handles | Body hit |
|---|---|---|
| Pencil freehand / rectangle / ellipse | 4 corners + 4 edge midpoints of the geometry bounds (resize; Shift keeps aspect) | within `tol + width/2` of the stored path or shape outline |
| Pencil line/polyline | `Start`, `End`, and every intermediate vertex | within `tol + width/2` of the polyline |
| Highlight | 4 corners + 4 edge midpoints of `rect` (resize; opposite edge pinned; Shift keeps aspect) | within `tol + width/2` of the ellipse polyline |
| Arrow | `Start`, `End`, `Control` | within `tol + width/2` of the flattened curve |
| Measurement | `Start`, `End` (axis-locked) | within `tol + width/2` of the segment or its labels |
| Text | `Start` (anchor), `End` (scale), `Control` (bend); **rotation ring** around `Start`/`End` | inside the rotated glyph box (or curved envelope) |

- Sizes in widget pixels: handle square 8 px; hit tolerance `tol = 8 px / image_scale` (image px); rotation ring: `8 px < d ≤ 20 px` from a Text end handle.
- Priority: handles of the selected object → rotation ring (Text) → bodies, topmost (most recently created) first.
- Cursors: create = `crosshair`; body = `move`; corner handles = `nwse-resize`/`nesw-resize`; edge handles = `ew-resize`/`ns-resize`; Start/End of line objects = `crosshair`; Control = `grab` (`grabbing` while dragging); rotation ring = custom rotate cursor from an embedded 24 px texture (`gdk::Cursor::from_texture`, fallback `grab`). Measure additionally hides the pointer and shows the snapped crosshair while hovering empty space, as today.

### 5.3 Drag state machine (`src/window/annotation.rs`)

```rust
enum AnnotationDrag {
    Create { tool: Tool, id: AnnotationId, start: Point },
    Move   { id, original: Annotation, start: Point },
    Handle { id, kind: HandleKind, original: Annotation, start: Point },
    Rotate { id, original: Annotation, center: Point, start_angle: f32 },
}
```
- One `GestureDrag` (button 1) and one `EventControllerMotion`, active only for annotation tools. `apply_drag(&drag, pointer, modifiers) -> Annotation` is pure (axis lock, aspect lock, pinned edges).
- A drag shorter than 4 image px in both axes is a click: for Highlight/Arrow/Measure nothing is created; for Text the object is placed (§6.4).
- **Live preview:** at drag begin the window requests `render_excluding(id)` (cache hit → composite only) so the committed pixels of the dragged object disappear; each motion event rasterises the dragged object (plus its dependent measurement markers) into a bbox-sized pixmap → `gdk::MemoryTexture` → canvas overlay, drawn with the same filter and image→widget mapping as the base texture (WYSIWYG, pixel-aligned in hard zoom). Handles are GSK vectors in widget space. Drag end → one `apply(Operation::Annotate(..))` → normal render.
- The canvas gets plural slots: `annotation_preview: Option<AnnotationOverlay { texture, bounds }>` and `selection: Option<SelectionHandles>`, drawn after the base texture and before lens/marker/crop overlays.

### 5.4 Keyboard

| Key | Context | Effect |
|---|---|---|
| `P` `O` `A` `M` `T` | any | toggle Pencil / Highlight / Arrow / Measure / Text |
| Escape | see §2.1 | cancel in the defined order |
| Delete, KP_Delete, BackSpace | annotation selected | delete it |
| Arrow keys / Shift+Arrows | annotation selected | nudge 1 px / 10 px (coalesced) |
| Arrow keys | no selection, annotation tool | move the keyboard cursor (existing mechanism) |
| Space / Enter | no selection | keyboard creation at the cursor: Pencil creates an editable dot; Measure anchors then commits (as today); Highlight creates a 64×40 px rect centred on the cursor; Arrow creates an 80 px horizontal arrow; Text opens the editor at the cursor |
| Enter | Text selected | open the text editor |
| Ctrl+Z / Ctrl+Shift+Z | any | undo / redo |

## 6. Tools

### 6.1 Highlight (`O`)

- Drag a rectangle; on release an `Annotation::Highlight` is created with the current colour and `seed` from a document counter mixed with the id. Effective width is derived at render time from the image's longest side as `floor(max(width, height) / 1024) + 1` native image pixels: dimensions below 1024 use 1 px, 1024–2047 use 2 px, 2048–3071 use 3 px, and so on. Exact 1024-pixel boundaries advance to the next width. The stored `style.width` remains canonical at 1 px for document compatibility and is ignored by rendering.
- Rendering: a loose hand-drawn oval made as one continuous two-curl scribble. It begins in the upper-left (angle 3.85–4.25 rad), traces exactly two full revolutions, and finishes near the first curl. Both passes share the seeded harmonic wobble, slow ±1.5% radial wander, at most ±2° evolving tilt, and at most ±2% centre drift. Around that common path they sit on opposite sides of a 3.5–4.5% base half-gap. A stronger seeded two-lobe separation wave reverses their inner/outer order several times, creating natural intersections while leaving visibly wider space elsewhere. The polyline uses round caps and joins and the image-proportional width above. All randomness comes from a 64-bit SplitMix generator seeded by `seed`; the curve is computed in a rect-relative frame so moving preserves the scribble exactly.
- Handles: 8 rect handles; body drag moves. Minimum rect 4×4 px.

### 6.2 Arrow (`A`)

- Drag from tail to head; `start` = press, `end` = release, `control` = midpoint (straight).
- Rendering: quadratic Bézier `start → control → end`, stroke width `w`; filled triangular head at `end` with length `clamp(5·w, 10, 60)` px and half-angle 25°, oriented along the curve tangent at `end`; the shaft is shortened so it does not poke through the head.
- Handles: `Start`, `End`, `Control`. Dragging `Control` bends the arrow. Dragging `Start`/`End` re-derives `control` so its chord-relative position (along/perpendicular fractions) is preserved. Double-click on `Control` resets it to the midpoint.

### 6.3 Measure (`M`) — measurement lines

- Drag creates an axis-locked line: `axis` = dominant drag component (`|dx| ≥ |dy|` → Horizontal); endpoints snap to pixel-grid intersections (`pixel_boundary_at`), so `from`, `to`, `at` are integers and all lengths are whole pixels. A snapped crosshair cursor is shown while hovering.
- Every line renders at a fixed width of one native image pixel, without anti-aliasing, in the current colour; it has perpendicular end ticks of length 3 px and its compact lowercase length label `"{n}px"` (`n = round(to − from)`) in the built-in 4×5 bitmap face inside a fixed 5×7 image-pixel cell, placed above-centre (horizontal) or right-centre (vertical), clamped inside the image. The lowercase `p` uses a descender and both suffix glyphs sit at x-height. Each ink dot is exactly one native image pixel with no anti-aliasing. Measurement labels are independent of `annotation-text-size`; the stored `label_size` field remains only for document compatibility. Scaling an image changes measurement geometry but keeps the line, ticks, and label raster at native-pixel size.
- **Pairing rule.** For lines with the same `axis`, sorted by `at`: a pair (P, Q) receives a gap marker iff `overlap = [max(from_P, from_Q), min(to_P, to_Q)]` has positive length, `|at_P − at_Q| ≥ 1`, and no third line R with `at_P < at_R < at_Q` whose extent intersects `overlap`. This yields neighbour-only markers for stacks but still pairs A–C across a non-overlapping B.
- Gap marker: a non-antialiased, one-native-pixel perpendicular segment at `mid(overlap)` from `at_P` to `at_Q`, 2 px ticks at both ends, with the same fixed 7-pixel bitmap label `"{|at_P − at_Q|}px"` upright and offset 3 px to the right (horizontal lines) or above (vertical lines) of the marker midpoint.
- Labels and markers are **derived at composite time** from all measurement lines, so every move/resize/delete re-measures immediately; during a drag the preview includes the markers that involve the dragged line.
- Handles: `Start`, `End` (locked to the axis); body drag moves the line (both coordinates, snapped to integers).

### 6.4 Text (`T`)

- Click places `anchor` at the pointer (snapped to `+0.5` pixel centres like the pencil), `angle = 0`, `font_size = annotation-text-size`, `bend = 0`. A drag sets `angle` from the drag vector (size still from the palette). A frameless single-line `gtk::Text` is placed directly over the image at the anchor; it owns native text input and IME handling while the object is previewed live in Excalifont. Its invisible layout uses the same Excalifont face, exact zoomed em size, glyph fallback policy and unshaped horizontal advances as the preview; Pango kerning, ligatures and contextual alternates are disabled so the native caret follows every rendered prefix, including consecutive spaces, to within one device pixel. While it is open, application accelerators are suspended so every typed character stays in the editor; Enter commits and Escape cancels through editor-local handling, then accelerators are restored. Empty text or clicking elsewhere discards. Enter on a selected Text (or double-click) reopens the editor; commit is one `Set`.
- Rendering: Excalifont glyph outlines via `ttf-parser`, filled with `color`, `font_size` = em size in image px. Glyphs are placed along the baseline curve (quadratic with control offset `bend`): flatten to 64 segments, arc-length parametrise, place each glyph's unshaped horizontal-advance centre at its proportional arc position and rotate by the local tangent, so the text always spans the whole curve. Unsupported code points fall back to the font's `.notdef` glyph.
- **Baseline instead of a rect:** the selection outline is the baseline curve with `Start`, `End`, `Control` handles. Dragging `End` sets `font_size = |pointer − anchor| / advance(text, 1.0)` and `angle` — uniform scaling, aspect always kept. Dragging `Start` does the same about `end`. `Control` sets `bend`. Body drag moves.
- **Rotation:** hovering in the ring outside `Start`/`End` (`8 px < d ≤ 20 px`) shows the rotate cursor; dragging rotates about the baseline chord midpoint: `anchor' = M + R(Δθ)(anchor − M)`, `angle += Δθ`; Shift snaps to 15° steps. Rotation keeps `font_size` and `bend`.
- Palette colour/size changes with a Text selected update `color`/`font_size` (one `Set`).

### 6.5 Pencil (`P`)

Pencil creates editable `Shape::Pencil` annotation nodes in the primary document. A normal drag creates `Freehand`; Ctrl creates or extends one `Line` polyline until the chain is cancelled; Shift creates a `Rectangle`; Alt creates a circular `Ellipse`. Every polyline vertex is retained and has a repositioning handle. The stored geometry, colour, width, anti-aliasing flag and freehand pressure samples remain editable and participate in undo/redo and image transforms. Rendering adapts the node to the existing Pencil rasterizer, preserving pixel-perfect one-pixel strokes and the established visual output. Rectangle and ellipse nodes have eight bounding-box handles; freehand nodes can be moved or scaled through their bounds. Changing palette colour or size updates a selected Pencil node.

Right-click sampling and the Pencil preference controls are unchanged. The size spin binds to `pencil-size`, whose fresh-profile default is 1 image pixel; existing saved preferences remain authoritative. Compare mode has no document annotation model, so Pencil marks made directly on comparison image B remain transient raster edits rather than document operations.

## 7. Excalifont bundling

- Source: Excalidraw's Excalifont (2024), **SIL Open Font License 1.1**, published as seven Unicode-range WOFF2 shards (Latin, Greek, Cyrillic, digits, combining marks, and common symbols).
- One-time merge and WOFF2 → TTF conversion with `fontTools`, vendored as `data/fonts/Excalifont-Regular.ttf`, embedded with `include_bytes!` in `src/tools/annotation/font.rs` (`OnceLock<ttf_parser::Face<'static>>`). The vendored TTF must contain all seven shards; a single shard is not a valid replacement. No fontconfig, no gresource, hermetic tests.
- Licence compliance: ship `data/fonts/OFL-Excalifont.txt` (full OFL-1.1 text + copyright notice), install it via `data/meson.build` alongside the app licence, credit the font in About → Legal. Before vendoring, inspect the font's `name` table for an OFL Reserved Font Name; if one is declared, the converted TTF's family name is changed to "Diorama Hand" (OFL §3 treats format conversion as a Modified Version) while the credits keep the original attribution. Record the outcome in the spec.
- The 2024 source font's `name` table declares no Reserved Font Name, so the converted TTF keeps the Excalifont family name.

## 8. Settings and shortcuts

GSettings (`data/io.github.mendrik.Diorama.gschema.xml`, `src/settings.rs`):

| Key | Type | Default | Change |
|---|---|---|---|
| `annotation-text-size` | i | 24 | new (6–512) |
| `pencil-size` | i | 1 | unchanged key; default explicitly 1 image pixel |
| `pencil-antialiasing`, `hard-zoom` | | | unchanged |
| `ai-model-installed` | b | | **removed** |

Accelerators (`src/application.rs`): add `win.highlight = o`, `win.arrow = a`, `win.text = t`; keep `win.pencil = p`, `win.measure = m`; **remove `win.delete-file = Delete`** (handled per §2.7). Update the shortcuts dialog (Editing group) and the `edit_menu` (Pencil, Highlight, Arrow, Measure, Text, Select & Copy).

## 9. Accessibility and i18n

- Every palette button has an accessible name and tooltip; tool changes update the existing accessible tool label; selected annotation type is announced ("Arrow selected").
- Full keyboard operation per §5.4; selection outline and handles use the black/white contrast pattern (not colour alone).
- New user-visible strings use `gettext`; add `src/window/annotation.rs`, `src/tools/annotation/*.rs` and `src/settings.rs` to `po/POTFILES.in`; regenerate `po/diorama.pot`. Measurement labels use the ASCII `px` suffix inside the image (rendered by the built-in bitmap face; not translated because its deliberately minimal glyph set contains only digits, space, `p`, and `x`).

## 10. Performance

- Preview rasterisation is bbox-limited; target ≤ 8 ms on the main thread for objects up to 4 Mpx bbox, otherwise render on the worker with the newest-request-wins pattern used by `render_candidate`.
- `render_excluding` reuses the raster cache (annotation entries never invalidate raster prefixes).
- Hit-testing is O(objects) with flattened polylines cached per object.
- No extra redraws on scroll at integer render scales (§3).

## 11. Dependencies, packaging, build

| Change | Detail |
|---|---|
| Add `tiny-skia = { version = "0.11", default-features = false, features = ["std", "simd"] }` | BSD-3; pure Rust; shapes, strokes, AA |
| Add `ttf-parser = "0.25"` | MIT/Apache-2.0; glyph outlines, advances, `kern` |
| Remove `rlx-sam2`, `hf-hub`, `[patch.crates-io]` | and `vendor/rlx-sam2`, `vendor/rlx-cpu` |
| Regenerate `Cargo.lock`, `build-aux/cargo-sources.json` | offline Flatpak build |
| Flatpak manifest | drop `onnxruntime`, `openblas`, their env vars and `only-arches` |
| `data/meson.build` | install new icons, `OFL-Excalifont.txt` |
| Lints | `unsafe_code = "forbid"` unaffected (crate-local); clippy `-D warnings` must stay clean |
| Version bump | 0.3.0 across `Cargo.toml`, `meson.build`, metainfo (release process) |

Module layout:

| Path | Content |
|---|---|
| `src/document/annotation.rs` (new) | types, `AnnotationEdit`, `fold_annotations`, per-operation transforms |
| `src/document/{operation,history,model,mod}.rs` | `Operation::Annotate`, `replace_last`, render skip/composite, `render_excluding`, `amend_annotation`, id counter, re-exports |
| `src/tools/annotation/{mod,geometry,pencil,highlight,arrow,measure,text,font,hit,edit,render}.rs` (new) | pure geometry, Pencil-node raster adaptation, Bézier flattening/arc length, sloppy ellipse, arrowhead, pairing, glyph layout, embedded font, hit-testing, drag application, tiny-skia compositor |
| `src/canvas/annotation_overlay.rs` (new) + `src/canvas/mod.rs` | preview/selection slots, `image_point_at`, `image_scale`, device-space layout (§3), `snap_to_device` |
| `src/window/tool.rs` (new) | `Tool` enum, `set_tool`, palette visibility, size-spin rebinding, Escape chain |
| `src/window/annotation.rs` (new) | `AnnotationDrag`, gestures, image-anchored inline text editor, Delete/nudge handling, `refresh_annotations()` after apply/undo/redo/load |
| `src/window/mod.rs`, `src/window/zoom.rs` | remove segmentation and transient measure code, wire the above, `Surface::scale()` |
| `data/fonts/`, `data/icons/…/actions/` | Excalifont TTF + OFL, four symbolic icons |

## 12. Test plan and acceptance criteria

Headless unit tests (no display), following the existing geometry-helper style:
- Fold: crop translation; CW∘CCW = id; 4×CW = id; flip² = id; axis swap on rotate; text footprint preserved under flips; `angle + 90°` under CW; Pencil geometry and Arrow widths × `sqrt(sx·sy)` while Highlight/Measure remain one native image pixel; Set/Delete semantics; Palette ignored; out-of-image objects survive and return on undo.
- Model: `Annotate` entries create no cache prefix; `[Rotate, Annotate]` reuses the raster cache; pixels differ only inside the bbox; undo of `Set` restores geometry; dirty/clean round trip; `amend_annotation` coalesces same id and refuses with a redo branch; `render_excluding` omits exactly one object.
- Highlight: determinism per `(rect, seed)`; translation invariance; different seeds differ; exactly two visibly separated revolutions whose radial order reverses at least twice; upper-left tails remain visually related; width band boundaries at 1023/1024, 2047/2048, and 3071/3072; stored width ignored and rendered coverage increases with the effective image-size width.
- Arrow: straight when control = midpoint; head tangent; control re-derivation keeps chord-relative position.
- Measure: overlap/non-overlap pairing; three-line adjacency with occluder; disjoint middle line does not block; label text `"128px"`; fixed 7-image-pixel bitmap labels ignore `annotation-text-size`; lowercase suffix glyphs use x-height and a `p` descender; label origins clamp inside the image, including images smaller than the label; endpoints integral; line/ticks/labels contain no partially covered anti-aliased pixels; width stays one image pixel after scaling; the hover crosshair is the top child of a `Difference` blend over the image.
- Text: required ASCII glyphs resolve to non-empty outlines; a representative Text annotation produces non-transparent pixels; advance monotone in length; chord covered when `bend = 0`; rotation keeps midpoint; end-handle scaling keeps aspect; flip mapping keeps glyph readability; ignored display test compares `gtk::Text::compute_cursor_extents` at every prefix of a kerning-sensitive string containing consecutive spaces against the renderer's advance at normal and low zoom (≤ 1 device px error).
- Pencil: freehand/line/rectangle/circle commits are `Annotate(Create)` entries; each renders identically to the established Pencil rasterizer; single-point nodes remain selectable; move/resize preserves pressure; undo/redo restores nodes.
- Hit: handle beats body; topmost wins; Pencil endpoint/bounds handles; ring only outside the handle; tolerance/zoom conversion; cursor map.
- Tool model: exactly one tool active; palette visibility table; eyedropper returns to `return_tool`; lens independent; Escape order.
- Application: no `"Delete"` accelerator; `o`/`a`/`t`/`m`/`p` present; `win.select-object` absent; application accelerators are empty only for the lifetime of an inline text edit and restored on commit/cancel.
- Hard zoom: `device_rect_origin_is_integral_in_surface_space` over origins × `rs ∈ {1, 1.25, 1.5, 2}` × pixel scales; `align_device_offset` idempotent; `aligned_render_pixel_scale(0.8, 1.25) == Some(1)`, `(1.5, 2.0) == Some(3)`; updated `one_hundred_percent_maps_every_source_pixel_to_equal_render_blocks`; ignored display test walking render nodes (`Transform(1/rs)` → `TextureScale` with device bounds).

Manual / graphical verification:
1. Toolbar: Pencil on → click eyedropper → palette stays; pick → Pencil restored; lens toggles without hiding; Crop hides the palette.
2. Start Diorama → swatch is `#FF0000`.
3. Open a white image under light and dark themes → all floating toolbars readable (dark scrim, white icons).
4. Hard zoom 800 % on a 1-px checkerboard (`magick -size 32x32 pattern:gray50 check.png`) under nested Weston at scale 2 and nested mutter at 1.25/1.5: after middle-drag by a fraction, anchored wheel zoom, pinch, minimap drag, kinetic scroll and compare lock, every run along a row is exactly `round(8·scale)` device px with no intermediate greys (script the run-length check on the screenshot).
5. Create each annotation type, including Pencil freehand, line, rectangle and circle; move, resize, rotate (text), bend (arrow/text); undo/redo each step; Delete and BackSpace; nudge with arrows; crop/rotate/flip/scale the image with annotations present and confirm they follow; export PNG and compare against the on-screen result pixel for pixel; reopen the export.
6. Two overlapping horizontal measurement lines show one gap marker; adding a third in between re-pairs neighbours; moving one line re-measures live.
7. Build the Flatpak offline (`cargo-sources.json`), on x86_64 and aarch64; confirm no onnxruntime/openblas modules and no network access.

Release acceptance (adds to spec.md §25): (a) the palette is never hidden by colour, eyedropper, lens or size changes; (b) startup colour is red; (c) hard-zoom checkerboard test passes at scales 1, 1.25, 1.5, 2; (d) every annotation type is creatable, editable, deletable and undoable with mouse and keyboard; (e) exported pixels equal the on-screen composite; (f) no SAM 2 code, model download or x86_64 restriction remains; (g) `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test` pass.

## 13. Delivery phases (implementation follow-up, not part of this task)

1. **Cleanup** — remove segmentation and the transient measure tool; Delete-key scoping; `.toolbar.osd` styling; default red; version bump. (Independent, low risk.)
2. **Tool model** — `Tool` enum, `set_tool`, unified palette with size-spin rebinding, lens/eyedropper semantics, Escape chain, tests.
3. **Hard zoom** — device-space layout, `Surface::scale()`, zoom guards, tests, checkerboard verification.
4. **Annotation core** — document types, `Operation::Annotate`, fold + transforms, tiny-skia compositor, font embedding, headless tests.
5. **Interaction** — hit-testing, drag machine, preview overlay, selection handles, keyboard, Highlight + Arrow.
6. **Measure + Text** — pairing/labels, text editor popover, rotation ring, curved text.
7. **Polish & release** — i18n, shortcuts dialog, README, Flatpak sources, metainfo release notes.

## 14. Assumptions and open items

- Excalifont Reserved Font Name status is verified during Phase 4 (§7); the renaming path is specified so either outcome is covered.
- `gdk::Surface::scale()` is available with the crate's `gnome_49` feature (GTK ≥ 4.12); GTK 4.20+ renders at the fractional surface scale on Wayland — verified against GTK sources by the design review, to be re-confirmed on the Flatpak runtime during Phase 3.
- Compare mode keeps its existing pencil gestures; other annotation tools are unavailable there (non-goal).
- Animated images: annotation tools follow the existing `editable` gating (single frame).
