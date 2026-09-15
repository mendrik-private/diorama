# Game Asset CPU multiversioning experiment

## Contract

Further optimize the existing scaler, preserving exact reviewed RGBA output,
per-channel f64 accumulation order, signed zero, solver tolerance, restart and
iteration limits, cancellation, and the portable implementation. No FMA,
approximate math, changes to source preparation, or target-result caching.

Primary workload: reviewed 800×800 elf → 128×128. Also measure 257×193 → 64×48
and 1024×768 → 128×96 synthetic images and each benchmark's two subsequent
targets. Inputs are contiguous straight-alpha sRGB RGBA8; the solver operates
on interleaved premultiplied linear `[f64; 4]`. Work is single-threaded and
includes allocations and completed output construction, but excludes decoding.

Hypothesis: compiling the four independent solver lanes for AVX instead of
baseline x86-64 SSE2 reduces repair and end-to-end latency, without changing
the arithmetic inside a lane. Disassembly without four-wide arithmetic, any
changed result, or end-to-end timings within observed variation falsify a win.

Use normal release settings (thin LTO, one codegen unit, no `target-cpu=native`)
on the Ryzen AI MAX+ PRO 395, Linux x86_64, rustc 1.94.1 / LLVM 21.1.8. Save
before/after executables and alternate A–B–B–A on CPU 2. Fix seven samples per
case per run after the existing untimed pilot before inspecting candidate
results. Report both repetitions, median, p95, and IQR; these are warm-process,
new-source previews, not GUI startup. Do not run builds or profilers alongside
timed comparisons. If machine activity overwhelms the effect, report it rather
than choosing the fastest run.

The first comparison showed large variability even in unchanged source
analysis. Before examining further samples, add one bounded confirmation:
12 fresh-process pairs of the existing exact `game_asset_first_preview` test,
alternating within-pair A/B order, on CPU 2. No extra samples after these pairs.
This measures the real session's 800→128 path, not the stage-summed benchmark;
its timer still excludes PNG decoding. Report the paired ratios and both
distributions, without replacing or discarding the initial results.

```sh
DIORAMA_BENCH_SAMPLES=7 taskset -c 2 <saved-test-binary> \
  tools::scale::game_asset::benchmarks::game_asset_benchmark \
  --exact --ignored --nocapture --test-threads=1
```

