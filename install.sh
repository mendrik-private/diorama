#!/bin/sh
# Build this checkout and install it into the current user's Flatpak installation.
set -eu

app_id=io.github.mendrik_private.Diorama
runtime_remote=flathub
runtime_url=https://dl.flathub.org/repo/flathub.flatpakrepo
project_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest=build-aux/io.github.mendrik_private.Diorama.Devel.json
build_dir=.flatpak-builder/local

cd "$project_root"

for command in flatpak flatpak-builder; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'Missing required command: %s\n' "$command" >&2
        exit 1
    fi
done

if ! flatpak remotes --user --columns=name | grep -Fx "$runtime_remote" >/dev/null; then
    flatpak remote-add --user --if-not-exists "$runtime_remote" "$runtime_url"
fi

flatpak-builder \
    --user \
    --install \
    --install-deps-from="$runtime_remote" \
    --force-clean \
    "$build_dir" \
    "$manifest"

printf '\nInstalled current checkout as %s. Run it with:\n  flatpak run %s\n' \
    "$app_id" "$app_id"
