use anyhow::anyhow;
use clap::Args;
use std::collections::BTreeMap;
use std::path::PathBuf;

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

// ─── ComposerConfig ───────────────────────────────────────────────────────────

/// Holds the effective configuration key-value pairs for a project.
/// Keys mirror Composer's `Config.php` defaults.
pub struct ComposerConfig {
    pub values: BTreeMap<String, serde_json::Value>,
}

impl ComposerConfig {
    /// Build a `ComposerConfig` starting from the built-in defaults.
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

    /// Resolve `{$vendor-dir}`, `{$home}`, `{$cache-dir}` placeholders inside
    /// string values.  Only one pass is performed (no recursive expansion).
    pub fn resolve_references(&mut self) {
        // Snapshot the values we need for substitution before mutating.
        let vendor_dir = self
            .values
            .get("vendor-dir")
            .and_then(|v| v.as_str())
            .unwrap_or("vendor")
            .to_string();

        let home = composer_home();

        let cache_dir = self
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

        let keys: Vec<String> = self.values.keys().cloned().collect();
        for key in keys {
            if let Some(serde_json::Value::String(s)) = self.values.get(&key).cloned() {
                let mut resolved = s.clone();
                for (placeholder, replacement) in replacements {
                    resolved = resolved.replace(placeholder, replacement);
                }
                if resolved != s {
                    self.values.insert(key, serde_json::Value::String(resolved));
                }
            }
        }
    }

    /// Return the effective value for a single key, or `None` if absent.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.values.get(key)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Return the Composer home directory, respecting `COMPOSER_HOME` and
/// falling back to the platform default (`~/.config/composer` on Unix,
/// `%APPDATA%/Composer` on Windows).
fn composer_home() -> String {
    if let Ok(home) = std::env::var("COMPOSER_HOME") {
        return home;
    }

    // Platform-specific defaults
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .map(|p| format!("{p}/Composer"))
            .unwrap_or_else(|_| "C:/ProgramData/ComposerSetup/bin".to_string())
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Prefer XDG_CONFIG_HOME if set, otherwise fall back to ~/.config/composer
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            format!("{xdg}/composer")
        } else {
            std::env::var("HOME")
                .map(|h| format!("{h}/.config/composer"))
                .unwrap_or_else(|_| "/tmp/composer".to_string())
        }
    }
}

/// Load the `config` section from a JSON file (global `config.json` or local
/// `composer.json`).  Returns an empty map when the file is absent or has no
/// `config` key.
fn load_config_section(
    path: &std::path::Path,
) -> anyhow::Result<BTreeMap<String, serde_json::Value>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let content = std::fs::read_to_string(path)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;

    match json.get("config") {
        Some(serde_json::Value::Object(obj)) => {
            Ok(obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        }
        _ => Ok(BTreeMap::new()),
    }
}

/// Build the working directory path, preferring `--working-dir` over `cwd`.
fn working_dir(cli: &super::Cli) -> anyhow::Result<PathBuf> {
    match &cli.working_dir {
        Some(d) => Ok(PathBuf::from(d)),
        None => Ok(std::env::current_dir()?),
    }
}

// ─── Value rendering ─────────────────────────────────────────────────────────

/// Render a `serde_json::Value` as a human-readable string suitable for
/// single-line display (matching Composer's behaviour).
fn render_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            arr.iter().map(render_value).collect::<Vec<_>>().join(", ")
        }
        serde_json::Value::Object(obj) => {
            if obj.is_empty() {
                "{}".to_string()
            } else {
                serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())
            }
        }
    }
}

// ─── execute() ───────────────────────────────────────────────────────────────

