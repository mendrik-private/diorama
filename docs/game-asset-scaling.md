# game-asset scaling

Version 0.1 — down-sampling algorithm specification

## 1. Purpose

**game-asset scaling** reduces an illustrated game asset to a smaller pixel grid while preserving its silhouette and visually distinctive strokes. It combines multiple nearest-neighbour sampling offsets, perceptual colour contrast, directional wavelet evidence, and fitting of connected digital lines and curves.

Every visible output pixel selects a colour from the original image. Analysis may use continuous filtering and geometric fitting; rendering uses discrete sample selection. Thin dark contours, bright rim-lights, and coloured accents are eligible under the same selection process.

The method is a proposed engineering design. Its defaults are starting values for evaluation, and its visual quality has not yet been measured against ordinary nearest-neighbour down-sampling.

### Intended behaviour

- A bowstring sampled intermittently by different NN offsets can become a continuous one-pixel line.
- A curved cloak outline can become a connected pixel staircase that follows a simple fitted curve.
- A narrow highlight or coloured seam can survive even when it is neither dark nor present in most sampling passes.
- Broad colour regions retain ordinary NN sampling unless a supported structural edit requires a different sample.

Small output grids cannot retain every source feature. The algorithm must report discarded or unresolved features instead of claiming complete topology preservation.

## 2. Inputs and outputs

| Item | Definition |
| --- | --- |
| Source image | Decoded, straight-alpha sRGB RGBA image of size `W × H`; version 0.1 assumes 8-bit channels. RGB images receive fully opaque alpha. |
| Target size | Positive integer dimensions `w × h`, with `w ≤ W` and `h ≤ H`. |
| Geometry | Preserve the source canvas and aspect ratio by default. Non-uniform scaling requires an explicit option. |
| Options | Sampling offsets, feature scales, confidence thresholds, fitting tolerance, and search budget. |
| Result | RGBA image of exactly `w × h`. |
| Provenance | For every visible output pixel: selected source coordinate, baseline or reconstructed status, and any associated stroke ID. |
| Diagnostics | Retained, dropped, and unresolved features; unsupported candidate gaps; collisions; and detected connectivity failures. |

If both dimensions are unchanged, return the source image unchanged. That identity case bypasses the down-sampling alpha policy.

For down-sampling, the default alpha policy is binary: the output alpha is either `0` or `255`. An opaque output pixel copies its source candidate's RGB bytes exactly. Transparent output pixels use RGB `(0, 0, 0)` as a storage convention. Promoting an eligible partially transparent sample to opaque changes its alpha, so this is an exact RGB guarantee, not an exact RGBA guarantee.

Version 0.1 performs no palette quantization, dithering, sharpening, or output colour averaging. Any such operation is a separate, explicitly requested post-process and changes the exact-source-colour contract.

## 3. Coordinate convention and NN candidates

Source pixel `(i, j)` occupies `[i, i+1) × [j, j+1)` and has its centre at `(i+0.5, j+0.5)`. Output pixel `(x, y)` follows the same convention in output coordinates.

Define:

```text
sx = W / w
sy = H / h

footprint(x, y) =
    [x*sx, (x+1)*sx) × [y*sy, (y+1)*sy)
```

Use `n = 4` offsets along each axis initially. For `a, b ∈ {0, …, n−1}`:

```text
qx = (a + 0.5) / n
qy = (b + 0.5) / n

i = floor((x + qx) * sx)
j = floor((y + qy) * sy)
candidate = source[i, j]
```

These are 16 independent NN sampling phases from the original image. They are not successive resizes. Also include the ordinary centre-sampled NN candidate, using `qx = qy = 0.5`, as the baseline.

Keep each candidate's source coordinate, RGB, alpha, and feature evidence. Deduplicate identical source coordinates. Equal colours at different source positions remain distinct when their feature membership differs.

Do not average candidates or use majority voting to decide whether a stroke exists. Sparse minority samples can support a continuous feature.

**Adaptive sampling:** when an otherwise supported proposed path lacks a suitable candidate at an output pixel, inspect all source pixels whose cells have positive-area overlap with that pixel's footprint. Add qualifying samples and rescore. This is equivalent to adding suitable NN phases locally. Do not borrow a colour from a neighbouring footprint. If no supported sample exists, reject or reroute that proposal.

## 4. Source analysis

Compute feature maps once on the original image, then attach their values to candidate source coordinates. Analysing each reduced pass independently would make the evidence depend unnecessarily on the sampling phase.

