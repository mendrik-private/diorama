# Game Asset AA validation

Game Asset scaling keeps its 0–100% AA strength control with a 50% default. Its
contour antialiasing now uses the pen tool's softened edge coverage curve. At 0%
the additional contour edge coverage is hard; at 100% the full pen-style softened
coverage is composited. Intermediate values blend cached 0% and 100% endpoints.

The control is a numeric spinner, visible only for Game Asset scaling. It is tested
with the paired Opacity spinner for its default and range, preview updates,
persistence, committed scale operations, cancellation of obsolete previews,
keyboard focus, and layout. AA changes reuse the endpoint pair for the current
target size and opacity; changing either cache key rebuilds it.

Contour opacity is deliberately separate from AA. It defaults to 100%; 0% adds no
contour paint and 100% overlays the full detected contour using its original donor
color. Opacity affects contour compositing coverage and must not darken or recolor
the fill.

To run the graphical control check, compile the schema into a temporary directory,
set `GSETTINGS_SCHEMA_DIR` and `GSETTINGS_BACKEND=memory`, then run:

```sh
cargo test --lib game_asset_aa_control_updates_preview_and_committed_operation -- --ignored --test-threads=1
```
