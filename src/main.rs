use gio::prelude::*;

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("diorama=info")),
        )
        .init();

    let application = diorama::application::build();
    application.run()
}
