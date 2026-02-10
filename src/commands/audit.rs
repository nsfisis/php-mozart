use clap::Args;

#[derive(Args)]
pub struct AuditArgs {
    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Output format (table, plain, json, summary)
    #[arg(short, long, default_value = "table")]
    pub format: String,

    /// Audit packages from the lock file
    #[arg(long)]
    pub locked: bool,

    /// Handling of abandoned packages (ignore, report, fail)
    #[arg(long)]
    pub abandoned: Option<String>,

    /// Ignore advisories of a given severity (low, medium, high, critical)
    #[arg(long)]
    pub ignore_severity: Vec<String>,

    /// Ignore advisories from sources that are unreachable
    #[arg(long)]
    pub ignore_unreachable: bool,
}

pub fn execute(_args: &AuditArgs) {
    todo!()
}
