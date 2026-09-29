# Guide screenshot provenance

These 1024 × 640 PNGs were captured on 2026-09-29 from Diorama 0.3.25 at
source revision `92331c8`, using the uncommitted guide-capture harness in
`src/window/screenshots.rs` and `build-aux/capture-guide-screenshots.sh`.

The real app renderer used temporary copies of these read-only originals:

| Original | SHA-256 |
| --- | --- |
| `50-wyvern.png` | `19b02e2e569ae23b4592913d818c3997dd3018d48e6f21ebdb299b2349101e5c` |
| `27-stone-gargoyle.png` | `a4968c5675c9a37445b62027fa23252dd07c0401244fd0e14ea9de3653d37567` |

The originals came from
`/home/mendrik/Pictures/game-assets/characters-midwalk-v2/horror-variants/dnd-fantasy`.
Use `DIORAMA_SCREENSHOT_SAMPLE_ROOT` or the script's first positional argument
to use another directory with the same filenames.

Capture used:

```sh
weston --backend=headless-backend.so --socket=diorama-guide-capture --idle-time=0
WAYLAND_DISPLAY=diorama-guide-capture XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
  build-aux/capture-guide-screenshots.sh
```

The script runs with `GSETTINGS_BACKEND=memory`, validates the original hashes
after capture, and checks that every generated PNG is nonempty.
