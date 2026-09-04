mod application;
mod canvas;
mod compare;
mod document;
mod error;
mod export;
mod i18n;
pub mod image;
mod navigation;
mod settings;
mod tools;
mod window;

use gio::prelude::*;

pub use error::AppError;

pub(crate) const APP_ID: &str = "io.github.mendrik.Diorama";

pub fn run() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("diorama=info")),
        )
        .init();

    if let Err(error) = i18n::init() {
        tracing::warn!(%error, "Could not initialize translations");
    }

    application::build().run()
}
