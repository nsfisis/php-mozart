use clap::Parser;
use mozart::commands;
use mozart_core::exit_code;

fn main() {
    let cli = commands::Cli::parse();
    match commands::execute(&cli) {
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
