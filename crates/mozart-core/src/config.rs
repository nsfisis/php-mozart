//! Typed Composer configuration.
//!
//! Mirrors `Composer\Config` from the PHP implementation: holds the merged
//! effective configuration for a project with strongly-typed fields for all
//! known properties.  Unknown properties are captured in the `extra` map so
//! that round-tripping through serde is lossless.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::composer::composer_home;

/// Parse a size string like "300MiB", "1GB", "512k", or a plain integer string
/// into a byte count.  Mirrors Composer's `Config::get('cache-files-maxsize')`.
fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let i = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let num: f64 = s[..i].trim().parse().ok()?;
    let multiplier: f64 = match s[i..].trim().chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('g') => 1024.0 * 1024.0 * 1024.0,
        Some('m') => 1024.0 * 1024.0,
        Some('k') => 1024.0,
        None => 1.0,
        Some(_) => return None,
    };
    Some((num * multiplier).max(0.0) as u64)
}

fn deserialize_size_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::{Error, Visitor};
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a non-negative integer or a size string like \"300MiB\"")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }
        fn visit_i64<E: Error>(self, v: i64) -> Result<u64, E> {
            Ok(v.max(0) as u64)
        }
        fn visit_f64<E: Error>(self, v: f64) -> Result<u64, E> {
            Ok(v.max(0.0) as u64)
        }
        fn visit_str<E: Error>(self, v: &str) -> Result<u64, E> {
            parse_size_bytes(v).ok_or_else(|| E::custom(format!("invalid size: {v}")))
        }
    }
    d.deserialize_any(V)
}

