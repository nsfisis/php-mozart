use mozart_core::composer::composer_home;
use mozart_core::config_source::JsonConfigSource;
use std::path::PathBuf;

/// Mirrors Composer's `BaseConfigCommand`: resolves the target config file path
/// and enforces the `--file` ↔ `--global` mutual exclusivity.
pub(crate) struct BaseConfigContext {
    pub config_source: JsonConfigSource,
}

impl BaseConfigContext {
    pub fn initialize(global: bool, file: Option<&str>, cli: &super::Cli) -> anyhow::Result<Self> {
        if global && file.is_some() {
            anyhow::bail!("--file and --global can not be combined");
        }

        let path: PathBuf = if global {
            composer_home().join("config.json")
        } else if let Some(f) = file {
            PathBuf::from(f)
        } else {
            cli.working_dir()?.join("composer.json")
        };

        Ok(Self {
            config_source: JsonConfigSource::new(path, false),
        })
    }
}
