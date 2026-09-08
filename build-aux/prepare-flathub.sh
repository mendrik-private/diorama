#!/bin/sh
# Generate packaging from a published upstream revision; never submits a PR.
set -eu
if [ "$#" -ne 2 ]; then
    printf 'Usage: sh build-aux/prepare-flathub.sh REVISION NEW_OUTPUT_DIRECTORY\n' >&2
    exit 2
fi
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"
revision=$(git rev-parse --verify "$1^{commit}")
output=$2
app_id=io.github.mendrik_private.Diorama
manifest=build-aux/$app_id.Devel.json
# Old releases predate the ID migration and cannot build this package.
if ! git cat-file -e "$revision:$manifest" 2>/dev/null; then
    printf 'Revision %s predates the app-ID migration. Choose a release containing this preparation.\n' "$1" >&2
    exit 2
fi
if [ -e "$output" ]; then
    printf 'Output must be a new directory: %s\n' "$output" >&2
    exit 2
fi
mkdir -p "$output"
git show "$revision:$manifest" | jq --arg revision "$revision" '
    .modules[1].sources[0] = {
        type: "git",
        url: "https://github.com/mendrik-private/diorama.git",
        commit: $revision
    }
    | .modules[0].sources |= map(. as $source | {
        type: "file",
        url: ("https://raw.githubusercontent.com/mendrik-private/diorama/" + $revision + "/build-aux/" + $source.path)
    })
' > "$output/$app_id.json"
for helper in diorama-glycin-heif diorama-glycin-heif.conf; do
    checksum=$(git show "$revision:build-aux/$helper" | sha256sum | cut -d ' ' -f 1)
    jq --arg helper "$helper" --arg checksum "$checksum" '
        .modules[0].sources |= map(if (.url | endswith("/" + $helper)) then . + {sha256: $checksum} else . end)
    ' "$output/$app_id.json" > "$output/manifest.tmp"
    mv "$output/manifest.tmp" "$output/$app_id.json"
done
git show "$revision:build-aux/cargo-sources.json" > "$output/cargo-sources.json"
printf 'Generated %s pinned to %s. Publish that upstream commit before building.\n' "$output/$app_id.json" "$revision"
