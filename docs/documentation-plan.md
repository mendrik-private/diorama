# Documentation and screenshot plan

## Status

`build-aux/build-docs.sh` builds the GitHub Pages guide from
`docs/user-guide/`. The Pages workflow validates it for pull requests to
`main` and `develop` and for pushes to those branches. A direct push to `main`
deploys the static site. Releases use the same read-only documentation build
workflow before publication.

The guide's six screenshot filenames are part of the build contract. Their
source directory is `data/screenshots/guide/`; the documentation build copies
them into an isolated temporary book and never writes generated assets into
the checkout. Keep the six required captures versioned with the guide; verify
deployment through the workflow and release checklist after merging.

## Reader structure

The published guide uses task-oriented pages: overview; opening and browsing;
pixel and metadata inspection; comparison; reversible edits and annotation;
save and export; and shortcuts and common questions. Existing engineering
documents in `docs/` remain separate from the user guide.

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

The guide build requires POSIX `sh`, mdBook `0.5.4`, and Ruby with its standard
library. Screenshot capture additionally requires Bash, Cargo/Rust, a Wayland
display, and the local sample images. Neither operation requires Python.

## Release maintenance

Before finalizing a release, update the guide whenever a user-visible feature,
shortcut, label, workflow, file-format claim, or screenshot has changed. Run
`sh build-aux/build-docs.sh`; it fails for a missing screenshot, link, or
image. Inspect the rendered guide at the repository Pages subpath
`/diorama/`, then include the documentation result in the release checklist.

When a capture changes, replace its PNG with a real app capture, keep its
documented filename unless the Markdown is updated in the same change, and
check the full-size link and alt text. Do not stage mockups or edited UI
screenshots as product evidence.
