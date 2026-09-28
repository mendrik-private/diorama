# Vendored asset-scaler

This is the self-contained library dependency used by Diorama's Game Asset
resize tool. It starts from `asset-scaler` commit
`cff93b5e626270be4a3cf923f01109ee9eab452a`.

Only source contour extraction is selectively taken from commit `b6bc785`:
ridge deduplication, source sliver and spur cleanup, ordered tracing, spline
fitting, and same-owner source colour donors. De-inking, crowding, tiny-sprite
policies, thick-stroke work, experiments, services, and model runtimes are not
included.

The original project license remains [GPL-3.0-only](LICENSE).

`LineArtSession` reduces an extracted foreground with application-supplied,
source-aligned line art: the line art is reduced with bicubic resampling,
sharpened with an unsharp mask (radius 1, amount 0.5–3.0 from `Strength`
0–100) and multiplied over the Lanczos foreground fill in 8-bit sRGB, keeping
the fill's alpha. Diorama's Game Asset mode uses it; see Diorama's
`docs/game-asset-scaling.md`. `Session` and the `resize*` functions keep the
traced-contour reduction.
