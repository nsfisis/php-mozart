use clap::Args;

#[derive(Args)]
pub struct SuggestsArgs {
    /// Package(s) to list suggestions for
    pub packages: Vec<String>,

    /// Group output by package
    #[arg(long)]
    pub by_package: bool,

    /// Group output by suggestion
    #[arg(long)]
    pub by_suggestion: bool,

    /// Show suggestions for all packages, not just root
    #[arg(short, long)]
    pub all: bool,

    /// Show only suggested package names in list format
    #[arg(long)]
    pub list: bool,

    /// Disables suggestions from require-dev packages
    #[arg(long)]
    pub no_dev: bool,
}

pub fn execute(_args: &SuggestsArgs) {
    todo!()
}
