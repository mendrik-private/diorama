# Game Asset performance

## Contract and measurement

Preserve the reviewed 800×800 elf at 128, 160 and 200 pixels exactly. Preserve
the solver's explicit per-channel residual check, `max(1e-12, 1e-12 * ||rhs||)`,
iteration bound, cancellation, reflecting boundaries and premultiplied linear
RGBA semantics. No approximate math, image-specific shortcuts or fixture caches.

Measure completed, single-threaded CPU work with the normal release profile
(`opt-level=3`, thin LTO, one codegen unit; no `target-cpu=native`). Input is
contiguous, owned RGBA8 with straight alpha and sRGB RGB; analysis uses f64,
and repair operates on premultiplied linear RGBA. Decoding is excluded;
allocations and output construction are included. Source and destination do not
overlap. Identity copies remain outside this downscaling workload.

The ignored benchmark exercises source analysis, a first resize and three
distinct targets per session. Later targets reuse source analysis, but do not
hit the existing one-target result cache. The fixtures are the reviewed elf,
a 257×193 odd rectangular image, and a 1024×768 image. Synthetic inputs contain
curved and vertical dark strokes, color variation, and transparency; one target
reduces only the vertical axis.

```sh
cargo test --release --lib game_asset_benchmark -- --ignored --nocapture --test-threads=1
```

One pilot warms code/data and fixes the sample count before measurement:
approximately eight seconds per case, with a minimum of seven and maximum of
31 samples. `DIORAMA_BENCH_CASE=elf|odd|large` selects a case;
`DIORAMA_BENCH_SAMPLES` fixes the count for comparisons between saved binaries.
Report median, p95, IQR and ns/source pixel. Output equality on repeated runs
detects nondeterminism; reviewed image tests and independent numerical tests
provide correctness oracles. Checks run outside the timed section.

Hypothesis: sharing sparse-matrix reads across independent RGBA solves reduces
repair latency; reusing Gaussian intermediates and moving boundary calculations
out of pixel/tap loops reduces analysis latency. Differences within measured
variability, changed outputs, or reduced convergence accuracy falsify a win.

## Baseline

Measured on an AMD Ryzen AI MAX+ PRO 395 (16 cores / 32 threads), Radeon 8060S,
Linux x86_64, rustc 1.94.1 / LLVM 21.1.8. The scaler uses one CPU thread.

Sampling the original reviewed-image test at 499 Hz attributed approximately
64% of cycles to biharmonic repair, 11% to Gaussian filtering, 7% to radius
queries, 4% to sorting, and 2% to nearest queries. These are samples from a
sequence of one source analysis and three target renders, not a first-render
breakdown. Disassembly identified scalar multiply/add and indexed matrix
access in the solver as the dominant loop.

## Final results (2026-09-15)

Saved original and final release test binaries were run on CPU 2 in A–B–B–A
order, with seven samples per case per run after each pilot. The first A/B pair
is reported below; both repetitions and every target's distribution are in
[the measurement CSV](game-asset-performance.csv). These measure a new source
in a warmed process, not application startup or file decoding. No compiler or
other test run was deliberately run alongside the measured samples.

First-resize times, including source analysis, in milliseconds:

| Source → target | Before median / p95 / IQR | After median / p95 / IQR | Median speedup |
| --- | ---: | ---: | ---: |
| Reviewed 800×800 → 128×128 | 1784.6 / 1924.4 / 68.5 | 870.7 / 915.9 / 31.5 | 2.05× |
| Synthetic 257×193 → 64×48 | 64.4 / 64.7 / 0.1 | 20.8 / 21.7 / 0.4 | 3.10× |
| Synthetic 1024×768 → 128×96 | 1322.1 / 1329.6 / 2.7 | 456.0 / 499.5 / 39.9 | 2.90× |