/// Effective Composer configuration for a project.
///
/// Known properties are typed fields; anything else lands in `extra`.
/// `Default::default()` yields Composer's built-in defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Config {
    pub process_timeout: u64,
    pub use_include_path: bool,
    /// Either a single mode string (e.g. `"dist"`) or a per-package map.
    pub preferred_install: serde_json::Value,
    pub notify_on_install: bool,
    pub github_protocols: Vec<String>,
    pub vendor_dir: String,
    pub bin_dir: String,
    pub bin_compat: String,
    pub cache_dir: String,
    pub cache_files_dir: String,
    pub cache_repo_dir: String,
    pub cache_vcs_dir: String,
    pub cache_files_ttl: u64,
    #[serde(deserialize_with = "deserialize_size_bytes")]
    pub cache_files_maxsize: u64,
    pub cache_read_only: bool,
    pub prepend_autoloader: bool,
    pub autoloader_suffix: Option<String>,
    pub optimize_autoloader: bool,
    pub sort_packages: bool,
    pub classmap_authoritative: bool,
    pub apcu_autoloader: bool,
    /// Per-platform package version overrides.
    pub platform: BTreeMap<String, serde_json::Value>,
    /// `true`, `false`, or `"php-only"`.
    pub platform_check: serde_json::Value,
    pub lock: bool,
    /// `true`, `false`, or `"stash"`.
    pub discard_changes: serde_json::Value,
    pub archive_format: String,
    pub archive_dir: String,
    pub htaccess_protect: bool,
    pub secure_http: bool,
    /// `false` (disable all) or a `{plugin: bool}` map.
    pub allow_plugins: serde_json::Value,

    /// Repositories declared at the `composer.json` top level (and merged
    /// from global config), keyed by name. Mirrors
    /// `Composer\Config::$repositories`. Unnamed entries from the list
    /// shape get integer-string keys. Each value is a repository
    /// definition object, or `false` to disable a named repository.
    ///
    /// Sits outside the inner `config: { ... }` serde representation
    /// (Composer keeps it as a sibling property), so it is `#[serde(skip)]`
    /// and preserved across [`Config::merge`] manually.
    #[serde(skip)]
    pub repositories: IndexMap<String, serde_json::Value>,

    /// Catch-all for properties not explicitly listed above.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            process_timeout: 300,
            use_include_path: false,
            preferred_install: serde_json::json!("dist"),
            notify_on_install: true,
            github_protocols: vec!["https".to_string(), "ssh".to_string(), "git".to_string()],
            vendor_dir: "vendor".to_string(),
            bin_dir: "{$vendor-dir}/bin".to_string(),
            bin_compat: "auto".to_string(),
            cache_dir: "{$home}/cache".to_string(),
            cache_files_dir: "{$cache-dir}/files".to_string(),
            cache_repo_dir: "{$cache-dir}/repo".to_string(),
            cache_vcs_dir: "{$cache-dir}/vcs".to_string(),
            cache_files_ttl: 15_552_000,
            cache_files_maxsize: 300 * 1024 * 1024,
            cache_read_only: false,
            prepend_autoloader: true,
            autoloader_suffix: None,
            optimize_autoloader: false,
            sort_packages: false,
            classmap_authoritative: false,
            apcu_autoloader: false,
            platform: BTreeMap::new(),
            platform_check: serde_json::json!("php-only"),
            lock: true,
            discard_changes: serde_json::json!(false),
            archive_format: "tar".to_string(),
            archive_dir: ".".to_string(),
            htaccess_protect: true,
            secure_http: true,
            allow_plugins: serde_json::json!({}),
            repositories: IndexMap::from([(
                "packagist.org".to_string(),
                serde_json::json!({
                    "type": "composer",
                    "url": "https://repo.packagist.org",
                }),
            )]),
            extra: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Merge `overrides` on top of the current values.
    ///
    /// Serialises the current config to a JSON object, applies `overrides`,
    /// then deserialises back.  Known fields are validated by serde; unknown
    /// keys flow into `extra`.
    pub fn merge(&mut self, overrides: &BTreeMap<String, serde_json::Value>) -> anyhow::Result<()> {
        if overrides.is_empty() {
            return Ok(());
        }
        let mut map = match serde_json::to_value(&*self)? {
            serde_json::Value::Object(m) => m,
            _ => unreachable!(),
        };
        for (k, v) in overrides {
            map.insert(k.clone(), v.clone());
        }
        let preserved_repositories = std::mem::take(&mut self.repositories);
        *self = serde_json::from_value(serde_json::Value::Object(map))?;
        self.repositories = preserved_repositories;
        Ok(())
    }

    /// Merge a `repositories` block (from a composer.json or config.json)
    /// into [`Self::repositories`]. Mirrors the repositories branch of
    /// `Composer\Config::merge` (composer/src/Composer/Config.php lines
    /// 243-284):
    ///
    /// - Accepts either a JSON object (name-keyed) or array (positional).
    /// - `false` disables the named repository.
    /// - A single-key `{name: false}` entry also disables the named repo.
    /// - Redefining a `packagist.org`-like composer repo auto-disables the
    ///   default `packagist.org` entry.
    /// - The reverse-merge dance puts new repositories ahead of existing
    ///   ones in priority order.
    /// - Preserves the `packagist` → `packagist.org` BC alias.
    pub fn merge_repositories(&mut self, repos: &serde_json::Value) {
        enum Key {
            Named(String),
            Positional(usize),
        }

        let new_repos: Vec<(Key, serde_json::Value)> = match repos {
            serde_json::Value::Object(obj) => obj
                .iter()
                .map(|(k, v)| (Key::Named(k.clone()), v.clone()))
                .collect(),
            serde_json::Value::Array(arr) => arr
                .iter()
                .enumerate()
                .map(|(i, v)| (Key::Positional(i), v.clone()))
                .collect(),
            _ => return,
        };
        if new_repos.is_empty() {
            return;
        }

        self.repositories.reverse();
        for (key, repo) in new_repos.into_iter().rev() {
            // `false` value → disable by name (only meaningful for named keys)
            if matches!(repo, serde_json::Value::Bool(false)) {
                if let Key::Named(n) = &key {
                    self.disable_repo_by_name(n);
                }
                continue;
            }

            // Single-key `{name: false}` → disable by inner name
            if let serde_json::Value::Object(o) = &repo
                && o.len() == 1
                && let Some((inner_name, inner_val)) = o.iter().next()
                && matches!(inner_val, serde_json::Value::Bool(false))
            {
                self.disable_repo_by_name(inner_name);
                continue;
            }

            // Auto-disable the default packagist.org repo if it gets redefined
            if let serde_json::Value::Object(o) = &repo
                && o.get("type").and_then(|v| v.as_str()) == Some("composer")
                && let Some(url) = o.get("url").and_then(|v| v.as_str())
                && is_packagist_url(url)
            {
                self.disable_repo_by_name("packagist.org");
            }

            match key {
                Key::Positional(i) => {
                    let candidate = i.to_string();
                    let stored_key = if self.repositories.contains_key(&candidate) {
                        self.next_positional_key()
                    } else {
                        candidate
                    };
                    self.repositories.insert(stored_key, repo);
                }
                Key::Named(n) if n == "packagist" => {
                    // BC: legacy `packagist` name maps to `packagist.org`
                    self.repositories.insert("packagist.org".to_string(), repo);
                }
                Key::Named(n) => {
                    self.repositories.insert(n, repo);
                }
            }
        }
        self.repositories.reverse();
    }

    fn disable_repo_by_name(&mut self, name: &str) {
        if self.repositories.shift_remove(name).is_some() {
            return;
        }
        // BC: `packagist` aliases the default `packagist.org` repo
        if name == "packagist" {
            self.repositories.shift_remove("packagist.org");
        }
    }

    fn next_positional_key(&self) -> String {
        let mut max: i64 = -1;
        for k in self.repositories.keys() {
            if let Ok(n) = k.parse::<i64>()
                && n > max
            {
                max = n;
            }
        }
        (max + 1).to_string()
    }

    /// Return the effective value for a single key, or `None` if absent.
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        match key {
            "process-timeout" => Some(serde_json::json!(self.process_timeout)),
            "use-include-path" => Some(serde_json::json!(self.use_include_path)),
            "preferred-install" => Some(self.preferred_install.clone()),
            "notify-on-install" => Some(serde_json::json!(self.notify_on_install)),
            "github-protocols" => Some(serde_json::json!(self.github_protocols)),
            "vendor-dir" => Some(serde_json::json!(self.vendor_dir)),
            "bin-dir" => Some(serde_json::json!(self.bin_dir)),
            "bin-compat" => Some(serde_json::json!(self.bin_compat)),
            "cache-dir" => Some(serde_json::json!(self.cache_dir)),
            "cache-files-dir" => Some(serde_json::json!(self.cache_files_dir)),
            "cache-repo-dir" => Some(serde_json::json!(self.cache_repo_dir)),
            "cache-vcs-dir" => Some(serde_json::json!(self.cache_vcs_dir)),
            "cache-files-ttl" => Some(serde_json::json!(self.cache_files_ttl)),
            "cache-files-maxsize" => Some(serde_json::json!(self.cache_files_maxsize)),
            "cache-read-only" => Some(serde_json::json!(self.cache_read_only)),
            "prepend-autoloader" => Some(serde_json::json!(self.prepend_autoloader)),
            "autoloader-suffix" => Some(match &self.autoloader_suffix {
                Some(s) => serde_json::json!(s),
                None => serde_json::Value::Null,
            }),
            "optimize-autoloader" => Some(serde_json::json!(self.optimize_autoloader)),
            "sort-packages" => Some(serde_json::json!(self.sort_packages)),
            "classmap-authoritative" => Some(serde_json::json!(self.classmap_authoritative)),
            "apcu-autoloader" => Some(serde_json::json!(self.apcu_autoloader)),
            "platform" => Some(serde_json::json!(self.platform)),
            "platform-check" => Some(self.platform_check.clone()),
            "lock" => Some(serde_json::json!(self.lock)),
            "discard-changes" => Some(self.discard_changes.clone()),
            "archive-format" => Some(serde_json::json!(self.archive_format)),
            "archive-dir" => Some(serde_json::json!(self.archive_dir)),
            "htaccess-protect" => Some(serde_json::json!(self.htaccess_protect)),
            "secure-http" => Some(serde_json::json!(self.secure_http)),
            "allow-plugins" => Some(self.allow_plugins.clone()),
            _ => self.extra.get(key).cloned(),
        }
    }

    /// Return all config entries as sorted (key, value) pairs.
    pub fn entries(&self) -> Vec<(String, serde_json::Value)> {
        let mut map: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        map.insert("allow-plugins".to_string(), self.allow_plugins.clone());
        map.insert(
            "apcu-autoloader".to_string(),
            serde_json::json!(self.apcu_autoloader),
        );
        map.insert(
            "archive-dir".to_string(),
            serde_json::json!(self.archive_dir),
        );
        map.insert(
            "archive-format".to_string(),
            serde_json::json!(self.archive_format),
        );
        map.insert(
            "autoloader-suffix".to_string(),
            match &self.autoloader_suffix {
                Some(s) => serde_json::json!(s),
                None => serde_json::Value::Null,
            },
        );
        map.insert("bin-compat".to_string(), serde_json::json!(self.bin_compat));
        map.insert("bin-dir".to_string(), serde_json::json!(self.bin_dir));
        map.insert("cache-dir".to_string(), serde_json::json!(self.cache_dir));
        map.insert(
            "cache-files-dir".to_string(),
            serde_json::json!(self.cache_files_dir),
        );
        map.insert(
            "cache-files-maxsize".to_string(),
            serde_json::json!(self.cache_files_maxsize),
        );
        map.insert(
            "cache-files-ttl".to_string(),
            serde_json::json!(self.cache_files_ttl),
        );
        map.insert(
            "cache-read-only".to_string(),
            serde_json::json!(self.cache_read_only),
        );
        map.insert(
            "cache-repo-dir".to_string(),
            serde_json::json!(self.cache_repo_dir),
        );
        map.insert(
            "cache-vcs-dir".to_string(),
            serde_json::json!(self.cache_vcs_dir),
        );
        map.insert(
            "classmap-authoritative".to_string(),
            serde_json::json!(self.classmap_authoritative),
        );
        map.insert("discard-changes".to_string(), self.discard_changes.clone());
        map.insert(
            "github-protocols".to_string(),
            serde_json::json!(self.github_protocols),
        );
        map.insert(
            "htaccess-protect".to_string(),
            serde_json::json!(self.htaccess_protect),
        );
        map.insert("lock".to_string(), serde_json::json!(self.lock));
        map.insert(
            "notify-on-install".to_string(),
            serde_json::json!(self.notify_on_install),
        );
        map.insert(
            "optimize-autoloader".to_string(),
            serde_json::json!(self.optimize_autoloader),
        );
        map.insert("platform".to_string(), serde_json::json!(self.platform));
        map.insert("platform-check".to_string(), self.platform_check.clone());
        map.insert(
            "prepend-autoloader".to_string(),
            serde_json::json!(self.prepend_autoloader),
        );
        map.insert(
            "preferred-install".to_string(),
            self.preferred_install.clone(),
        );
        map.insert(
            "process-timeout".to_string(),
            serde_json::json!(self.process_timeout),
        );
        map.insert(
            "secure-http".to_string(),
            serde_json::json!(self.secure_http),
        );
        map.insert(
            "sort-packages".to_string(),
            serde_json::json!(self.sort_packages),
        );
        map.insert(
            "use-include-path".to_string(),
            serde_json::json!(self.use_include_path),
        );
        map.insert("vendor-dir".to_string(), serde_json::json!(self.vendor_dir));
        for (k, v) in &self.extra {
            map.insert(k.clone(), v.clone());
        }
        map.into_iter().collect()
    }

    /// Resolve relative *-dir fields to absolute paths by joining with `base`.
    pub fn make_dirs_absolute(&mut self, base: &std::path::Path) {
        fn resolve(base: &std::path::Path, s: &mut String) {
            let p = std::path::Path::new(s.as_str());
            if p.is_relative() {
                *s = base.join(p).to_string_lossy().into_owned();
            }
        }
        resolve(base, &mut self.vendor_dir);
        resolve(base, &mut self.bin_dir);
        resolve(base, &mut self.cache_dir);
        resolve(base, &mut self.cache_files_dir);
        resolve(base, &mut self.cache_repo_dir);
        resolve(base, &mut self.cache_vcs_dir);
        resolve(base, &mut self.archive_dir);
        for (key, val) in &mut self.extra {
            if key.ends_with("-dir")
                && let serde_json::Value::String(s) = val
            {
                resolve(base, s);
            }
        }
    }
}

