//! Composer-equivalent root state: composer.json + effective config.
//!
//! Mirrors the role of `Composer\Composer` (PHP) to the extent that command
//! handlers need today: a single struct loaded from the project directory,
//! exposing a `config()` accessor over the merged Composer config.
//!
//! See `Composer\Command\BaseCommand::requireComposer()` /
//! `Composer\Command\BaseCommand::tryComposer()` for the upstream contract
//! that [`Composer::require`] and [`Composer::try_load`] are modelled on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, resolve_references};

/// Return the Composer home directory, respecting `COMPOSER_HOME` and falling
/// back to the platform default using Composer-compatible logic.
///
/// On Unix:
/// - If XDG is in use (any `XDG_*` env var exists, or `/etc/xdg` exists),
///   prefer `$XDG_CONFIG_HOME/composer` (or `$HOME/.config/composer`).
/// - Always include `$HOME/.composer` as a fallback candidate.
/// - Return the first candidate directory that exists on disk;
///   if none exist, return the first candidate.
pub fn composer_home() -> PathBuf {
    if let Ok(val) = std::env::var("COMPOSER_HOME")
        && !val.is_empty()
    {
        return PathBuf::from(val);
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.is_empty()
        {
            return PathBuf::from(appdata).join("Composer");
        }
        return PathBuf::from("C:/ProgramData/ComposerSetup/bin");
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));

        let mut candidates: Vec<PathBuf> = Vec::new();

        if use_xdg() {
            let xdg_config = std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home_dir.join(".config"));
            candidates.push(xdg_config.join("composer"));
        }

        candidates.push(home_dir.join(".composer"));

        // Return first candidate that exists; otherwise return the first
        candidates
            .iter()
            .find(|p| p.is_dir())
            .cloned()
            .unwrap_or_else(|| candidates.into_iter().next().unwrap())
    }
}

#[cfg(not(target_os = "windows"))]
fn use_xdg() -> bool {
    std::env::vars().any(|(k, _)| k.starts_with("XDG_"))
        || std::path::Path::new("/etc/xdg").is_dir()
}

/// Project-level Composer state. Currently only carries the merged
/// [`Config`]; additional accessors (root package, locker, …) can be
/// layered on as commands need them.
pub struct Composer {
    project_dir: PathBuf,
    config: Config,
}

impl Composer {
    /// Load Composer state for `project_dir`, requiring a composer.json.
    /// Mirrors `BaseCommand::requireComposer()`.
    pub fn require(project_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let project_dir = project_dir.into();
        let composer_json = project_dir.join("composer.json");
        if !composer_json.exists() {
            anyhow::bail!(
                "Composer could not find a composer.json file in {}",
                project_dir.display()
            );
        }
        Self::load(project_dir, &composer_json)
    }

    /// Load Composer state for `project_dir`, returning `None` if no
    /// composer.json exists. Other I/O or parse errors still propagate.
    /// Mirrors `BaseCommand::tryComposer()`.
    pub fn try_load(project_dir: impl Into<PathBuf>) -> anyhow::Result<Option<Self>> {
        let project_dir = project_dir.into();
        let composer_json = project_dir.join("composer.json");
        if !composer_json.exists() {
            return Ok(None);
        }
        Self::load(project_dir, &composer_json).map(Some)
    }

    fn load(project_dir: PathBuf, composer_json: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(composer_json)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        let mut config = Config::default();
        if let Some(cfg_obj) = value.get("config").and_then(|v| v.as_object()) {
            let overrides: BTreeMap<String, serde_json::Value> = cfg_obj
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            config.merge(&overrides)?;
        }
        resolve_references(&mut config);
        Ok(Self {
            project_dir,
            config,
        })
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
}