Source-analysis medians fell from 618.4 to 190.4 ms, 46.0 to 10.8 ms, and
845.3 to 213.3 ms, respectively. With source analysis already cached, the
reviewed image's 128/160/200 targets fell from 1168.8/1169.9/1156.1 ms to
680.3/639.7/601.6 ms. Across the nine target renders, first-pair speedups range
from 1.46× to 1.93×. The existing identical-target cache is not involved.

The second repetition had substantial timing variation: first-resize medians
were 2474.6/81.1/1379.4 ms before and 922.9/24.3/621.4 ms after. The improvement
remained visible, but these measurements are not evidence for stable p95 latency
on an idle machine. They cover one real asset and two synthetic workloads, not
all possible images or hardware.

First-target RGBA CRC32 remained `7f94c4c2`, `d67bfbe9`, and `21c31d3d` for
elf/odd/large in every before/after run. The independent reviewed-image tests
separately require exact equality at all three target sizes.

One `/usr/bin/time` measurement of the reviewed-image test (one analysis and
three target renders, including decoding) reported peak RSS of 102100 KiB
before and 99740 KiB after, approximately 99.7 and 97.4 MiB. This is whole-process
resident memory for that workload, not an allocation count or a universal bound.

## Development-launch regression

The release measurements above did not cover the user's normal development
launch. A subsequent report of an 800→128 preview appearing to run forever
identified `target/debug/diorama` consuming a CPU for more than two minutes.
The existing `[profile.dev.package."*"]` optimization covered dependencies,
but not Diorama's own numerical kernels. The default app crate was compiled
at optimization level zero. See [Cargo's profile override rules](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides).

The focused `game_asset_first_preview` check runs the real session on the
reviewed 800×800 source, requests 128×128, and checks exact reviewed output.
Before the profile fix, an eight-second deadline terminated it with exit 124.
CPU samples showed substantial time in uninlined iterator operations and
pointer precondition checks. The original three-target golden test completed
in 3.7 seconds in release mode but also exceeded that deadline in debug mode.

`[profile.dev] opt-level = 3` now optimizes the app crate too. Normal launches
and tests retain debug symbols, debug assertions and integer overflow checks.
This trades some development compile time and source-stepping precision for
responsive numerical work. It does not change the scaling algorithm or its
convergence/quality checks. Explicitly unoptimized stepping remains available
through `CARGO_PROFILE_DEV_OPT_LEVEL=0`.

After rebuilding, the same first-preview check passed the eight-second deadline
in all three runs: 1764, 2772, and 1239 ms. Every run retained
`debug_assertions=true` and exact RGBA CRC32 `7f94c4c2`. Cargo's artifact metadata
confirmed optimization level 3, debug symbols, debug assertions and overflow
checks for the rebuilt `target/debug/diorama` executable. The normal development
launch therefore exercises optimized kernels without requiring `--release`.
`cargo test --all-targets` then passed 214 unit tests and one integration test
(58 ignored), with the unit suite completing in 3.07 seconds. Formatting,
all-target checking and strict Clippy also passed for the updated profile.

To check the first preview in the normal development profile:

```sh
cargo test --lib game_asset_first_preview -- --ignored --nocapture --test-threads=1
```

Its printed timing excludes compilation and source PNG decoding. For a hard
deadline, first build using `cargo test --lib --no-run`, then run the test binary
reported by Cargo under `timeout 8s`, with the same test filter and arguments.
Timing remains a manual check; normal CI does not assert a wall-clock threshold.

## Implementation and tradeoffs

The four scalar conjugate-gradient solves now run as four independent lanes in
one traversal. Matrix indices and coefficients are shared, while each channel
keeps its own step, stopping threshold and restart. Accumulation order within
each channel is unchanged. Release disassembly confirms packed double-precision
`mulpd`/`addpd` instructions in the matrix product. Test-only copies of the original
scalar solver and Gaussian implementation provide exact floating-point oracles.

The six Gaussian derivatives at each scale now share their three distinct
vertical intermediates: nine one-dimensional passes replace twelve. Boundary
reflection happens once per source row/tap or through a reusable padded row,
instead of inside every pixel/tap calculation. Contiguous pixel loops permit
SIMD without reassociating a pixel's sum. Intermediates are dropped before the
next vertical derivative is computed.

The sRGB conversion uses a 256-entry (2 KiB) table containing exactly the old
function's value for each input byte. Smoothing builds spatial bins at its actual
search radius rather than five source pixels. It still filters by the same
distance and sorts the same IDs, so membership and floating-point accumulation
order do not change.

Batching keeps four four-channel solver vectors live instead of five scalar
vectors, reusing the matrix-product buffer for the preconditioned residual. It
also removes the separate per-channel RHS and repaired-output buffers.
The vector increase during the solve is 48 bytes per masked pixel. Sparse rows
now store 32-bit indices (the existing size limit bounds them below 2^21),
reducing each row from 128 to 80 bytes on 64-bit hosts and offsetting that
increase. Gaussian sharing does not retain additional full-frame fields at peak.
The accepted image-size and working-set limits are unchanged; this does not introduce
threads, GPU buffers, new dependencies or persistent full-image caches.

This is reuse of repeated subproblems within a render, not cached answers to
specific images. A more extensive dynamic-programming cache for repaired
backgrounds would need to key on the complete retained-ink mask: target size
changes which source contours are removed. More aggressive solver
preconditioners or GPU reductions would also need new numerical parity evidence.

## GPU considerations

The existing ordinary-resampling GPU implementation uses WGSL. The Game Asset
pipeline depends on f64 arithmetic, discrete contour decisions and a 1e-12
solver tolerance. A GPU implementation would need a separate parity and
convergence investigation. wgpu exposes f64 through its native Vulkan SPIR-V
path and warns that GPU double precision can be much slower than single
precision ([wgpu feature documentation](https://wgpu.rs/doc/wgpu_types/features/struct.FeaturesWGPU.html#associatedconstant.SHADER_F64)).
This is a compatibility and precision consideration, not a measured rejection
of GPU acceleration. Any future GPU comparison must include transfers,
dispatch, reductions, synchronization and pipeline compilation separately from
kernel time, followed by an end-to-end comparison.

## Correctness and checks

- The three independently reviewed RGBA fixtures are unchanged and match exactly.
- The batched solver matches the frozen scalar solver bit for bit on one-axis
  images and several masks, including zero RHS and channels with different
  magnitudes, signs and convergence times.
- All six Gaussian derivatives match the frozen scalar implementation bit for
  bit at all four production sigmas, including signed zero, constant fields,
  negative samples, odd sizes, one-axis images and kernels larger than the image.
- Lookup-table tests cover every input byte; spatial-query tests compare with
  brute force across bin sizes, negative coordinates, duplicate points and ties.
- Existing tests cover independent SciPy values, masked-source poisoning,
  transparency, fractional area conservation, cancellation and cache behavior.

Validation: `cargo fmt --all -- --check`, `cargo check --all-targets`,
`cargo clippy --all-targets -- -D warnings`, `cargo test` (214 unit tests plus
one integration test passed; 57 tests ignored), and 24 scaler tests in both
debug and release builds after the final matrix layout change. Crusty validation
reported no new or worsened architectural findings. Ignored graphical tests
were not exercised by this computation-only change.

Source fixture SHA-256:
`cc3e15e92312cd825feadba7fde6424cb22bda94a36c9467d71b024f0ed8289f`.
Expected 128/160/200 PNG SHA-256 hashes, respectively:
`c89cb8b11b6ca91e86396f4cb4885bbcfc6d161a2ae68cc437d60fbd115a399f`,
`7df63affa7297b687109204dd8a10b3a62e8dbd7ccc9505ed7a91d4031eeffcd`,
`2a11e37bf0db785433b0a8c76ce6336cc441c784aaf56e84753bd697f13f7f7d`.
