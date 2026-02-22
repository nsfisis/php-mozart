use clap::Parser;
use mozart::commands;
use mozart_core::exit_code;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn init_tracing(profile: bool, verbose: u8, quiet: bool) {
    // MOZART_LOG environment variable takes highest priority.
    if let Ok(env_filter) = EnvFilter::try_from_env("MOZART_LOG") {
        tracing_subscriber::registry()
            .with(fmt::layer().with_writer(std::io::stderr))
            .with(env_filter)
            .init();
        return;
    }

    if profile {
        let filter = match verbose {
            0 => "mozart=info",
            1 | 2 => "mozart=debug",
            _ => "mozart=trace",
        };
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_timer(fmt::time::uptime())
                    .with_span_events(fmt::format::FmtSpan::CLOSE),
            )
            .with(EnvFilter::new(filter))
            .init();
    } else if verbose >= 3 && !quiet {
        tracing_subscriber::registry()
            .with(fmt::layer().with_writer(std::io::stderr).with_target(false))
            .with(EnvFilter::new("mozart=debug"))
            .init();
    }
    // Otherwise: no subscriber installed → tracing macros are effectively zero-cost no-ops.
}

#[tokio::main]
async fn main() {
    let cli = commands::Cli::parse();
    init_tracing(cli.profile, cli.verbose, cli.quiet);
    match commands::execute(&cli).await {
        Ok(()) => {}
        Err(e) => {
            // Check if this is a structured MozartError with a specific exit code.
            if let Some(mozart_err) = e.downcast_ref::<exit_code::MozartError>() {
                // Only print a message when there is one (bail_silent produces empty message).
                if !mozart_err.message.is_empty() {
                    eprintln!("{}", mozart_core::console::error(&mozart_err.message));
                }
                std::process::exit(mozart_err.exit_code);
            }

            // Generic anyhow error — print and exit with GENERAL_ERROR.
            eprintln!("{}", mozart_core::console::error(&format!("{e:#}")));
            std::process::exit(exit_code::GENERAL_ERROR);
        }
    }
}
