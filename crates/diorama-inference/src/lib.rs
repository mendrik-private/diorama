//! Native inference primitives for Diorama's selection preparation.
//!
//! Model execution is deliberately isolated from the GTK application. The
//! worker protocol can therefore hard-cancel a cutout job without a separate
//! host-installed inference executable.

pub(crate) mod resample;

/// Selects the inference device requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Deterministic CPU implementation and the correctness fallback.
    Cpu,
    /// Burn's WGPU backend where every required operation is available.
    Gpu,
}

pub mod birefnet;
pub mod gguf;