### 4.1 Colour and transparency

Convert RGB to CIELAB for analysis while retaining original RGB bytes for rendering. Use colour distances that include `L`, `a`, and `b`; brightness alone is insufficient for accent-coloured strokes.

Build a source occupancy mask using `alpha ≥ 0.5`, with alpha normalized to `[0, 1]`. Record its components, boundaries, and holes. Use 8-connectivity for foreground and 4-connectivity for background, consistently throughout the implementation.

Ignore RGB values in transparent pixels. For filter support, extend visible colours into transparent areas from the nearest reliable visible sample, with deterministic coordinate tie-breaking. This extension is analysis-only. Use alpha as a separate boundary signal, and reduce confidence where too little real foreground supports the filter footprint.

Near the silhouette, evaluate the inward colour contrast and the alpha boundary tangent together. A rim-light need not have opaque material on both sides. An alpha boundary does not itself prescribe a dark outline colour.

### 4.2 Directional colour prominence

For a source position `p`, possible tangent `t`, normal `n`, and trial half-width `r`, compare a short centre strip parallel to `t` with two parallel flank strips at `p − r*n` and `p + r*n`.

Let `c0`, `c−`, and `c+` be robust Lab colour representatives of these strips. Component-wise medians are sufficient for the initial implementation. Analysis representatives need not be actual source colours.

Use this initial prominence measure:

```text
C(p, t, r) = clamp(distance(c0, segment(c−, c+)) / tau_colour, 0, 1)
```

`segment(c−, c+)` is the line segment between the two flank colours in Lab space. Distance to that segment measures whether the centre has a distinctive colour beyond a simple interpolation between its surroundings. This suppresses a smooth colour ramp and many ordinary region transitions while responding to dark, bright, and chromatic stripes.

This heuristic can miss a stroke whose colour lies between its flank colours. Wavelet evidence and silhouette analysis provide additional evidence; this test is not a hard veto.

Estimate continuation confidence `A ∈ [0, 1]` from how well neighbouring centre-strip samples agree along the tangent. A concrete starting measure is the mean of `max(0, 1 − LabDistance / tau_along)` over the valid samples in a short tangent window. Evaluate short windows so gradual shading changes along a long stroke remain possible. Reduce the influence of this term at identified corners and junctions.

Evaluate several trial widths. Keep direction and width attached to each response until line fitting; do not retain only one undirected salience value.

### 4.3 Directional wavelet evidence

Use a full-resolution oriented log-Gabor filter bank with even and odd quadrature responses. Evaluate every source location without decimating the response maps. Start with eight orientations over `[0, π)` and a small set of wavelengths beginning near three source pixels and increasing geometrically.

Choose the scale range using the reduction factor and expected stroke widths. Filter wavelength is not identical to stroke width; estimate width from the transverse colour profile. Features wider than the output pixel footprint may still supply useful structural context.

At each position, scale, and orientation, collect even responses `E` and odd responses `O` across the Lab channels. After applying consistent channel weights and filter normalization, calculate:

```text
e = sqrt(sum(channel_weight[c] * E[c]^2))
o = sqrt(sum(channel_weight[c] * O[c]^2))
amplitude = sqrt(e^2 + o^2)

line_evidence = max(e - o - noise_floor, 0) / (amplitude + epsilon)
edge_evidence = max(o - e - noise_floor, 0) / (amplitude + epsilon)
```

The even response supplies evidence for a centreline; the odd response helps identify a step boundary. Squared channel responses make the line score independent of bright versus dark polarity and allow chromatic features to contribute. Combine channels at the same position, scale, and orientation before taking maxima.

This is a proposed colour extension inspired by phase symmetry. It does not inherit all guarantees or invariance properties of an existing grayscale implementation merely by using this formula. Phase-symmetry filters can also respond to blobs, so directional continuation remains necessary.

Estimate a noise floor from robust background or coefficient statistics, with a configurable minimum. Normalize scores into `[0, 1]`. Do not normalize each colour channel independently to equal peak response: that can amplify a nearly empty, noisy channel.

For a given orientation, combine scale responses using:

```text
R = 0.75 * strongest_line_evidence
  + 0.25 * second_strongest_line_evidence
```

Use zero for the second term if only one scale is available. Agreement across scales adds confidence; it is not required. Keep the best scale and nearby alternatives. Record tangent orientation consistently; some filter APIs report the normal instead.

Wavelet filtering is auxiliary analysis. There is no inverse wavelet reconstruction of the output image.