/// Resolve `{$vendor-dir}`, `{$home}`, and `{$cache-dir}` placeholders in
/// string-valued fields.  Only one pass is performed (no recursive expansion).
pub fn resolve_references(config: &mut Config) {
    let vendor_dir = config.vendor_dir.clone();
    let home = composer_home().to_string_lossy().into_owned();
    let cache_dir = substitute(&config.cache_dir, &vendor_dir, &home, "");

    let resolved_bin_dir = substitute(&config.bin_dir, &vendor_dir, &home, &cache_dir);
    config.bin_dir = resolved_bin_dir;

    let resolved_cache_dir = substitute(&config.cache_dir, &vendor_dir, &home, &cache_dir);
    config.cache_dir = resolved_cache_dir;

    let resolved_cache_files = substitute(&config.cache_files_dir, &vendor_dir, &home, &cache_dir);
    config.cache_files_dir = resolved_cache_files;

    let resolved_cache_repo = substitute(&config.cache_repo_dir, &vendor_dir, &home, &cache_dir);
    config.cache_repo_dir = resolved_cache_repo;

    let resolved_cache_vcs = substitute(&config.cache_vcs_dir, &vendor_dir, &home, &cache_dir);
    config.cache_vcs_dir = resolved_cache_vcs;

    let resolved_archive_dir = substitute(&config.archive_dir, &vendor_dir, &home, &cache_dir);
    config.archive_dir = resolved_archive_dir;

    for val in config.extra.values_mut() {
        if let serde_json::Value::String(s) = val {
            let resolved = substitute(s, &vendor_dir, &home, &cache_dir);
            if resolved != *s {
                *s = resolved;
            }
        }
    }
}

