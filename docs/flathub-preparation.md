# Flathub preparation

The selected application ID is `io.github.mendrik_private.Diorama`, matching
<https://github.com/mendrik-private/diorama>. The old ID remains installed
independently; this change does not migrate its Flatpak data or preferences.

## Current state

- The upstream app, desktop entry, schema, icon, and CI use the new ID.
- Static filesystem access is limited to Pictures and removable media.
  Files elsewhere can be opened with the portal file chooser. Opening a single
  file through a portal does not grant its parent directory: neighboring-image
  browsing outside the permitted directories may be unavailable.
- The HEIF wrapper discovers the runtime's architecture-specific codec path.
  ARM64 still needs an actual build and runtime test.
- The application and bundled font licenses are installed under
  `/app/share/licenses/io.github.mendrik_private.Diorama`.
- Screenshots are real GTK window captures made under Weston, using the supplied
  shrine image without altering the source. They live in `data/screenshots/`.
- MetaInfo references screenshots under the planned `v0.3.5` tag. Those links
  intentionally cannot validate online until that upstream release exists.

The README retains the requested pre-stable status. Flathub requires stable
software and does not accept new beta-repository submissions. This is an open
eligibility issue, not something passing the linter resolves.

Validation on 2026-09-08: formatting, Clippy, 174 non-graphical tests, and the
Weston screenshot capture passed. The renamed x86_64 Flatpak built and exported
successfully. The manifest (with the required application-ID filename) and a
generator fixture passed Flathub manifest lint. Offline MetaInfo validation
passed. Repository lint reports only `appstream-screenshots-not-mirrored-in-ostree`
and `appstream-missing-screenshots`, because the planned release URLs are not yet
published. Repeat online validation and repository lint after publication.

## Produce the submission files after an upstream release

Publish an upstream revision containing this preparation and the screenshots.
For the planned next release, update Cargo and Meson versions and AppStream
release notes to `0.3.5` before tagging it. If another tag is chosen, update the
immutable screenshot URLs to match.

From this repository, choose an output directory that does not yet exist:

```sh
sh build-aux/prepare-flathub.sh v0.3.5 /tmp/diorama-submission
```

The generator reads files from the supplied revision, pins the application to
its full commit hash, references helper files by immutable URLs and SHA-256,
and includes the generated Cargo sources manifest. It never creates submission
commits, pushes, or opens a pull request. Do not include application source,
screenshots, or prebuilt Flatpak bundles in the Flathub submission repository.

Validate and build from the generated directory:

```sh
cd /tmp/diorama-submission
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest io.github.mendrik_private.Diorama.json
flatpak run --command=flathub-build org.flatpak.Builder --install io.github.mendrik_private.Diorama.json
flatpak run io.github.mendrik_private.Diorama
flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo repo
```

Check image formats, navigation, saving, comparison, and printing in the sandbox,
including files opened outside Pictures and removable media.

## Reproduce screenshots

Start a separate Weston compositor:

```sh
weston --backend=headless --renderer=pixman --shell=kiosk-shell.so \
  --width=1280 --height=800 --socket=diorama-shots --idle-time=0 --no-config
```

In another terminal, from the source checkout:

```sh
WAYLAND_DISPLAY=diorama-shots GDK_BACKEND=wayland GSK_RENDERER=cairo \
GSETTINGS_BACKEND=memory \
DIORAMA_SCREENSHOT_SOURCE=/path/to/shrine.png \
DIORAMA_SCREENSHOT_DIR="$PWD/data/screenshots" \
dbus-run-session -- cargo test capture_store_screenshots -- --ignored --test-threads=1
```

The opt-in capture test opens the actual window, fits the image, and captures
the browsing and region-selection states. It does not save edits to the source.
Without the source environment variable, it does nothing.

## Human submission

Follow <https://docs.flathub.org/docs/for-app-authors/submission> using the
`new-pr` branch. Write submission commit messages, the PR, and review replies
yourself. Disclose the affected parts and approximate extent of included
AI-generated material, including this preparation. See
<https://docs.flathub.org/docs/for-app-authors/requirements#generative-ai-policy>.