### 4.4 Combined stroke evidence

For an initial implementation, define:

```text
S(p, t, r) = (0.65 * C(p, t, r) + 0.35 * R(p, t, r))
            * (0.5 + 0.5 * A(p, t, r))
```

This permits either colour prominence or wavelet evidence to nominate a feature. Path support subsequently decides whether it forms a useful stroke. Keep silhouette confidence and ordinary region-boundary evidence separate from `S`.

The weights are proposed defaults to tune through ablation, not measured optimum values. Wavelets supply the multiscale structural evidence; adding several redundant edge detectors is unnecessary for version 0.1.

## 5. Build the source feature graph

Apply non-maximum suppression across candidate stroke normals to localize ridges. Use high-confidence points as seeds and trace into lower-confidence points with compatible direction, width, and colour. A hysteresis scheme with seed and continuation thresholds is suitable.

Represent each traced feature as an ordered source path. Split paths at corners, endpoints, and junctions. Keep nearby parallel strokes distinct. Retain both alternatives at ambiguous junctions instead of committing early.

Each graph edge stores source positions, local tangents, width estimates, sample colours, and confidence. Each graph node records the observed connection, corner, or endpoint. Trace silhouette boundaries independently as closed contours, with component and hole IDs.

A weak or broken detector response may be bridged only where the original image supplies compatible stroke evidence. Bound the projected gap length. Do not connect endpoints solely because they are close or because a spline can pass through them. Occluded parts of a stroke are not reconstructed unless visible source evidence supports the connection.

Features too short to support reliable fitting remain eligible through baseline NN. Do not erase isolated eye highlights or texture pixels merely because they fail the path-length threshold.

## 6. Fit geometry and generate raster proposals

Map source centre coordinates into an output coordinate system whose pixel centres have integer coordinates:

```text
u = (i + 0.5) / sx - 0.5
v = (j + 0.5) / sy - 0.5
```

Transform tangents and widths consistently. Under non-uniform scaling, transform the geometry before estimating its output normal and width.

### 6.1 Fit source-supported paths

Fit a straight segment first. Use a robust geometric error to reduce the influence of stray samples. When a straight model exceeds the fitting tolerance, split at a supported corner or fit a low-complexity piecewise cubic curve with endpoint and junction constraints.

Penalize additional segments or control points and unsupported bending. Evaluate smoothness on the continuous fitted geometry. A correct pixel staircase naturally alternates horizontal, vertical, and diagonal steps; those alternations are not geometric roughness.

Keep fits within the configured distance of the projected source path. Lock supported corners and shared junctions within that tolerance. Do not straighten an intentional corner simply to lower curve complexity.

### 6.2 Explore output-grid placement

For each fit, generate a bounded set of alternatives by varying endpoint placement and translating the model locally. A starting search uses normal offsets of `−0.5`, `−0.25`, `0`, `0.25`, and `0.5` output pixels, plus eligible endpoint choices in the surrounding output cells.

Every alternative must satisfy the source-distance tolerance. Independently fitted graph edges sharing a junction must use a compatible output anchor.

Rasterize straight segments with a specified deterministic Bresenham variant that is 8-connected in every octant. Canonicalize endpoint order and document tie-breaking. For curves, flatten adaptively to a polyline with at most `0.125` output-pixel geometric error, rasterize its segments, and deduplicate shared pixels in traversal order. Detect accidental self-touching and self-intersections introduced by rasterization.

Thin strokes receive a one-pixel centreline. Where the source supports a wider stroke, use the rounded projected width, clamped to at least one pixel. Construct a binary ribbon around the fitted geometry and verify its cross-sections. Do not skeletonize every dark or bright region. Broad areas can remain baseline NN while their boundaries are fitted.

### 6.3 Associate source samples with each proposal

For every proposed output pixel, retain candidates compatible with the same source feature, its local colour, width, and direction. A bright rim-light candidate must not silently switch to an adjacent dark contour midway along the path.

Use adaptive footprint sampling where the initial offsets missed a qualifying sample. Candidate compatibility is evaluated against the corresponding local source path segment, allowing its colour to change along the stroke.

For silhouette proposals, constrain foreground occupancy and region ownership. Do not paint a new outline merely because a boundary was detected. A selected opaque pixel still needs an eligible source foreground sample in its footprint.

