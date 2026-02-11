use clap::Args;

#[derive(Args)]
pub struct SelfUpdateArgs {
    /// Version to update to
    pub version: Option<String>,

    /// Revert to a previous version
    #[arg(short, long)]
    pub rollback: bool,

    /// Delete old backups during self-update
    #[arg(long)]
    pub clean_backups: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Prompt user for a key update
    #[arg(long)]
    pub update_keys: bool,

    /// Force update to the stable channel
    #[arg(long)]
    pub stable: bool,

    /// Force update to the preview channel
    #[arg(long)]
    pub preview: bool,

    /// Force update to the snapshot channel
    #[arg(long)]
    pub snapshot: bool,

    /// Force update to the 1.x channel
    #[arg(long = "1")]
    pub channel_1: bool,

    /// Force update to the 2.x channel
    #[arg(long = "2")]
    pub channel_2: bool,

    /// Force update to the 2.2.x LTS channel
    #[arg(long = "2.2")]
    pub channel_2_2: bool,

    /// Only store the channel as default and skip the update
    #[arg(long)]
    pub set_channel_only: bool,
}

pub fn execute(_args: &SelfUpdateArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    todo!()
}
