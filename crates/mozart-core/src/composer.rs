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

// ─── composer_home ────────────────────────────────────────────────────────────

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

// ─── ComposerConfig ───────────────────────────────────────────────────────────

/// Effective Composer config key/value pairs for a project.
/// Keys mirror `Composer\Config`'s defaults; values are stored as raw
/// `serde_json::Value` so callers can re-interpret them per key.
pub struct ComposerConfig {
    pub values: BTreeMap<String, serde_json::Value>,
}

impl ComposerConfig {
    /// Build a `ComposerConfig` populated with Composer's built-in defaults.
    pub fn defaults() -> Self {
        let mut m: BTreeMap<String, serde_json::Value> = BTreeMap::new();

        m.insert("process-timeout".to_string(), serde_json::json!(300));
        m.insert("use-include-path".to_string(), serde_json::json!(false));
        m.insert("preferred-install".to_string(), serde_json::json!("dist"));
        m.insert("notify-on-install".to_string(), serde_json::json!(true));
        m.insert(
            "github-protocols".to_string(),
            serde_json::json!(["https", "ssh", "git"]),
        );
        m.insert("vendor-dir".to_string(), serde_json::json!("vendor"));
        m.insert(
            "bin-dir".to_string(),
            serde_json::json!("{$vendor-dir}/bin"),
        );
        m.insert("bin-compat".to_string(), serde_json::json!("auto"));
        m.insert("cache-dir".to_string(), serde_json::json!("{$home}/cache"));
        m.insert(
            "cache-files-dir".to_string(),
            serde_json::json!("{$cache-dir}/files"),
        );
        m.insert(
            "cache-repo-dir".to_string(),
            serde_json::json!("{$cache-dir}/repo"),
        );
        m.insert(
            "cache-vcs-dir".to_string(),
            serde_json::json!("{$cache-dir}/vcs"),
        );
        m.insert("cache-files-ttl".to_string(), serde_json::json!(15_552_000));
        m.insert(
            "cache-files-maxsize".to_string(),
            serde_json::json!("300MiB"),
        );
        m.insert("cache-read-only".to_string(), serde_json::json!(false));
        m.insert("prepend-autoloader".to_string(), serde_json::json!(true));
        m.insert("autoloader-suffix".to_string(), serde_json::Value::Null);
        m.insert("optimize-autoloader".to_string(), serde_json::json!(false));
        m.insert("sort-packages".to_string(), serde_json::json!(false));
        m.insert(
            "classmap-authoritative".to_string(),
            serde_json::json!(false),
        );
        m.insert("apcu-autoloader".to_string(), serde_json::json!(false));
        m.insert("platform".to_string(), serde_json::json!({}));
        m.insert("platform-check".to_string(), serde_json::json!("php-only"));
        m.insert("lock".to_string(), serde_json::json!(true));
        m.insert("discard-changes".to_string(), serde_json::json!(false));
        m.insert("archive-format".to_string(), serde_json::json!("tar"));
        m.insert("archive-dir".to_string(), serde_json::json!("."));
        m.insert("htaccess-protect".to_string(), serde_json::json!(true));
        m.insert("secure-http".to_string(), serde_json::json!(true));
        m.insert("allow-plugins".to_string(), serde_json::json!({}));

        Self { values: m }
    }

    /// Merge `overrides` on top of the current values.
    pub fn merge(&mut self, overrides: &BTreeMap<String, serde_json::Value>) {
        for (k, v) in overrides {
            self.values.insert(k.clone(), v.clone());
        }
    }

    /// Return the effective value for a single key, or `None` if absent.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.values.get(key)
    }
}

/// Resolve `{$vendor-dir}`, `{$home}`, `{$cache-dir}` placeholders inside
/// string values.  Only one pass is performed (no recursive expansion).
pub fn resolve_references(config: &mut ComposerConfig) {
    // Snapshot the values we need for substitution before mutating.
    let vendor_dir = config
        .values
        .get("vendor-dir")
        .and_then(|v| v.as_str())
        .unwrap_or("vendor")
        .to_string();

    let home = composer_home().to_string_lossy().into_owned();

    let cache_dir = config
        .values
        .get("cache-dir")
        .and_then(|v| v.as_str())
        .unwrap_or("{$home}/cache")
        .replace("{$home}", &home);

    let replacements: &[(&str, &str)] = &[
        ("{$vendor-dir}", &vendor_dir),
        ("{$home}", &home),
        ("{$cache-dir}", &cache_dir),
    ];

    let keys: Vec<String> = config.values.keys().cloned().collect();
    for key in keys {
        if let Some(serde_json::Value::String(s)) = config.values.get(&key).cloned() {
            let mut resolved = s.clone();
            for (placeholder, replacement) in replacements {
                resolved = resolved.replace(placeholder, replacement);
            }
            if resolved != s {
                config
                    .values
                    .insert(key, serde_json::Value::String(resolved));
            }
        }
    }
}

// ─── Composer ────────────────────────────────────────────────────────────────

/// Project-level Composer state. Currently only carries the merged
/// `ComposerConfig`; additional accessors (root package, locker, …) can be
/// layered on as commands need them.
pub struct Composer {
    project_dir: PathBuf,
    config: ComposerConfig,
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
        let mut config = ComposerConfig::defaults();
        if let Some(cfg_obj) = value.get("config").and_then(|v| v.as_object()) {
            let overrides: BTreeMap<String, serde_json::Value> = cfg_obj
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            config.merge(&overrides);
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

    pub fn config(&self) -> &ComposerConfig {
        &self.config
    }
}