**Select a complete local patch, including the area around the line.** Its corridor covers the projected source stroke, its proposed ribbon, and the permitted displacement. Pixels on the chosen ribbon receive compatible stroke samples. Pixels vacated by a displaced baseline stroke must receive supported fill or transparency samples from their own footprints, respecting the source regions on each side. Preserve other identified features in that corridor. If a vacated pixel has no valid alternative, reject the placement or accept a wider supported ribbon and rescore it. Merely drawing the new line over the baseline can leave doubled outlines and is not a valid implementation of this selection step.

## 7. Score paths and choose candidate colours

Choose pixels jointly along an ordered path. For a fixed proposal, dynamic programming can select one candidate per path pixel using unary costs and transition costs. The unary terms measure:

- Local colour prominence and wavelet line confidence.
- Agreement between the source tangent and the proposed path tangent.
- Distance from the source sample to the corresponding fitted source feature.
- Difference from the baseline NN sample, giving supported edits a preference for minimal change.

Transition terms penalize jumping between unrelated source features, reversing source traversal order, and abrupt colour changes beyond those present along the source path. Use a larger transition allowance at genuine corners and junctions.

An illustrative path objective is:

```text
cost(path, labels) =
    sum(pixel_source_mismatch
        + weighted_orientation_error
        + weighted_baseline_change)
  + sum(candidate_transition_cost)
  + weighted_geometric_fit_error
  + weighted_model_complexity
  + weighted_unsupported_gap_cost
```

Normalize the terms so their weights have comparable meanings. Treat provenance failures and unsupported feature switches as invalid proposals, rather than merely expensive ones.

For geometry comparison on the same source feature, use mean error plus explicit penalties for missed source coverage and lost endpoints. Otherwise, a short path can win simply by omitting difficult parts. For prioritizing edits across different features, use total supported benefit with a bounded length contribution, so one very long contour does not automatically erase several short, important details.

For wider ribbons, keep the centreline label order and select side samples by their source region and transverse position. Boundary changes must retain the intended inside/outside relationship.

After choosing a centreline's samples, assign the remaining corridor pixels to eligible source-region candidates. Use baseline-preferring unary costs and reject assignments that violate the selected boundary or another accepted feature. Include the cost of these fill changes in the proposal score. For the initial solver, enumerate the small set of line proposals and validate each complete patch; an unrestricted optimization over all image pixels is unnecessary.

Include a no-edit proposal for every feature. An edit must improve the scored representation over baseline by a configured margin. Keeping all NN alternatives as a union would thicken lines; the solver must select a coherent subset.

## 8. Resolve conflicts and update the image

Begin with ordinary NN RGB and the binary-alpha baseline. Generate a small shortlist of proposals per feature and their candidate assignments. A proposed edit is a transactional patch with a source-feature identity, pixel assignments, required anchors, and expected adjacency.

A practical version 0.1 solver is deterministic greedy selection followed by bounded local improvement:

1. Rank proposals by improvement over the current image; break ties by source feature ID and proposal index.
2. Tentatively apply the best remaining positive-gain proposal.
3. Check candidate provenance, path connectivity, shared anchors, feature ownership, silhouette occupancy, and accidental joins in the changed region and its neighbours.
4. Accept it if valid; otherwise try the next proposal for that feature, including no edit.
5. Recompute affected gains after every accepted edit.
6. Perform up to two local improvement sweeps over conflicting features. Evaluate alternative proposals and pairwise replacements within the configured search budget.

Maintain the set of accepted feature constraints. A later edit may not break an earlier accepted path or change a shared anchor without replacing all affected proposals transactionally.

Reject a new join between unrelated source strokes or components when checking an edit. Reject a newly filled source hole that remains representable at the target resolution. Source component and feature IDs must be carried through candidate provenance; RGB similarity alone cannot establish a valid connection.

The NN baseline can already contain merged or broken features. These are diagnosed separately and count against the final quality assessment; they do not become valid merely because they were present initially. The solver should repair them when a supported proposal exists. When the target grid or search budget cannot accommodate a repair, retain the best available representation and report the failure.

If two strokes require the same output pixel with incompatible colours, evaluate alternate placements within tolerance. If none works, choose the higher-value representation and record the other feature as unresolved or dropped. Complete preservation is not always feasible.

This bounded solver is a heuristic. An integer-programming or graph-optimization solver over the same finite proposals may improve selection, but is not required by this specification.

## 9. Final rendering and guarantees

Write the selected RGB samples directly to the target grid. Apply binary alpha according to the selected candidate occupancy. If several eligible samples can supply the same accepted pixel, use the lowest-cost compatible sample with deterministic source-coordinate tie-breaking.

