#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
book_root="$repo_root/docs/user-guide"
screenshot_root="$repo_root/data/screenshots/guide"
output_root="$repo_root/target/docs-site"
required_screenshots='overview.png inspect-pixel-lens.png compare-lenses.png edit-selection.png annotate-character.png export-options.png'

command -v mdbook >/dev/null 2>&1 || {
    echo "mdbook is required; install it with: cargo install mdbook --version 0.5.4 --locked" >&2
    exit 1
}

test "$(mdbook --version)" = "mdbook v0.5.4" || {
    echo "mdbook 0.5.4 is required for reproducible documentation builds" >&2
    exit 1
}

for screenshot in $required_screenshots; do
    test -f "$screenshot_root/$screenshot" || {
        echo "Required guide screenshot is missing: data/screenshots/guide/$screenshot" >&2
        exit 1
    }
done

staging_root=$(mktemp -d "${TMPDIR:-/tmp}/diorama-docs.XXXXXX")
cleanup() { rm -rf "$staging_root"; }
trap cleanup EXIT HUP INT TERM

cp -R "$book_root/." "$staging_root/"
mkdir -p "$staging_root/src/assets/screenshots"
cp "$screenshot_root"/*.png "$staging_root/src/assets/screenshots/"

rm -rf "$output_root"
mdbook build "$staging_root" --dest-dir "$output_root"
ruby "$repo_root/build-aux/check-doc-site.rb" "$output_root"
