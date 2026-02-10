use clap::Args;

#[derive(Args)]
pub struct BrowseArgs {
    /// Package(s) to browse
    pub packages: Vec<String>,

    /// Open the homepage instead of the repository URL
    #[arg(short = 'H', long)]
    pub homepage: bool,

    /// Only show the homepage or repository URL
    #[arg(short, long)]
    pub show: bool,
}

pub fn execute(_args: &BrowseArgs) {
    todo!()
}