Do not apply a blur, antialiasing pass, global morphological closing, or final smoothing filter. Filtering used during analysis does not change this rendering rule.

### Hard output requirements

- Exact target dimensions and deterministic output for fixed options and implementation.
- Every opaque output RGB value has a recorded, eligible source sample in that output pixel's footprint.
- Binary alpha for down-sampled output under the default policy.
- Every feature marked as successfully reconstructed has its specified connected raster path, retained anchors, and no unsupported join introduced by its accepted edit.
- Unresolved collisions and detected connectivity losses appear in diagnostics.

### Quality goals to measure

- Preserve silhouette readability, significant holes, and thin structural connections.
- Retain dark, bright, and chromatic strokes without a systematic preference for darkness.
- Avoid doubled outlines, excessive line thickening, and texture promoted into false strokes.
- Keep unrelated colour regions close to the baseline image.

These are quality goals rather than universal guarantees. The method is neither a guarantee of artist-quality pixel art nor a guarantee of preserving every source component at arbitrary reduction ratios. It also does not guarantee temporal stability across animation frames; version 0.1 treats each image independently.

Display enlarged previews with nearest-neighbour sampling, ideally at integer zoom factors, so preview filtering does not conceal the actual pixel choices.

## 10. Initial configuration

All values below are experimental starting points. Record the effective configuration with every result.

| Parameter | Initial value | Purpose |
| --- | --- | --- |
| NN phase grid | `4 × 4`, plus centre sample | Generate alternatives without blending. |
| Adaptive sampling | Full footprint enumeration near candidate gaps | Recover features missed by sparse phases. |
| Alpha threshold | `0.5` | Determine eligible foreground samples. |
| Filter orientations | `8` over `[0, π)` | Supply directional evidence. |
| Filter wavelengths | Start at `3` source pixels; multiply by `2` | Cover several feature sizes. |
| Largest filter wavelength | First wavelength at or above `4 * max(sx, sy)`; cap at one quarter of the smaller source dimension | Bound context and runtime. Skip invalid scales on tiny inputs. |
| Trial stroke half-widths | `1, 2, 4, …` up to about `2 * max(sx, sy)` source pixels | Compare centre and flank colours. |
| Colour scale `tau_colour` | `12` Lab units | Normalize centre prominence. |
| Along-stroke scale `tau_along` | `12` Lab units | Normalize local colour continuation. |
| Centre-strip tangent half-length | `max(1, r)` source pixels | Obtain a short robust colour estimate at each trial width. |
| Seed / continuation thresholds | `0.55 / 0.25` | Initialize and extend source paths. |
| Fit tolerance | `0.75` output pixels maximum deviation | Bound geometric displacement. |
| Curve flattening tolerance | `0.125` output pixels | Keep rasterization close to the fitted curve. |
| Gap proposal length | At most `1` output pixel | Limit source-verified bridging. |
| Promoted path length | At least `2` output pixels | Require some line evidence; shorter details remain in baseline. |
| Proposal shortlist | At most `8` non-null proposals per feature | Bound conflict search. |
| Local improvement | `2` sweeps | Resolve a limited set of competing edits. |

Noise-floor estimation, channel weights, edit-gain margin, and objective weights must be explicit configuration fields. Begin with equal Lab channel weights and tune them using the validation cases below. No unexplained per-asset constants should be embedded in the implementation.

## 11. Reference pseudocode

```text
function game_asset_scaling(source, target_size, options):
    validate_input_and_geometry(source, target_size, options)
    if target_size == source.size:
        return source unchanged

    original_rgb = retain_source_rgb_bytes(source)
    lab, alpha, occupancy = prepare_analysis_channels(source, options)

    baseline = nearest_neighbour(source, target_size, centre_phase)
    baseline = apply_binary_alpha_policy(baseline, options)

    colour_evidence = directional_colour_prominence(lab, alpha, options)
    wavelet_evidence = full_resolution_log_gabor_analysis(lab, alpha, options)
    stroke_evidence = combine_evidence(colour_evidence, wavelet_evidence)
    feature_graph = trace_source_features(stroke_evidence, occupancy, options)

    candidates = sample_nn_phases(source, target_size, options.phase_grid)
    add_baseline_candidates(candidates)
    attach_source_evidence_and_feature_ids(candidates, feature_graph)

    proposals = []
    for feature in feature_graph:
        fits = fit_simple_source_supported_models(feature, target_size, options)
        for fit in bounded_grid_placement_alternatives(fits, options):
            raster = rasterize_without_antialiasing(fit, feature.width)
            augment_missing_candidates_from_original_footprints(
                raster, candidates, feature, source)
            labels = choose_compatible_samples_jointly(raster, candidates, feature)
            labels = complete_corridor_with_supported_fill_labels(
                labels, raster, candidates, feature, baseline)
            if labels are feasible:
                proposals.append(score_patch(raster, labels, feature, baseline))
        proposals.append(no_edit(feature))

    selected = choose_patches_with_conflict_checks(
        baseline, candidates, proposals, feature_graph, options)
    output, provenance = render_selected_source_samples(selected, original_rgb)
    diagnostics = validate_and_measure(output, provenance, feature_graph, options)

    return output, provenance, diagnostics
```

