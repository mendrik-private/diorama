#!/usr/bin/env bash
# Rebuild the screenshot-led user guide from real Diorama widgets and renderers.
# Requires a Wayland compositor (weston --backend=headless-backend.so is suitable).
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
sample_root=${DIORAMA_SCREENSHOT_SAMPLE_ROOT:-${1:-/home/mendrik/Pictures/game-assets/characters-midwalk-v2/horror-variants/dnd-fantasy}}
primary_source="$sample_root/50-wyvern.png"
comparison_source="$sample_root/27-stone-gargoyle.png"
output_dir="$repo_root/data/screenshots/guide"

test -n "${WAYLAND_DISPLAY:-}" || {
  printf '%s\n' 'Set WAYLAND_DISPLAY (for example by starting a headless Weston compositor).' >&2
  exit 1
}
test -f "$primary_source"
test -f "$comparison_source"

before=$(sha256sum "$primary_source" "$comparison_source")
mkdir -p "$output_dir"
DIORAMA_SCREENSHOT_SOURCE="$primary_source" \
DIORAMA_SCREENSHOT_COMPARE_SOURCE="$comparison_source" \
DIORAMA_SCREENSHOT_DIR="$output_dir" \
GSETTINGS_BACKEND=memory \
GDK_SCALE=1 \
GDK_DPI_SCALE=1 \
cargo test --lib window::screenshots::capture_guide_screenshots -- --ignored --exact --nocapture
test "$before" = "$(sha256sum "$primary_source" "$comparison_source")"
for screenshot in \
  overview.png inspect-pixel-lens.png compare-lenses.png edit-selection.png \
  scale-options.png scaling-method-comparison.png \
  game-asset-options.png game-asset-result.png game-asset-line-art.png \
  flip-and-rotate.png mesh-warp-grid.png crop-detected-content.png \
  canvas-resize.png preferences.png background-removal-cutout.png \
  annotate-character.png export-options.png; do
  test -s "$output_dir/$screenshot"
done
