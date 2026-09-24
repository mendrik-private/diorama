use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Cooperative cancellation. Closures can adapt a host application's token.
pub trait Cancellation: Sync {
    fn is_cancelled(&self) -> bool;

    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl<F: Fn() -> bool + Sync> Cancellation for F {
    fn is_cancelled(&self) -> bool {
        self()
    }
}

/// A lightweight cancellation token shared across threads and clones.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Cancellation for CancellationToken {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Antialiasing intensity, clamped to 0–100; the default is 50%.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameAssetAa(u8);

impl GameAssetAa {
    pub const fn new(percent: u8) -> Self {
        Self(if percent > 100 { 100 } else { percent })
    }
    pub const fn percent(self) -> u8 {
        self.0
    }
}

impl Default for GameAssetAa {
    fn default() -> Self {
        Self(50)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("The operation was cancelled")]
    Cancelled,
    #[error("Game Asset scaling requires nonzero dimensions no larger than the source")]
    InvalidDimensions,
    #[error(
        "Game Asset scaling would exceed the configured {limit_bytes} byte working-memory limit"
    )]
    GameAssetMemoryLimit { limit_bytes: u64 },
    #[error("Could not scale the image: {0}")]
    Scaling(String),
}

pub type Result<T> = std::result::Result<T, Error>;
