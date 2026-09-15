//! Runtime CPU multiversioning, following `core::arch`'s auto-vectorization
//! example. Only feature dispatch is unsafe; all arithmetic and indexing stay
//! in the shared, safe solver. AVX provides four f64 lanes (AVX2 is not needed).
#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use super::{CancellationToken, Result, Row, solve_portable};

pub(super) fn solve(
    rows: &[Row],
    rhs: &[[f64; 4]],
    diagonal: &[f64],
    cancel: &CancellationToken,
) -> Result<Vec<[f64; 4]>> {
    if std::is_x86_feature_detected!("avx") {
        // SAFETY: this module is compiled only for x86/x86_64, and detection
        // checks both CPU AVX support and OS support for saving its registers.
        // The callee adds no pointer, alignment, length, or aliasing invariants.
        unsafe { solve_avx(rows, rhs, diagonal, cancel) }
    } else {
        solve_portable(rows, rhs, diagonal, cancel)
    }
}

/// Compile the shared, bounds-checked solver for four-wide f64 arithmetic.
///
/// # Safety
///
/// The running CPU and OS must support AVX. The sole caller above establishes
/// this with runtime detection before entering this function. There are no
/// additional memory or alignment preconditions beyond the Rust references.
#[target_feature(enable = "avx")]
unsafe fn solve_avx(
    rows: &[Row],
    rhs: &[[f64; 4]],
    diagonal: &[f64],
    cancel: &CancellationToken,
) -> Result<Vec<[f64; 4]>> {
    solve_portable(rows, rhs, diagonal, cancel)
}