The approach follows the runtime CPU-feature dispatch and auto-vectorization
examples in [Rust's `core::arch` documentation](https://doc.rust-lang.org/core/arch/index.html).
Architecture-specific code must remain behind a runtime feature check; a
globally native-targeted binary would not preserve portability.

## Implementation and safety

A fresh 499 Hz sample of the existing 800→128 first preview attributed about
41% of cycles to the solver, 12% to radius queries, 8% to sorting and 6% to
Gaussian convolution. The baseline solver uses two-wide SSE2 `mulpd`/`addpd`.

The candidate dispatches once per solve using `is_x86_feature_detected!("avx")`.
A private x86/x86_64 module calls a `#[target_feature(enable = "avx")]` entry
point only after detection succeeds; other CPUs use the portable implementation.
The solver and its two inner kernels are inlined into that entry point. There
is one arithmetic implementation, not a separate intrinsic rewrite. Slice
bounds checks, Rust allocation alignment and borrow-checked access remain
intact. No raw pointers, overreads, vector alignment assumptions or tail loads
are introduced. AVX2 and FMA are not enabled or required.

With explicit user approval, the Cargo unsafe lint changes from `forbid` to
`deny`; only this private SIMD module allows it, with
`unsafe_op_in_unsafe_fn` denied. Its only unsafe call has one precondition:
AVX must be supported by the running CPU/OS. Runtime detection establishes that
precondition. All other code continues to reject unsafe by default.

Tests compare runtime-selected and portable solves bit for bit against the
frozen scalar oracle, including one-axis shapes, differing channel magnitudes
and convergence, and disconnected masks. Direct boundary tests cover empty
systems, signed-zero RHS, cancellation and a non-positive-definite system.
The three independently reviewed image fixtures must remain unchanged.

Release disassembly confirms YMM-register `vmulpd`, `vaddpd`, `vsubpd` and
`vdivpd` in the AVX solver, including the sparse product; no fused multiply-add
instructions appear in that function. The release test executable grew from
24,821,176 to 24,852,192 bytes (30.3 KiB, 0.125%, including the added test).
There are no new per-image allocations, persistent caches or dependencies.

Validation completed: `cargo fmt --all -- --check`, `git diff --check`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets`
(215 unit tests and one integration test passed; 58 ignored). The release test
binary separately passed all 25 non-ignored Game Asset tests. All four source
and expected PNG SHA-256 hashes match the previous performance report.
`cargo check --all-targets` also passed. Crusty's advisory detector still reports
a missing safety contract on `solve_avx`, despite its adjacent `# Safety`
documentation and the sole caller's `SAFETY:` explanation. Manual review traced
that caller through runtime CPU/OS detection; the function has no additional
memory preconditions. No other new or worsened findings were reported.
Non-x86 hardware and the declared Rust 1.92 toolchain were not available for
runtime/MSRV validation; no new API requires a newer compiler. The unsafe
boundary has no manual memory operations for Miri/sanitizers to exercise.

## Results (2026-09-15)

The change reduces measured CPU work, but this desktop session does **not**
support a reliable large wall-clock speedup claim. Keep the small dispatcher
because it preserves one safe arithmetic implementation and exact output while
reducing retired instructions and the typical paired cycle count.

All initial measurements, including unfavorable and noisy ones, are in
[the A–B–B–A CSV](game-asset-simd.csv). First-resize medians in milliseconds:

| Workload | Before A1 | AVX B1 | AVX B2 | Before A2 |
| --- | ---: | ---: | ---: | ---: |
| 800×800 → 128×128 | 1940.0 | 843.5 | 1602.6 | 1519.8 |
| 257×193 → 64×48 | 23.1 | 19.9 | 57.9 | 60.6 |
| 1024×768 → 128×96 | 443.3 | 790.3 | 976.0 | 404.3 |

The unchanged analysis stage varies by roughly 3× across some runs; even the
sign of the larger end-to-end timing differences is inconsistent. The small
synthetic target-render medians improve in both comparisons, but these results
are not evidence of a universal preview latency improvement. All output CRCs
remain `7f94c4c2`, `d67bfbe9` and `21c31d3d` for elf/odd/large.

The bounded 12-pair real-session confirmation adds user-space `perf stat`
task-clock/cycle/instruction counters around each test process. All 24 exact
checks passed. Raw results are in [the paired CSV](game-asset-simd-paired.csv).
Counters include test startup and image decoding; the preview timer excludes
them. Summaries below use interpolated percentiles (an even-count median is
the average of the two center samples).

| Metric | Before median / p95 / IQR | AVX median / p95 / IQR |
| --- | ---: | ---: |
| Preview, ms | 1783.9 / 2743.5 / 950.0 | 1557.2 / 2903.2 / 905.6 |
| User-space cycles, billions | 4.053 / 5.156 / 0.417 | 3.852 / 4.405 / 0.287 |
| Retired instructions, billions | 13.321 / 13.322 / 0.0013 | 11.576 / 11.579 / 0.0036 |

The median within-pair instruction reduction is 13.1%; the median within-pair
cycle reduction is 3.2%, with fewer cycles in 11/12 pairs. Wall latency improves
in 9/12 pairs, but the paired speedup has median 1.147×, IQR 0.385× and range
0.472–2.565×. Do not present the 12.7% difference between overall wall medians
as a reliably attributable speedup. Observed cycles per scheduled time range
from 1.48 to 4.59 GHz, consistent with substantial frequency variation; this is
not proof of its cause. CPU governor, process priorities and background
applications were left unchanged.

The normal development executable was rebuilt after measurement. The feature
check applies in both optimized development and release builds. This does not
change the earlier development-profile fix or the GUI preview scheduler.