pub fn execute(args: &ConfigArgs, cli: &super::Cli) -> anyhow::Result<()> {
    // Write-mode operations are not yet implemented.
    let is_write = !args.setting_value.is_empty() || args.unset || args.editor;
    if is_write {
        anyhow::bail!("Write-mode config operations are not yet implemented");
    }

    // Build the effective config.
    let mut config = ComposerConfig::defaults();

    if args.global {
        // Read from $COMPOSER_HOME/config.json
        let global_config_path = PathBuf::from(composer_home()).join("config.json");
        let overrides = load_config_section(&global_config_path)?;
        config.merge(&overrides);
    } else {
        // Read from working_dir/composer.json (config section only).
        let wd = working_dir(cli)?;
        let composer_json = wd.join("composer.json");
        let overrides = load_config_section(&composer_json)?;
        config.merge(&overrides);
    }

    // Resolve {$placeholder} references in string values.
    config.resolve_references();

    if args.list {
        // Print all key → value pairs.
        for (key, value) in &config.values {
            println!("[{}] {}", key, render_value(value));
        }
        return Ok(());
    }

    match &args.setting_key {
        None => {
            // No key and not --list: show a short usage hint (mirrors Composer).
            eprintln!(
                "{}",
                crate::console::error(
                    "No command specified. Use --list to show all config values, \
                     or provide a setting key."
                )
            );
            std::process::exit(1);
        }
        Some(key) => match config.get(key) {
            Some(value) => {
                println!("{}", render_value(value));
            }
            None => {
                return Err(anyhow!("Setting \"{}\" does not exist.", key));
            }
        },
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── defaults ───────────────────────────────────────────────────────────

    #[test]
    fn test_defaults_contain_expected_keys() {
        let cfg = ComposerConfig::defaults();

        let required_keys = [
            "process-timeout",
            "use-include-path",
            "preferred-install",
            "notify-on-install",
            "github-protocols",
            "vendor-dir",
            "bin-dir",
            "bin-compat",
            "cache-dir",
            "cache-files-dir",
            "cache-repo-dir",
            "cache-vcs-dir",
            "cache-files-ttl",
            "cache-files-maxsize",
            "cache-read-only",
            "prepend-autoloader",
            "autoloader-suffix",
            "optimize-autoloader",
            "sort-packages",
            "classmap-authoritative",
            "apcu-autoloader",
            "platform",
            "platform-check",
            "lock",
            "discard-changes",
            "archive-format",
            "archive-dir",
            "htaccess-protect",
            "secure-http",
            "allow-plugins",
        ];

        for key in &required_keys {
            assert!(cfg.values.contains_key(*key), "defaults missing key: {key}");
        }
    }

    #[test]
    fn test_defaults_values_correct() {
        let cfg = ComposerConfig::defaults();

        assert_eq!(cfg.values["process-timeout"], serde_json::json!(300));
        assert_eq!(cfg.values["preferred-install"], serde_json::json!("dist"));
        assert_eq!(cfg.values["vendor-dir"], serde_json::json!("vendor"));
        assert_eq!(
            cfg.values["github-protocols"],
            serde_json::json!(["https", "ssh", "git"])
        );
        assert_eq!(cfg.values["secure-http"], serde_json::json!(true));
        assert_eq!(cfg.values["lock"], serde_json::json!(true));
        assert_eq!(cfg.values["autoloader-suffix"], serde_json::Value::Null);
    }

    // ── merge ──────────────────────────────────────────────────────────────

    #[test]
    fn test_merge_overrides_existing_key() {
        let mut cfg = ComposerConfig::defaults();

        let mut overrides = BTreeMap::new();
        overrides.insert("vendor-dir".to_string(), serde_json::json!("packages"));
        overrides.insert("sort-packages".to_string(), serde_json::json!(true));

        cfg.merge(&overrides);

        assert_eq!(cfg.values["vendor-dir"], serde_json::json!("packages"));
        assert_eq!(cfg.values["sort-packages"], serde_json::json!(true));
    }

    #[test]
    fn test_merge_adds_new_key() {
        let mut cfg = ComposerConfig::defaults();

        let mut overrides = BTreeMap::new();
        overrides.insert("custom-key".to_string(), serde_json::json!("custom-value"));

        cfg.merge(&overrides);

        assert_eq!(cfg.values["custom-key"], serde_json::json!("custom-value"));
    }

    #[test]
    fn test_merge_empty_overrides_leaves_defaults_intact() {
        let mut cfg = ComposerConfig::defaults();
        let original_vendor = cfg.values["vendor-dir"].clone();

        cfg.merge(&BTreeMap::new());

        assert_eq!(cfg.values["vendor-dir"], original_vendor);
    }

    // ── reference resolution ───────────────────────────────────────────────

    #[test]
    fn test_reference_resolution_bin_dir() {
        let mut cfg = ComposerConfig::defaults();
        // bin-dir default is "{$vendor-dir}/bin"; vendor-dir default is "vendor"
        cfg.resolve_references();

        assert_eq!(cfg.values["bin-dir"], serde_json::json!("vendor/bin"));
    }

    #[test]
    fn test_reference_resolution_custom_vendor_dir() {
        let mut cfg = ComposerConfig::defaults();

        // Override vendor-dir before resolving
        cfg.values
            .insert("vendor-dir".to_string(), serde_json::json!("lib"));
        cfg.resolve_references();

        assert_eq!(cfg.values["bin-dir"], serde_json::json!("lib/bin"));
    }

    #[test]
    fn test_reference_resolution_cache_dirs() {
        let mut cfg = ComposerConfig::defaults();
        // Inject a predictable home so the test is environment-independent.
        cfg.values.insert(
            "cache-dir".to_string(),
            serde_json::json!("/home/user/.cache/composer"),
        );
        cfg.resolve_references();

        assert_eq!(
            cfg.values["cache-files-dir"],
            serde_json::json!("/home/user/.cache/composer/files")
        );
        assert_eq!(
            cfg.values["cache-repo-dir"],
            serde_json::json!("/home/user/.cache/composer/repo")
        );
        assert_eq!(
            cfg.values["cache-vcs-dir"],
            serde_json::json!("/home/user/.cache/composer/vcs")
        );
    }

    #[test]
    fn test_reference_resolution_no_change_for_non_string() {
        let mut cfg = ComposerConfig::defaults();
        let before = cfg.values["process-timeout"].clone();
        cfg.resolve_references();
        // Numeric values should be untouched.
        assert_eq!(cfg.values["process-timeout"], before);
    }

    // ── single key query ───────────────────────────────────────────────────

    #[test]
    fn test_get_existing_key() {
        let cfg = ComposerConfig::defaults();
        let value = cfg.get("vendor-dir");
        assert!(value.is_some());
        assert_eq!(value.unwrap(), &serde_json::json!("vendor"));
    }

    #[test]
    fn test_get_nonexistent_key_returns_none() {
        let cfg = ComposerConfig::defaults();
        assert!(cfg.get("does-not-exist").is_none());
    }

    // ── render_value ───────────────────────────────────────────────────────

    #[test]
    fn test_render_value_string() {
        assert_eq!(render_value(&serde_json::json!("hello")), "hello");
    }

    #[test]
    fn test_render_value_bool() {
        assert_eq!(render_value(&serde_json::json!(true)), "true");
        assert_eq!(render_value(&serde_json::json!(false)), "false");
    }

    #[test]
    fn test_render_value_number() {
        assert_eq!(render_value(&serde_json::json!(300)), "300");
    }

    #[test]
    fn test_render_value_null() {
        assert_eq!(render_value(&serde_json::Value::Null), "NULL");
    }

    #[test]
    fn test_render_value_array() {
        let v = serde_json::json!(["https", "ssh", "git"]);
        assert_eq!(render_value(&v), "https, ssh, git");
    }

    #[test]
    fn test_render_value_empty_object() {
        assert_eq!(render_value(&serde_json::json!({})), "{}");
    }

    // ── load_config_section ────────────────────────────────────────────────

    #[test]
    fn test_load_config_section_absent_file() {
        let path = std::path::Path::new("/tmp/nonexistent_composer_abc123.json");
        let result = load_config_section(path).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_config_section_with_config_key() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"{{"name":"test/pkg","config":{{"sort-packages":true,"vendor-dir":"packages"}}}}"#
        )
        .unwrap();

        let result = load_config_section(f.path()).unwrap();
        assert_eq!(result.get("sort-packages"), Some(&serde_json::json!(true)));
        assert_eq!(
            result.get("vendor-dir"),
            Some(&serde_json::json!("packages"))
        );
    }

    #[test]
    fn test_load_config_section_missing_config_key() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        write!(f, r#"{{"name":"test/pkg","require":{{}}}}"#).unwrap();

        let result = load_config_section(f.path()).unwrap();
        assert!(result.is_empty());
    }

    // ── full merge pipeline ────────────────────────────────────────────────

    #[test]
    fn test_full_pipeline_project_overrides_are_applied() {
        use std::io::Write;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let composer_json = dir.path().join("composer.json");
        let mut f = std::fs::File::create(&composer_json).unwrap();
        write!(
            f,
            r#"{{"name":"test/pkg","config":{{"vendor-dir":"custom_vendor","sort-packages":true}}}}"#
        )
        .unwrap();

        let overrides = load_config_section(&composer_json).unwrap();
        let mut cfg = ComposerConfig::defaults();
        cfg.merge(&overrides);
        cfg.resolve_references();

        assert_eq!(cfg.values["vendor-dir"], serde_json::json!("custom_vendor"));
        assert_eq!(cfg.values["sort-packages"], serde_json::json!(true));
        // bin-dir should have resolved against the overridden vendor-dir
        assert_eq!(
            cfg.values["bin-dir"],
            serde_json::json!("custom_vendor/bin")
        );
    }
}
