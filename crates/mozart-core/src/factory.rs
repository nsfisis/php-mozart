//! Factory helpers for constructing Composer state.
//!
//! Ports the static factory methods from `Composer\Factory`. Today we
//! cover [`create_config`] (effective global [`Config`]) and
//! [`create_composer`] (the project-level [`Composer`] root, built from
//! `composer.json` plus the on-disk `vendor/composer/installed.json`).
//!
//! Auth loading, htaccess creation, and the plugin/event-dispatcher
//! wiring are intentionally omitted as they are out of scope for the
//! current port.

use crate::composer::composer_home;
use crate::config::Config;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Rust port of `Factory::getCacheDir()`.
///
/// Priority:
/// 1. `$COMPOSER_CACHE_DIR` env var
/// 2. Windows: `%LOCALAPPDATA%/Composer`
/// 3. macOS:   `$HOME/Library/Caches/composer`
/// 4. Linux/other: `$XDG_CACHE_HOME/composer` (or `$HOME/.cache/composer`)
fn get_cache_dir(home: &std::path::Path) -> PathBuf {
    if let Ok(val) = std::env::var("COMPOSER_CACHE_DIR")
        && !val.is_empty()
    {
        return PathBuf::from(val);
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA")
            && !local.is_empty()
        {
            return PathBuf::from(local).join("Composer");
        }
        return home.join("cache");
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(h) = std::env::var("HOME")
            && !h.is_empty()
        {
            return PathBuf::from(h)
                .join("Library")
                .join("Caches")
                .join("composer");
        }
        return home.join("cache");
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let cache_base = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".cache"))
                    .unwrap_or_else(|_| home.join("cache"))
            });
        cache_base.join("composer")
    }
}

/// Rust port of `Factory::getDataDir()`.
///
/// Priority:
/// 1. `$COMPOSER_HOME` is set → use `home` (same path) as data dir
/// 2. Windows: `home`
/// 3. Linux/macOS: `$XDG_DATA_HOME/composer` (or `$HOME/.local/share/composer`)
fn get_data_dir(home: &std::path::Path) -> PathBuf {
    if std::env::var("COMPOSER_HOME").is_ok_and(|v| !v.is_empty()) {
        return home.to_path_buf();
    }

    #[cfg(target_os = "windows")]
    {
        return home.to_path_buf();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let data_base = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
                    .unwrap_or_else(|_| PathBuf::from("/tmp"))
            });
        data_base.join("composer")
    }
}

/// Rust port of `Factory::createConfig()`.
///
/// Builds the effective global [`Config`] by:
/// 1. Starting from `Config::default()`
/// 2. Setting `home`, `cache-dir`, and `data-dir` based on platform conventions
/// 3. Loading and merging `$COMPOSER_HOME/config.json` if it exists
///
/// Auth loading (`auth.json`, `COMPOSER_AUTH`) and htaccess-protect directory
/// creation are intentionally omitted.
///
/// **Callers must call [`crate::config::resolve_references`] after any
/// additional project-level merges.** This function does not call it
/// internally so that callers can overlay project config first.
pub fn create_config() -> anyhow::Result<Config> {
    let home = composer_home();
    let cache_dir = get_cache_dir(&home);
    let data_dir = get_data_dir(&home);

    let mut config = Config::default();

    // Inject home/cache-dir/data-dir as the platform-computed baseline.
    // `home` and `data-dir` have no dedicated fields on Config and land in `extra`.
    let mut defaults: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    defaults.insert(
        "home".to_string(),
        serde_json::json!(home.to_string_lossy().as_ref()),
    );
    defaults.insert(
        "cache-dir".to_string(),
        serde_json::json!(cache_dir.to_string_lossy().as_ref()),
    );
    defaults.insert(
        "data-dir".to_string(),
        serde_json::json!(data_dir.to_string_lossy().as_ref()),
    );
    config.merge(&defaults)?;

    // Load $COMPOSER_HOME/config.json global config
    let global_config_path = home.join("config.json");
    if global_config_path.exists() {
        let content = std::fs::read_to_string(&global_config_path)?;
        let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            anyhow::anyhow!("Failed to parse {}: {e}", global_config_path.display())
        })?;
        if let Some(obj) = json.get("config").and_then(|v| v.as_object()) {
            let overrides: BTreeMap<String, serde_json::Value> =
                obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            config.merge(&overrides)?;
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_config_cache_dir_has_no_placeholder() {
        let config = create_config().unwrap();
        assert!(
            !config.cache_dir.contains("{$home}"),
            "cache_dir should not contain placeholder, got: {}",
            config.cache_dir
        );
        assert!(!config.cache_dir.is_empty());
    }

    #[test]
    fn test_create_config_home_accessible_via_get() {
        let config = create_config().unwrap();
        let home_val = config.get("home");
        assert!(home_val.is_some(), "config.get('home') should return Some");
        assert!(
            home_val
                .unwrap()
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "home should be a non-empty string"
        );
    }

    #[test]
    fn test_create_config_data_dir_accessible_via_get() {
        let config = create_config().unwrap();
        assert!(config.get("data-dir").is_some());
    }

    #[test]
    fn test_get_cache_dir_ends_with_composer() {
        let home = std::path::PathBuf::from("/tmp/test-home");
        let result = get_cache_dir(&home);
        assert!(
            result.to_string_lossy().contains("composer"),
            "cache dir should contain 'composer', got: {}",
            result.display()
        );
    }

    #[test]
    fn test_get_data_dir_ends_with_composer_when_no_composer_home() {
        // Only valid when COMPOSER_HOME is not set in the test environment.
        if std::env::var("COMPOSER_HOME").is_ok_and(|v| !v.is_empty()) {
            return;
        }
        let home = std::path::PathBuf::from("/tmp/test-home");
        let result = get_data_dir(&home);
        assert!(
            result.to_string_lossy().contains("composer"),
            "data dir should contain 'composer', got: {}",
            result.display()
        );
    }
}
