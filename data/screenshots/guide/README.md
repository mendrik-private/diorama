# Guide screenshot provenance

These 1280 × 800 PNGs were captured on 2026-09-29 from Diorama 0.3.25 at
source revision `ab3303d`, using the guide-capture harness in
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
weston --backend=headless --renderer=pixman --width=1280 --height=800 --scale=1 \
  --socket=diorama-guide-capture-2 --idle-time=0 --no-config --shell=kiosk-shell.so
GSK_RENDERER=cairo GDK_BACKEND=wayland WAYLAND_DISPLAY=diorama-guide-capture-2 \
  XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
  build-aux/capture-guide-screenshots.sh
```

The script runs with `GSETTINGS_BACKEND=memory`, `GDK_SCALE=1`,
`GDK_DPI_SCALE=1`, and a guide-test `gtk-xft-dpi` of 96 DPI. It validates the
original hashes after capture and checks that every generated PNG is nonempty.

The harness makes three disposable derivatives in its system temporary
directory: a 64-pixel transparent border around the wyvern for the crop
confirmation, and 256-pixel Nearest and Lanczos versions for the 200%-zoom
method comparison. They are generated through Diorama's scaler and are never
written beside, or substituted for, the originals. The Game Asset result and
line-art captures use the installed local FLUX.2 [klein] and BiRefNet runtime;
the test fails if that real preview does not complete. The background-removal
capture shows the actual extracted selection moved over its repaired source
background, with its editable image handles visible. It does not represent
full-canvas background removal.
