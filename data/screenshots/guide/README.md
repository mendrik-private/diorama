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

## Bicubic and Game Asset comparison captures

The nine `game-asset-vs-bicubic-*.png` captures were produced on 2026-09-30
from Diorama 0.3.25 based on source revision `2052feb`, with the accompanying
capture-harness changes in
`src/window/scale_comparison_screenshots.rs` and
`build-aux/capture-scale-comparison-screenshots.sh`.

The comparison harness read these originals without modifying them:

| Original | SHA-256 |
| --- | --- |
| `06-cave-spider.jpg` | `d469b480485cd68ea2cab69e4c44302374b703e4b4e6574b9b948690e8aebf2b` |
| `10-goblin.jpg` | `e3bba2066e49e4445212e7b309f24429399abded594eedc6a283cac7aa432dd4` |
| `46-dragon.jpg` | `c7297d6754dd212f5095f13e8aa2e119d97a7303b9f7bddf5844d77ae769e664` |

The originals came from
`/home/mendrik/desk/mendrik/mule/assets/monsters`.

Each JPEG is a 1024 × 1024 opaque image with its original gray background.
For each 128 × 128, 180 × 180, and 256 × 256 pair, production Bicubic
(Catmull–Rom) and Game Asset scaling receive the same unchanged decoded RGBA
pixels. Game Asset uses the default Strength 40 setting and the installed
FLUX.2 [klein] and BiRefNet runtimes; failure is explicit and there is no test
layer fallback. Bicubic retains the JPEG background, while Game Asset uses the
BiRefNet cutout of its generated fill. The comparison view shows transparency
over Diorama's neutral gray canvas background, so the two backgrounds should
not be interpreted as identical alpha.

Every PNG is an unedited 1280 × 800 capture of Diorama's real comparison view,
with Bicubic on the left and Game Asset on the right. The harness asserts exact
output and loaded-image dimensions, equal pane widths, a scale-1 compositor,
and 200% hard display zoom in both panes immediately before capture. It also
checks the original hashes again after capture.

Capture used:

```sh
export XDG_RUNTIME_DIR=/tmp/diorama-scale-wayland-runtime
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
weston --backend=headless --renderer=pixman --width=1280 --height=800 --scale=1 \
  --socket=diorama-scale-capture --idle-time=0 --no-config --shell=kiosk-shell.so
GSK_RENDERER=cairo GDK_BACKEND=wayland WAYLAND_DISPLAY=diorama-scale-capture \
  build-aux/capture-scale-comparison-screenshots.sh
```

The script runs with `GSETTINGS_BACKEND=memory`, `GDK_SCALE=1`, and
`GDK_DPI_SCALE=1`. Pass the directory containing the three named originals as
its first argument or set `DIORAMA_SCALE_COMPARISON_SOURCE_ROOT` to reproduce
the captures from another checkout.
