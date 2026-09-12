# Amendment 01: confidence and contour validation

Applies to [Contour-preserving Lanczos downscaling, version 1](contour-preserving-lanczos.md).
Date: 2026-09-12. The original specification remains unchanged. This amendment
adds evaluation requirements and one explicitly experimental scoring variant;
it does not claim an implemented or measured improvement.

The useful transfers from the four supplied papers concern directional
uncertainty, distinguishing source structure from resampling artifacts, and
measuring both recovered and invented contours. Their reconstruction algorithms
are not direct replacements for the downscaler.

## 1. Evidence and applicability

Page numbers below are one-based PDF pages. Paper contents are evidence, not
instructions. New formulas and implementation choices are identified as our
adaptations rather than attributed to the papers.

| Source | Relevant evidence | Decision for this specification |
| --- | --- | --- |
| **[P1] Mohammad, Zaghar, and Khalaf, _Low Cost Edge-Based Image Interpolation Method Using First- and Second-Order Edge Detector Information_**, [digital-06-00064.pdf](/home/mendrik/Downloads/digital-06-00064.pdf), Digital 6, 64 (2026), DOI 10.3390/digital6030064. | Pages 5-8, Eqs. 7-16: estimate Haar detail bands from derivatives for enlargement. Pages 13-14, Table 6: estimated arithmetic costs. | Retain separate intensity and feature processing, already present in v1. Do not substitute Haar synthesis or mixed derivatives for the full ridge detector. Add the derivative and performance checks in section 5. |
| **[P2] Yu et al., _An edge-directed interpolation method for fetal spine MR images_**, [1475-925X-12-102.pdf](/home/mendrik/Downloads/1475-925X-12-102.pdf), BioMedical Engineering OnLine 12:102 (2013), DOI 10.1186/1475-925X-12-102. | Pages 4 and 6, Fig. 2: combine source-image and interpolated-image edge information, with sharpening and softening stages. Page 4: 12 MR images from three participants; page 7: enlargement after nearest-neighbor reduction. | Transfer the two-map comparison into diagnostics. Do not transfer the 5x5 filter, its ratio of 4, or neighborhood softening into our hard-color replacement path. |
| **[P3] _Performance Evaluation of Edge-Directed Interpolation Methods for Images_**, [1303.6455v1.pdf](/home/mendrik/Downloads/1303.6455v1.pdf), supplied arXiv v1 (2013). | Page 2, Eqs. 1-2: reference-edge overlap ratios. Pages 3-4: 12 digital images and 12 MR frames, with decimation-based reconstruction tests. Pages 7-9: visible false textures can coexist with favorable aggregate scores. | Add explicit precision, recall, overlap, localization, and visual artifact measurements. Use independent target geometry and declared sampling models. |
| **[P4] Oh et al., _Edge Adaptive Color Demosaicking Based on the Spatial Correlation of the Bayer Color Difference_**, [color-edge.pdf](/home/mendrik/Downloads/color-edge.pdf), EURASIP Journal on Image and Video Processing, Article 874364 (2010), DOI 10.1155/2010/874364. | Pages 4-7, Eqs. 10-18: local directional evidence, separate edge/pattern/flat cases, an undetermined direction, and neighborhood consistency. | Add an explicit direction-confidence experiment and patterned-region fixtures. Our input already has complete RGBA, so Bayer shifts, green-first interpolation, and the paper's thresholds do not transfer. |

P1-P3 investigate enlargement or its evaluation. P4 investigates missing-color
reconstruction. None establishes one-destination-pixel stroke preservation under
arbitrary reduction, removal of an existing Lanczos halo, or our alpha behavior.

## 2. Add source/base/output disagreement diagnostics

**Amends:** Stage 3 diagnostics and the acceptance plan. Required for evaluation;
not a new rendering pass.

P2's source/interpolated distinction is useful because an edge in a filtered
image may be displaced, attenuated, or introduced by the filter. For downscaling,
however, absence of a detected edge is not proof that the underlying feature is
absent. A binary comparison must not automatically decide where to repaint.