fn substitute(s: &str, vendor_dir: &str, home: &str, cache_dir: &str) -> String {
    s.replace("{$vendor-dir}", vendor_dir)
        .replace("{$home}", home)
        .replace("{$cache-dir}", cache_dir)
}

/// Mirrors Composer's `{^https?://(?:[a-z0-9-.]+\.)?packagist.org(/|$)}`
/// match used to detect a redefinition of the default packagist repo.
fn is_packagist_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let rest = if let Some(s) = lower.strip_prefix("https://") {
        s
    } else if let Some(s) = lower.strip_prefix("http://") {
        s
    } else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    host == "packagist.org" || host.ends_with(".packagist.org")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(c: &Config) -> Vec<&str> {
        c.repositories.keys().map(String::as_str).collect()
    }

    #[test]
    fn default_repositories_holds_packagist_org() {
        let c = Config::default();
        assert_eq!(keys(&c), vec!["packagist.org"]);
        assert_eq!(
            c.repositories.get("packagist.org"),
            Some(&serde_json::json!({
                "type": "composer",
                "url": "https://repo.packagist.org",
            })),
        );
    }

    #[test]
    fn merge_preserves_repositories_across_round_trip() {
        let mut c = Config::default();
        c.merge(&BTreeMap::from([(
            "vendor-dir".to_string(),
            serde_json::json!("deps"),
        )]))
        .unwrap();
        assert_eq!(c.vendor_dir, "deps");
        assert_eq!(keys(&c), vec!["packagist.org"]);
    }

    #[test]
    fn merge_repositories_disable_by_named_false() {
        let mut c = Config::default();
        c.merge_repositories(&serde_json::json!({"packagist.org": false}));
        assert!(c.repositories.is_empty());
    }

    #[test]
    fn merge_repositories_disable_via_packagist_bc_alias() {
        let mut c = Config::default();
        c.merge_repositories(&serde_json::json!({"packagist": false}));
        assert!(c.repositories.is_empty());
    }

    #[test]
    fn merge_repositories_disable_via_anonymous_single_key_false() {
        let mut c = Config::default();
        c.merge_repositories(&serde_json::json!([{"packagist.org": false}]));
        assert!(c.repositories.is_empty());
    }

    #[test]
    fn merge_repositories_packagist_bc_alias_renames_to_packagist_org() {
        let mut c = Config::default();
        c.merge_repositories(&serde_json::json!({
            "packagist": {"type": "composer", "url": "https://example.test"}
        }));
        // BC alias collapses onto the existing packagist.org entry.
        assert_eq!(keys(&c), vec!["packagist.org"]);
        assert_eq!(
            c.repositories.get("packagist.org"),
            Some(&serde_json::json!({"type": "composer", "url": "https://example.test"})),
        );
    }

    #[test]
    fn merge_repositories_redefining_packagist_url_disables_default() {
        let mut c = Config::default();
        c.merge_repositories(&serde_json::json!([
            {"type": "composer", "url": "https://repo.packagist.org"}
        ]));
        // Default packagist.org gone, replaced by the new positional entry.
        assert_eq!(keys(&c), vec!["0"]);
    }

    #[test]
    fn merge_repositories_new_entries_take_priority_over_defaults() {
        let mut c = Config::default();
        c.merge_repositories(&serde_json::json!([
            {"type": "vcs", "url": "https://example.test/a.git"},
            {"type": "vcs", "url": "https://example.test/b.git"},
        ]));
        // New repos appear before the default packagist.org, preserving their
        // original order (priority a > b > packagist.org).
        assert_eq!(keys(&c), vec!["0", "1", "packagist.org"]);
    }

    #[test]
    fn merge_repositories_ignores_non_object_non_array_input() {
        let mut c = Config::default();
        c.merge_repositories(&serde_json::Value::Null);
        c.merge_repositories(&serde_json::json!("ignored"));
        assert_eq!(keys(&c), vec!["packagist.org"]);
    }

    #[test]
    fn is_packagist_url_matches_subdomains_and_paths() {
        assert!(is_packagist_url("https://repo.packagist.org"));
        assert!(is_packagist_url("https://packagist.org/"));
        assert!(is_packagist_url("http://repo.packagist.org/p2/foo.json"));
        assert!(!is_packagist_url("https://example.com"));
        assert!(!is_packagist_url("ftp://packagist.org"));
        assert!(!is_packagist_url("https://notpackagist.org"));
    }
}