The input preparation, filtering, and feature tracing are shared by all sampling phases. Candidate colour assignment and conflict checks operate primarily near identified features. Cache source evidence and generate extra footprint candidates lazily to avoid storing every source sample for every target pixel.

## 12. Validation plan

Evaluate the full method against centre-sampled NN and three ablations: offset candidates with colour evidence only; colour plus wavelet evidence without geometric fitting; and geometric fitting with wavelet evidence disabled. Keep target sizes and alpha policy identical.

| Test | What it should expose |
| --- | --- |
| Thin lines at multiple slopes, subpixel positions, and reduction ratios | Missing samples, octant bugs, and dependence on sampling phase. |
| Bright lines on dark fields and dark lines on bright fields | Polarity bias. |
| Accent strokes with similar luminance to their backgrounds | Loss caused by grayscale-only analysis. |
| Straight lines, arcs, corners, and T junctions | Connectivity, curvature, anchor handling, and oversmoothing. |
| Closely spaced parallel strokes and small holes | Unsupported joins, collisions, and topology limits. |
| Smooth gradients and textured painted regions | False line promotion and excessive contrast selection. |
| Transparent sprites on different preview backgrounds | Alpha artifacts and contamination from invisible RGB. |
| The supplied elf asset at several target sizes | Bowstring continuity, cloak silhouette, hair highlights, fingers, and clothing seams. |

Measure reconstructed-path connectivity, source-supported feature recall, false joins, silhouette/component and hole changes, geometric displacement, target stroke width, and changed pixels outside feature neighbourhoods. Track runtime and memory by source size and reduction ratio.

Automate the hard output requirements, including provenance and deterministic tie-breaking. For synthetic fixtures, retain known line geometry and intended connections as ground truth. Assess real-asset readability at native size and integer nearest-neighbour zoom; geometric scores alone do not establish visual quality.

The first implementation is successful only if it improves the important strokes over NN without unacceptable silhouette distortion, false joins, or texture artifacts. The contribution of wavelets must be justified by the ablation rather than assumed.

## 13. Technical references

The combined algorithm and its scoring choices above are proposed here. The following sources supply relevant building blocks rather than an existing implementation of game-asset scaling.

1. [DGtal — Digital straight lines and segments](https://www.dgtal.org/doc/stable/moduleArithDSSReco.html). Representations and recognition of connected digital straight segments.
2. [Peter Kovesi — Phase symmetry implementation](https://peterkovesi.com/matlabfns/PhaseCongruency/phasesym.m). Oriented multiscale log-Gabor analysis for line and blob symmetry, with bright and dark polarity controls.
3. [Peter Kovesi — Phase congruency implementation](https://peterkovesi.com/matlabfns/PhaseCongruency/phasecong3.m). Orientation and local phase information that can distinguish lines from step edges.
4. [Scikit-image — Ridge operators](https://scikit-image.org/docs/stable/auto_examples/edges/plot_ridge_filter.html). Detection of structures with stronger transverse than longitudinal variation.
5. [MathWorks — Dual-tree complex wavelet transforms](https://www.mathworks.com/help/wavelet/ug/dual-tree-complex-wavelet-transforms.html). Shift sensitivity of conventional decimated wavelets and properties of an approximately shift-invariant alternative.
6. [Gerstner et al. — Pixelated Image Abstraction](https://gfx.cs.princeton.edu/gfx/pubs/Gerstner_2012_PIA/Gerstner_2012_PIA_full.pdf). Related work on jointly choosing a low-resolution representation and a reduced palette.
7. [Kopf et al. — Content-Adaptive Image Downscaling](https://johanneskopf.de/publications/downscaling/). Related work on adapting down-sampling to source features.
