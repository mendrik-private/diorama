mod loader;
mod metadata;
mod video;

pub(crate) use loader::{
    AnimationFrame, LoadedPreview, decode_animation, decode_headless, decode_memory, load_preview,
};
pub use loader::{DecodeLimits, DecodeProbe, probe_decode};
pub(crate) use video::{current_frame as current_video_frame, is_video, open_video};
