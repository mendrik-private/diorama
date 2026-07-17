pub mod loader;
pub mod metadata;

pub use loader::{
    AnimationFrame, DecodeLimits, DecodeProbe, LoadedPreview, decode_animation, decode_headless,
    decode_memory, load_preview, probe_decode,
};