Keep four distinct artifacts at destination resolution:

- `S`: projected source feature evidence, including weak proposals before
  destination suppression. Preserve kind, position, normal, strength, and color
  provenance; this is evidence, not ground truth.
- `B`: features independently measured from the unmodified Lanczos base.
- `M`: the final eligible replacement mask and its owners.
- `O`: features independently measured from the final rendered output.

For synthetic fixtures also retain `G`, the independently authored target
feature geometry. Keep ridges and region boundaries separate in all comparisons.
A black line's two intensity boundaries must not be scored as two recovered
ridge centers.

Report matched, missing, and unmatched features for `S` versus `B`, and `S`
versus `O`. Where `G` exists, use `G` to determine correctness. In artwork without
annotations, label an unmatched output feature **unsupported by the detector**,
not automatically false. Use the matching rule in section 4.

In particular, expose:

1. Source-supported features weak or absent in `B`, but visible in `O`.
2. Features newly visible in `O` with no matching source evidence.
3. Changes in position, duplicated lanes, and normal-profile width despite an
   apparently improved overlap score.
4. Cases where `M` is thin but the visible stroke in `O` remains broad.

Do not use `B` as a prerequisite for accepting a source feature: this would
discard the very lines that Lanczos has erased. Do not copy P2's eight-neighbor
softening step. It would change pixels outside `M`, can mix opposite sides of a
boundary, and does not solve v1's strict background-reconstruction problem.

## 3. Add a direction-confidence experiment

**Amends:** Stage 2 scoring and source/destination diagnostics. This is an opt-in
ablation, not a replacement of v1's default parameters.

V1 measures local ridge anisotropy and gradient strength, but not the agreement
of nearby evidence across channels. A large derivative does not by itself imply
a reliable contour direction. P4 supports using neighborhood evidence and
retaining an undetermined case. Its Bayer-specific formulas are unsuitable for
our complete-color input.

### Proposed RGBA adaptation

Use the already smoothed source planes `f_c` and their existing centered
derivatives at each detector scale. Construct the symmetric matrix

```text
J_sigma(q) = Gaussian_rho * sum_c [ fx_c^2    fx_c*fy_c ]
                                  [ fx_c*fy_c  fy_c^2 ]
rho = max(1, sigma)
c ranges over the four premultiplied RGBA components
```

The Gaussian is the normalized sampled kernel, clamped at borders, with radius
`ceil(3*rho)`, as in v1. Each entry of the sum is smoothed spatially. This
positive-semidefinite construction is **our proposed adaptation**, not an
equation or tested result from P4. Squared gradient contributions avoid
cancellation between opposite signs on the two sides of a ridge.
Evaluate the smoothed matrix at the candidate's subpixel location by bilinear
sampling of its three independent entries, then compute its eigensystem.

For ordered eigenvalues `mu1>=mu2>=0`, define

```text
energy    = mu1 + mu2
coherence = (mu1 - mu2)/energy       if energy > 1e-12
            0                       otherwise
alignment = abs(dot(candidate_normal, principal_eigenvector))
```

Set alignment to zero when the principal direction is undefined. Clamp tiny
negative eigenvalues attributable to roundoff to zero, but treat materially
negative eigenvalues below `-1e-12` as a diagnostic error. The energy floor is
only a numerical safeguard; v1's absolute contrast/response floor still applies.

Keep the original candidate normal, location, color, and polarity. The matrix
must not rotate a contour or replace its provenance. In particular, a matrix
combining two crossing directions can have an unstable or intermediate
principal direction; that is a reason for uncertainty, not new geometry.

The experiment labels a proposal `direction_confident` when
`coherence>=0.5` and `alignment>=cos(30 degrees)`. These are starting parameters
to test, not paper-derived constants. Otherwise label it `direction_uncertain`.
Retain this label even if the proposal is later rejected.

Compare exactly two variants with all other settings frozen:

