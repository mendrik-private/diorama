# Contrast-selected halving trial

The prior scaling and ridge/contour experiments are checkpointed on
`experiment/game-asset-ridge-cleanup` at `1f62cae`. This separate trial lives on
`experiment/contrast-halving`. Unrelated annotation edits were not committed.

`src/tools/scale/halving_experiment.rs` is test-only; the application's scaling
methods and defaults are unchanged.

## Algorithm

1. Halve both dimensions while the next integer-sized image remains at least
   as large as the target on both axes.
2. For every output cell, consider the four nearest source samples around its
   centre. Select the sample with maximum summed colour/alpha distance from the
   other three. Distance is Euclidean in premultiplied RGBA; the score is also
   multiplied by the candidate's alpha. Thus dark, bright, or chromatic isolates
   can win, while hidden RGB cannot make a transparent sample prominent.
3. Copy that sample's original RGBA bytes, without averaging or quantization.
   Equal scores prefer the normal NN sample, then a fixed neighbour order.
4. Use the application's existing Catmull–Rom bicubic scaler for the final
   dimensions. Skip that step if halving reaches the target exactly.

800×800 → 128×128 therefore runs **800 → 400 → 200 → bicubic 128**. There is no
100px intermediate and no subsequent enlargement. Odd sizes use integer halves
and centre-aligned neighbour pairs, including the final row/column; this is not
an area filter. For non-uniform targets, halving stops as soon as either axis
would undershoot. A one-pixel-wide image goes directly to the bicubic finish.

“Stand out” here means alpha-weighted local contrast within the four candidates,
not always the brightest or darkest pixel. It can promote noise or thicken thin
features. The final bicubic step intentionally blends colours and alpha, so the
final result does **not** carry the older source-only-colour guarantee. There is
no ridge detection, contour cleanup, wavelet analysis, or AI processing.

## Reproduce and inspect

```sh
CARGO_PROFILE_DEV_OPT_LEVEL=2 cargo test --lib halving_experiment
CARGO_PROFILE_DEV_OPT_LEVEL=2 \
  DIORAMA_GAME_ASSET_INPUT=/home/mendrik/Downloads/elf2-se.png \
  cargo test --lib elf_contrast_halving -- --ignored --nocapture
```

The fixture test exports to a fresh temporary directory:

- `400-standout.png` and `200-standout.png`: copied-source intermediate samples.
- `128-standout-bicubic.png` and `160-standout-bicubic.png`: final RGBA trials.
- `128-comparison-3x.png` and `160-comparison-3x.png`: NN / direct bicubic /
  contrast-selected halves plus bicubic.
- `128-heads-8x.png`: the same three choices around the hood and hair.

Tests check the stage sequence, exact power-of-two targets, odd/rectangular
sizes, NN tie-breaking, isolate selection, original RGBA copying during halves,
byte-equivalence to the existing final bicubic implementation, invalid targets,
identity and cancellation. Logged trial timings use an optimized development
build, not a controlled release benchmark. Artistic quality remains a visual
comparison rather than a test assertion.
