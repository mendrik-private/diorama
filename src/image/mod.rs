mod loader;
mod metadata;

pub(crate) use loader::{
    AnimationFrame, LoadedPreview, decode_animation, decode_headless, decode_memory, load_preview,
};
pub use loader::{DecodeLimits, DecodeProbe, probe_decode};