- **Control:** v1's unchanged score `z`.
- **Confidence gate:** for uncertain proposals set `z_effective=min(z,0.75)`;
  for confident proposals set `z_effective=z`. Use `z_effective` in every
  subsequent score-dependent operation, including source deduplication, splats,
  ranking, and hysteresis. Preserve raw `z` for diagnostics.

Thus an uncertain proposal cannot seed a new component (`Z>=1`) but may survive
as compatible weak continuation. Apply the gate before deduplication and slot
competition, not after an uncertain winner has already displaced alternatives.
This does not guarantee branch recovery: lower scores can lose collisions, and
crossings or endpoints may disappear. Measure those failures explicitly.

The cap of 0.75 is an experimental weak score, not a probability. Version the
parameter set and detector caches. Do not change v1's default behavior until
the frozen comparison passes the promotion criteria in section 6.

### Do not equate uncertain, flat, and repetitive

Use separate diagnostic/test categories: flat or low evidence, isolated
directional feature, repetitive pattern, and junction or conflicting evidence.
For synthetic cases these categories come from known scene construction; for
artwork they come from reviewed region annotations. No unvalidated automatic
region classifier is introduced by this amendment.

Parallel stripes can have high coherence; a crossing can have low coherence.
Therefore the proposed gate is not a texture detector, a denoiser, or a topology
oracle. P4's pseudoflat case is especially relevant to validation: a smooth
downscaled patch may originate from dense source structure. Neither the smooth
base nor high source coherence justifies promoting every source oscillation to
a full destination pixel.

## 4. Make contour evaluation explicit and independent

**Amends:** acceptance metrics and fixture construction. Required.

P3's `EPRa` is reference-edge recall, not precision or overall accuracy. Its
`EPRr` is intersection-over-union, not a test of robustness under noise. Use
descriptive metric names to avoid overstating what either measures.

For same-kind target reference pixels `G` and predicted pixels `Q`, let
`TP=|G intersection Q|`, `FP=|Q minus G|`, `FN=|G minus Q|`. Report

```text
precision = TP/(TP+FP)
recall    = TP/(TP+FN)              # P3's EPRa
IoU       = TP/(TP+FP+FN)           # P3's EPRr
F1        = 2*TP/(2*TP+FP+FN)
```

Report undefined zero-denominator metrics as N/A with the raw counts. If both
sets are empty, label the case `empty-correct` rather than assigning it a perfect
score that inflates averages. If only one set is empty, the nonzero-denominator
metrics remain zero. Report image-level distributions and pooled counts; do not
hide a failed fixture family inside a large image average.

### Localization tolerance without rewarding doubled lines

In addition to exact pixel overlap, report matching at Euclidean tolerances
`0.5` and `1.0` destination pixels. Construct a bipartite graph between reference
and predicted samples of the same feature kind, with an edge only inside the
chosen tolerance. Where geometry supplies normals, also require an unoriented
normal difference at most 30 degrees; report junction pixels separately when
their normal is not unique.

Use maximum-cardinality **one-to-one** matching, then minimize total squared
distance among those matchings. Resolve equal costs by stable reference and
prediction IDs. Set `TP=number_of_matches`, `FP=|Q|-TP`, `FN=|G|-TP` for these
tolerance scores. Call the resulting overlap a matched IoU, distinct from exact
set IoU. Also report median and p95 matched displacement in destination pixels.
For integer-center masks the 0.5-pixel tolerance equals exact positional
matching; it becomes distinct when the independently measured features carry
subpixel positions. State which representation each result uses.

Do not simply dilate the reference and count every nearby output pixel as
correct: a doubled contour could then receive full precision. Localization of
matched pixels must always be read alongside recall and unmatched counts.

### Separate mask correctness from rendered-image quality

