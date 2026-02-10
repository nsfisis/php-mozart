use clap::Args;

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to composer.json file
    pub file: Option<String>,

    /// Skips checks for non-essential issues
    #[arg(long)]
    pub no_check_all: bool,

    /// Validates the lock file
    #[arg(long)]
    pub check_lock: bool,

    /// Skips lock file validation
    #[arg(long)]
    pub no_check_lock: bool,

    /// Skips publish-related checks
    #[arg(long)]
    pub no_check_publish: bool,

    /// Skips version constraint checks
    #[arg(long)]
    pub no_check_version: bool,

    /// Also validate all dependencies
    #[arg(short = 'A', long)]
    pub with_dependencies: bool,

    /// Return a non-zero exit code on warnings as well as errors
    #[arg(long)]
    pub strict: bool,
}

pub fn execute(_args: &ValidateArgs) {
    todo!()
}
