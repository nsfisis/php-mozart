use clap::Args;

#[derive(Args)]
pub struct InitArgs {
    /// Name of the package (vendor/name)
    #[arg(long)]
    pub name: Option<String>,

    /// Description of the package
    #[arg(long)]
    pub description: Option<String>,

    /// Author name of the package
    #[arg(long)]
    pub author: Option<String>,

    /// Type of the package
    #[arg(long, value_name = "TYPE")]
    pub r#type: Option<String>,

    /// Homepage of the package
    #[arg(long)]
    pub homepage: Option<String>,

    /// Package(s) to require
    #[arg(long)]
    pub require: Vec<String>,

    /// Package(s) to require for development
    #[arg(long)]
    pub require_dev: Vec<String>,

    /// Minimum stability (stable, RC, beta, alpha, dev)
    #[arg(short, long)]
    pub stability: Option<String>,

    /// License of the package
    #[arg(short, long)]
    pub license: Option<String>,

    /// Add a custom repository
    #[arg(long)]
    pub repository: Vec<String>,

    /// Define a PSR-4 autoload namespace
    #[arg(short, long)]
    pub autoload: Option<String>,
}

pub fn execute(_args: &InitArgs) {
    todo!()
}