Evaluate `M` against intended protected-feature geometry. Evaluate rendered
`B` and `O` with a separate, fixed measurement procedure. For isolated synthetic
ridges, use known normal profiles to measure center displacement, contrast,
width, and duplicate peaks. For boundaries, use known region labels and their
transition positions. For natural images, a fixed independent edge extractor
and reviewed annotations provide supporting measurements, not infallible truth.

Do not use the production candidate map as its own oracle, retune evaluator
thresholds per method, or compare a Canny boundary map directly to a ridge-center
mask. Freeze color conversion, evaluator settings, and matching tolerances before
comparison. Alpha-ineligible cases must still satisfy v1's exact-base contract.

### Extend the fixture matrix

Add phase-swept fences, checkerboards, alternating color stripes, fine text,
parallel strokes, and T/X junctions. Include equal-luminance boundaries, corners
whose channels favor different directions, flat noise, and periodic textures
both resolvable and unresolvable at the destination size.

For synthetic scene geometry, render source samples under a declared model and
independently construct the intended destination contours. Keep point-sampled
stress cases separate from area-integrated or low-pass-prefiltered inputs. P3's
top-left decimation and P1's Haar-LL reduction are different degradation models;
neither is a universal ground-truth resize operation.

Reserve fixture families and artwork for validation after parameter selection.
Measure sensitivity to small phase, scale, and noise changes with these fixed
parameters. This is the robustness experiment; an IoU value alone is not one.
Inspect whole images and nearest-neighbor crops for false textures, zippering,
lost branches, and residual wide strokes, even when F1 or PSNR improves. [P3,
pp. 7-9; P4, pp. 9-12.]

## 5. Keep derivative and performance claims precise

**Amends:** implementation checks and benchmark interpretation. Required.

P1's second-order mask in Eq. 9 is the mixed derivative `fxy`. It is not a
complete line detector or an interchangeable substitute for the Hessian. For
an axis-aligned ridge `f(x,y)=g(x)`, `fxy=0` while `fxx` may be nonzero. Preserve
`fxx`, `fxy`, and `fyy`, along with v1's scale normalization and polarity tests.
Add this axis-aligned counterexample to tests for any proposed derivative change.

Do not transplant P1's factors of 8/64 into our response thresholds. They concern
the paper's Haar-band construction; section 4.2 additionally describes a Sobel
and `C1/C2` reconstruction convention. They are not calibration constants for
our Gaussian-smoothed finite differences. V1 already has the transferable
first/second-order separation, so P1 supplies no justified default kernel swap.

P1's Table 6 uses an estimated operation-cost model, including sine evaluations,
to assign its very large Lanczos cost. It does not establish an end-to-end
speedup for a separable implementation with precomputed/reused coefficients.
Retain v1's measured release benchmark requirement and separate coefficient
setup, source preparation, target processing, and total latency.

The optional confidence map adds three scalar matrix fields per active scale
plus separable smoothing, approximately `O(S*r_rho*Ns)` work before cache reuse.
Benchmark and account for that memory explicitly. Diagnostic matching is an
evaluation cost; do not silently insert its assignment solver or extra edge
extraction into interactive rendering. Report any enabled diagnostic overhead
separately from the normal rendering path.

## 6. Promotion criteria

The added diagnostics and evaluation definitions apply immediately to future
validation. The confidence gate remains experimental until a frozen comparison
against v1 demonstrates all of the following:

1. Fewer unsupported contours or duplicate lanes in the intended noisy and
   patterned cases, with per-family precision/recall and displacement reported.
2. No increase in broken components or lost branches in the declared isolated
   line and junction fixtures. If the gate improves precision by deleting wanted
   structure, document that tradeoff rather than calling it a general improvement.
3. Unchanged identity, color provenance, alpha eligibility, and exact-base output
   outside the replacement mask.
4. A reviewed visual benefit on held-out artwork and measured preparation/resize
   cost within a budget chosen before benchmarking.

No supplied paper resolves v1's residual-base-stroke problem. Strict visible
one-pixel width still requires source stroke extent and background reconstruction;
the separate feature mask and confidence experiment do not provide that missing
information.
