# Vendored asset-scaler

This is the self-contained library dependency used by Diorama's Game Asset
resize tool, a copy of the library crate of
[asset-scaler](https://github.com/mendrik-private/asset-scaler). The crate is
developed here: upstream commit `cfa1801` took its `src/` and `tests/` from
this copy as released in Diorama 0.3.25, and later changes are copied back the
same way. The upstream workspace's examples, docs, HTTP server and background
removal are not vendored.

The original project license remains [GPL-3.0-only](LICENSE).

`LineArtLayers` composes application-supplied, target-sized layers: line
art, a fill, the fill's alpha and the original's colours as a reference.
It first restores the fill's local colour towards the reference by an
edge-aware correction measured away from ink and outside the fill's own
background. Line art sharpened with an unsharp mask (radius 1, amount
0.5–3.0 from `Strength` 0–100) then becomes black ink (none at or above
gray 230, opaque at black) that is composited source-over the fill and its
alpha in 8-bit sRGB, without resampling any layer, so ink outside the alpha
extends the silhouette.
Diorama's Game Asset mode uses it; see Diorama's
`docs/game-asset-scaling.md`. `Session` and the `resize*` functions keep the
traced-contour reduction.
