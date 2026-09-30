#!/usr/bin/env bash
# Capture production Bicubic/Game Asset comparisons in Diorama's compare view.
# Requires the local FLUX/BiRefNet models and a 1280x800, 1x Wayland display.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
source_root=${DIORAMA_SCALE_COMPARISON_SOURCE_ROOT:-${1:-/home/mendrik/desk/mendrik/mule/assets/monsters}}
output_dir="$repo_root/data/screenshots/guide"

test -n "${WAYLAND_DISPLAY:-}" || {
  printf '%s\n' 'Set WAYLAND_DISPLAY to a 1280x800, scale-1 Wayland compositor.' >&2
  exit 1
}

sources=(06-cave-spider.jpg 10-goblin.jpg 46-dragon.jpg)
expected_hashes=(
  d469b480485cd68ea2cab69e4c44302374b703e4b4e6574b9b948690e8aebf2b
  e3bba2066e49e4445212e7b309f24429399abded594eedc6a283cac7aa432dd4
  c7297d6754dd212f5095f13e8aa2e119d97a7303b9f7bddf5844d77ae769e664
)
before=()
for index in "${!sources[@]}"; do
  source_path="$source_root/${sources[$index]}"
  test -f "$source_path"
  actual=$(sha256sum "$source_path" | cut -d ' ' -f 1)
  test "$actual" = "${expected_hashes[$index]}" || {
    printf 'Unexpected source hash for %s\n' "$source_path" >&2
    exit 1
  }
  before+=("$actual")
done

mkdir -p "$output_dir"
DIORAMA_SCALE_COMPARISON_SOURCE_ROOT="$source_root" \
DIORAMA_SCREENSHOT_DIR="$output_dir" \
GSETTINGS_BACKEND=memory \
GDK_SCALE=1 \
GDK_DPI_SCALE=1 \
cargo test --lib window::scale_comparison_screenshots::capture_monster_scale_comparisons \
  -- --ignored --exact --nocapture

for index in "${!sources[@]}"; do
  source_path="$source_root/${sources[$index]}"
  test "${before[$index]}" = "$(sha256sum "$source_path" | cut -d ' ' -f 1)"
done
for monster in cave-spider goblin dragon; do
  for size in 128 180 256; do
    test -s "$output_dir/game-asset-vs-bicubic-$monster-$size.png"
  done
done
