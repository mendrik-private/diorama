# Diorama

Diorama is a fast, native GNOME image viewer built with Rust, GTK 4, and
libadwaita. It combines everyday image browsing with focused, non-destructive
editing and comparison tools.

> [!NOTE]
> Diorama is under active development. File-format support and editing behavior
> may change before the first stable release.

## Features

- Smooth and pixel-perfect zoom modes, fit-to-window, panning, and a minimap.
- Folder navigation with natural filename ordering and neighboring-image
  prefetching.
- Side-by-side image comparison with synchronized navigation and a detail lens.
- Non-destructive crop, rotate, flip, scale, palette, pencil, and object-selection
  operations with undo and redo.
- Animated-image playback.
- Atomic PNG and JPEG export with configurable metadata preservation.
- GNOME-native keyboard shortcuts, preferences, and adaptive widgets.

## Install a release

Download `Diorama.flatpak` from the repository's latest GitHub Release, then run:

```sh
flatpak install --user ./Diorama.flatpak
flatpak run io.github.mendrik.Diorama
```

The bundle uses the GNOME 50 runtime. Flatpak will offer to install the runtime
from Flathub if it is not already available.

## Build from source

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

Diorama is available under the [MIT License](LICENSE).
