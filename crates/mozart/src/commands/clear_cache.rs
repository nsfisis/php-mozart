use clap::Args;

#[derive(Args)]
pub struct ClearCacheArgs {
    /// Only run garbage collection, not a full cache clear
    #[arg(long)]
    pub gc: bool,
}

pub fn execute(_args: &ClearCacheArgs) {
    todo!()
}
