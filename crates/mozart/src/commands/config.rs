use clap::Args;

#[derive(Args)]
pub struct ConfigArgs {
    /// Setting key
    pub setting_key: Option<String>,

    /// Setting value(s)
    pub setting_value: Vec<String>,

    /// Apply to the global config file
    #[arg(short, long)]
    pub global: bool,

    /// Open the config file in an editor
    #[arg(short, long)]
    pub editor: bool,

    /// Affect auth config file
    #[arg(short, long)]
    pub auth: bool,

    /// Unset the given setting key
    #[arg(long)]
    pub unset: bool,

    /// List the current configuration variables
    #[arg(short, long)]
    pub list: bool,

    /// Use a specific config file
    #[arg(short, long)]
    pub file: Option<String>,

    /// Returns absolute paths when fetching *-dir config values
    #[arg(long)]
    pub absolute: bool,

    /// JSON decode the setting value
    #[arg(short, long)]
    pub json: bool,

    /// Merge the setting value with the current value
    #[arg(short, long)]
    pub merge: bool,

    /// Append to existing array values
    #[arg(long)]
    pub append: bool,

    /// Display the origin of a config setting
    #[arg(long)]
    pub source: bool,
}

pub fn execute(_args: &ConfigArgs) {
    todo!()
}
