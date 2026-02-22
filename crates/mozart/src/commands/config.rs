use anyhow::anyhow;
use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::config_helpers::{
    add_repository, composer_home, read_json_file, remove_repository, render_value, working_dir,
    write_json_file,
};

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

// ─── ConfigValueType ─────────────────────────────────────────────────────────

/// Classification of config key value types for validation and normalization.
#[derive(Debug)]
enum ConfigValueType {
    /// Single boolean value (true/false/1/0)
    Bool,
    /// Single integer value
    Integer,
    /// Single string value (any string accepted)
    Str,
    /// One of a fixed set of string values
    Enum(&'static [&'static str]),
    /// Special: bool or a specific string (e.g. "stash" for discard-changes)
    BoolOrEnum(&'static [&'static str]),
    /// Multi-value: array of strings
    StringArray,
    /// Multi-value: array from a fixed set
    EnumArray(&'static [&'static str]),
}

/// Return the type descriptor for a known top-level config key.
/// Returns `None` for unknown keys.
fn config_value_type(key: &str) -> Option<ConfigValueType> {
    match key {
        "process-timeout" => Some(ConfigValueType::Integer),
        "use-include-path" => Some(ConfigValueType::Bool),
        "preferred-install" => Some(ConfigValueType::Enum(&["auto", "source", "dist"])),
        "notify-on-install" => Some(ConfigValueType::Bool),
        "vendor-dir" => Some(ConfigValueType::Str),
        "bin-dir" => Some(ConfigValueType::Str),
        "archive-dir" => Some(ConfigValueType::Str),
        "archive-format" => Some(ConfigValueType::Str),
        "cache-dir" => Some(ConfigValueType::Str),
        "cache-files-dir" => Some(ConfigValueType::Str),
        "cache-repo-dir" => Some(ConfigValueType::Str),
        "cache-vcs-dir" => Some(ConfigValueType::Str),
        "cache-files-ttl" => Some(ConfigValueType::Integer),
        "cache-files-maxsize" => Some(ConfigValueType::Str),
        "cache-read-only" => Some(ConfigValueType::Bool),
        "cache-ttl" => Some(ConfigValueType::Integer),
        "bin-compat" => Some(ConfigValueType::Enum(&["auto", "full", "proxy", "symlink"])),
        "discard-changes" => Some(ConfigValueType::BoolOrEnum(&["stash"])),
        "autoloader-suffix" => Some(ConfigValueType::Str),
        "sort-packages" => Some(ConfigValueType::Bool),
        "optimize-autoloader" => Some(ConfigValueType::Bool),
        "classmap-authoritative" => Some(ConfigValueType::Bool),
        "apcu-autoloader" => Some(ConfigValueType::Bool),
        "prepend-autoloader" => Some(ConfigValueType::Bool),
        "secure-http" => Some(ConfigValueType::Bool),
        "htaccess-protect" => Some(ConfigValueType::Bool),
        "lock" => Some(ConfigValueType::Bool),
        "allow-plugins" => Some(ConfigValueType::Bool),
        "platform-check" => Some(ConfigValueType::BoolOrEnum(&["php-only"])),
        "github-protocols" => Some(ConfigValueType::EnumArray(&["git", "https", "ssh"])),
        "github-domains" => Some(ConfigValueType::StringArray),
        "gitlab-domains" => Some(ConfigValueType::StringArray),
        "use-github-api" => Some(ConfigValueType::Bool),
        "update-with-minimal-changes" => Some(ConfigValueType::Bool),
        "disable-tls" => Some(ConfigValueType::Bool),
        "github-expose-hostname" => Some(ConfigValueType::Bool),
        "data-dir" => Some(ConfigValueType::Str),
        "cafile" => Some(ConfigValueType::Str),
        "capath" => Some(ConfigValueType::Str),
        "gitlab-protocol" => Some(ConfigValueType::Enum(&["git", "http", "https"])),
        // store-auths accepts true/false/prompt
        "store-auths" => Some(ConfigValueType::BoolOrEnum(&["prompt"])),
        // bump-after-update accepts true/false/dev/no-dev
        "bump-after-update" => Some(ConfigValueType::BoolOrEnum(&["dev", "no-dev"])),
        // use-parent-dir accepts true/false/prompt
        "use-parent-dir" => Some(ConfigValueType::BoolOrEnum(&["prompt"])),
        "audit.abandoned" => Some(ConfigValueType::Enum(&["ignore", "report", "fail"])),
        "audit.ignore-unreachable" => Some(ConfigValueType::Bool),
        "audit.block-insecure" => Some(ConfigValueType::Bool),
        "audit.block-abandoned" => Some(ConfigValueType::Bool),
        "audit.ignore-severity" => Some(ConfigValueType::EnumArray(&[
            "low", "medium", "high", "critical",
        ])),
        _ => None,
    }
}

/// Package properties that can be set/unset via the config command.
const CONFIGURABLE_PACKAGE_PROPERTIES: &[&str] = &[
    "name",
    "type",
    "description",
    "homepage",
    "version",
    "minimum-stability",
    "prefer-stable",
    "keywords",
    "license",
];

/// Return the type descriptor for a known package property key.
fn package_property_type(key: &str) -> Option<ConfigValueType> {
    match key {
        "name" => Some(ConfigValueType::Str),
        "type" => Some(ConfigValueType::Str),
        "description" => Some(ConfigValueType::Str),
        "homepage" => Some(ConfigValueType::Str),
        "version" => Some(ConfigValueType::Str),
        "minimum-stability" => Some(ConfigValueType::Enum(&[
            "stable", "rc", "beta", "alpha", "dev",
        ])),
        "prefer-stable" => Some(ConfigValueType::Bool),
        "keywords" => Some(ConfigValueType::StringArray),
        "license" => Some(ConfigValueType::StringArray),
        _ => None,
    }
}

/// Validate and normalize a single string value against its type descriptor.
/// Returns `Ok(normalized_json_value)` or an error.
fn validate_and_normalize(
    key: &str,
    value: &str,
    vtype: &ConfigValueType,
) -> anyhow::Result<serde_json::Value> {
    match vtype {
        ConfigValueType::Bool => normalize_bool(key, value),
        ConfigValueType::Integer => {
            let n: i64 = value
                .parse()
                .map_err(|_| anyhow!("Expected an integer for \"{key}\", got \"{value}\""))?;
            Ok(serde_json::json!(n))
        }
        ConfigValueType::Str => {
            // Special case: "null" → JSON null for autoloader-suffix
            if key == "autoloader-suffix" && value == "null" {
                return Ok(serde_json::Value::Null);
            }
            Ok(serde_json::json!(value))
        }
        ConfigValueType::Enum(variants) => {
            let lower = value.to_lowercase();
            if variants.contains(&lower.as_str()) {
                Ok(serde_json::json!(lower))
            } else {
                Err(anyhow!(
                    "Invalid value \"{value}\" for \"{key}\". Must be one of: {}",
                    variants.join(", ")
                ))
            }
        }
        ConfigValueType::BoolOrEnum(variants) => {
            // Try bool first
            if let Ok(b) = normalize_bool(key, value) {
                return Ok(b);
            }
            // Then try enum
            let lower = value.to_lowercase();
            if variants.contains(&lower.as_str()) {
                Ok(serde_json::json!(lower))
            } else {
                Err(anyhow!(
                    "Invalid value \"{value}\" for \"{key}\". Must be a boolean or one of: {}",
                    variants.join(", ")
                ))
            }
        }
        ConfigValueType::StringArray | ConfigValueType::EnumArray(_) => {
            // validate_and_normalize_multi should be used for these
            Err(anyhow!(
                "\"{key}\" is a multi-value setting. Provide one or more values."
            ))
        }
    }
}

/// Validate and normalize multiple string values against a multi-value type.
/// Returns `Ok(normalized_json_array)` or an error.
fn validate_and_normalize_multi(
    key: &str,
    values: &[String],
    vtype: &ConfigValueType,
) -> anyhow::Result<serde_json::Value> {
    match vtype {
        ConfigValueType::StringArray => {
            let arr: Vec<serde_json::Value> = values.iter().map(|v| serde_json::json!(v)).collect();
            Ok(serde_json::Value::Array(arr))
        }
        ConfigValueType::EnumArray(variants) => {
            let mut arr = Vec::new();
            for v in values {
                let lower = v.to_lowercase();
                if variants.contains(&lower.as_str()) {
                    arr.push(serde_json::json!(lower));
                } else {
                    return Err(anyhow!(
                        "Invalid value \"{v}\" for \"{key}\". Must be one of: {}",
                        variants.join(", ")
                    ));
                }
            }
            Ok(serde_json::Value::Array(arr))
        }
        _ => Err(anyhow!("\"{key}\" is not a multi-value setting.")),
    }
}

/// Normalize a boolean string value to a JSON bool.
fn normalize_bool(key: &str, value: &str) -> anyhow::Result<serde_json::Value> {
    match value.to_lowercase().as_str() {
        "true" | "1" => Ok(serde_json::json!(true)),
        "false" | "0" => Ok(serde_json::json!(false)),
        _ => Err(anyhow!(
            "Expected a boolean (true/false/1/0) for \"{key}\", got \"{value}\""
        )),
    }
}

// ─── Repository helpers ───────────────────────────────────────────────────────

/// Match `repo.X`, `repos.X`, `repositories.X` and return the suffix X.
fn match_repository_key(key: &str) -> Option<&str> {
    for prefix in &["repositories.", "repos.", "repo."] {
        if let Some(suffix) = key.strip_prefix(prefix)
            && !suffix.is_empty()
        {
            return Some(suffix);
        }
    }
    None
}

// ─── JSON path helpers ────────────────────────────────────────────────────────

/// Set a value at a dot-separated path within a JSON Value.
/// Creates intermediate objects as needed.
fn json_set_nested(root: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let parts: Vec<&str> = path.splitn(2, '.').collect();
    if parts.len() == 1 {
        if let Some(obj) = root.as_object_mut() {
            obj.insert(parts[0].to_string(), value);
        }
    } else {
        let key = parts[0];
        let rest = parts[1];
        // Ensure root is an object and the key exists as an object
        if let Some(obj) = root.as_object_mut() {
            if !obj.contains_key(key) || !obj[key].is_object() {
                obj.insert(key.to_string(), serde_json::json!({}));
            }
            if let Some(child) = obj.get_mut(key) {
                json_set_nested(child, rest, value);
            }
        }
    }
}

/// Remove a value at a dot-separated path within a JSON Value.
/// Returns true if the value was found and removed.
fn json_remove_nested(root: &mut serde_json::Value, path: &str) -> bool {
    let parts: Vec<&str> = path.splitn(2, '.').collect();
    if parts.len() == 1 {
        if let Some(obj) = root.as_object_mut() {
            return obj.remove(parts[0]).is_some();
        }
        false
    } else {
        let key = parts[0];
        let rest = parts[1];
        if let Some(obj) = root.as_object_mut()
            && let Some(child) = obj.get_mut(key)
        {
            return json_remove_nested(child, rest);
        }
        false
    }
}

// ─── File I/O helpers ─────────────────────────────────────────────────────────

/// Determine which JSON file to read/write.
/// - `--global` → `$COMPOSER_HOME/config.json`
/// - `--file <path>` → user-specified file
/// - default → `<working_dir>/composer.json`
fn resolve_config_file_path(args: &ConfigArgs, cli: &super::Cli) -> anyhow::Result<PathBuf> {
    if args.global && args.file.is_some() {
        anyhow::bail!("Cannot combine --global and --file");
    }
    if args.global {
        return Ok(PathBuf::from(composer_home()).join("config.json"));
    }
    if let Some(ref file) = args.file {
        return Ok(PathBuf::from(file));
    }
    Ok(working_dir(cli)?.join("composer.json"))
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

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

// ─── Value rendering ─────────────────────────────────────────────────────────

// ─── execute() ───────────────────────────────────────────────────────────────

pub async fn execute(
    args: &ConfigArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // 1. Handle --editor mode
    if args.editor {
        return execute_editor(args, cli);
    }

    // 2. Determine file path
    let config_file_path = resolve_config_file_path(args, cli)?;

    // 3. Detect write vs read mode
    let is_write = !args.setting_value.is_empty() || args.unset;

    if is_write {
        // 4a. Validate: cannot combine --unset with setting values
        if args.unset && !args.setting_value.is_empty() {
            anyhow::bail!("You cannot combine a setting value with --unset");
        }
        return execute_write(args, cli, &config_file_path);
    }

    // 4b. Read mode
    execute_read(args, cli, &config_file_path)
}

// ─── execute_editor() ────────────────────────────────────────────────────────

fn execute_editor(args: &ConfigArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let file_path = resolve_config_file_path(args, cli)?;

    #[cfg(target_os = "windows")]
    let default_editor = "notepad";
    #[cfg(not(target_os = "windows"))]
    let default_editor = "vi";

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| default_editor.to_string());

    let status = std::process::Command::new(&editor)
        .arg(&file_path)
        .status()
        .map_err(|e| anyhow!("Failed to launch editor \"{editor}\": {e}"))?;

    if !status.success() {
        anyhow::bail!(
            "Editor \"{editor}\" exited with non-zero status: {}",
            status.code().unwrap_or(-1)
        );
    }

    Ok(())
}

// ─── execute_write() ─────────────────────────────────────────────────────────

fn execute_write(
    args: &ConfigArgs,
    _cli: &super::Cli,
    config_file_path: &Path,
) -> anyhow::Result<()> {
    let key = args
        .setting_key
        .as_ref()
        .ok_or_else(|| anyhow!("A setting key is required for write operations"))?;
    let values = &args.setting_value;

    let mut json = read_json_file(config_file_path, args.global)?;

    if args.unset {
        execute_unset(&mut json, key, args)?;
    } else {
        execute_set(&mut json, key, values, args)?;
    }

    write_json_file(config_file_path, &json)?;
    Ok(())
}

// ─── execute_unset() ─────────────────────────────────────────────────────────

fn execute_unset(json: &mut serde_json::Value, key: &str, args: &ConfigArgs) -> anyhow::Result<()> {
    // 1. Repository key
    if let Some(repo_name) = match_repository_key(key) {
        remove_repository(json, repo_name);
        // If repositories array is empty, remove the key entirely
        if json["repositories"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false)
            && let Some(obj) = json.as_object_mut()
        {
            obj.remove("repositories");
        }
        return Ok(());
    }

    // 2. Dotted config subkeys: preferred-install.X, allow-plugins.X, platform.X
    if let Some((base, sub)) = split_dotted_config_key(key) {
        let path = format!("config.{base}.{sub}");
        json_remove_nested(json, &path);
        return Ok(());
    }

    // 3. Known top-level config key
    if config_value_type(key).is_some() {
        json_remove_nested(json, &format!("config.{key}"));
        return Ok(());
    }

    // 4. Package property
    if CONFIGURABLE_PACKAGE_PROPERTIES.contains(&key) {
        if args.global {
            anyhow::bail!("Package property \"{key}\" cannot be unset in the global config");
        }
        if let Some(obj) = json.as_object_mut() {
            obj.remove(key);
        }
        return Ok(());
    }

    // 5. Extra dot-path (extra.X or suggest.X)
    if key.starts_with("extra.") || key.starts_with("suggest.") {
        json_remove_nested(json, key);
        return Ok(());
    }

    Err(anyhow!(
        "Setting \"{key}\" does not exist or is not supported"
    ))
}

/// Split a dotted config subkey like `preferred-install.vendor/*` into
/// `("preferred-install", "vendor/*")` for the supported dotted config keys.
fn split_dotted_config_key(key: &str) -> Option<(&str, &str)> {
    for base in &["preferred-install", "allow-plugins", "platform"] {
        if let Some(suffix) = key.strip_prefix(&format!("{base}."))
            && !suffix.is_empty()
        {
            return Some((base, suffix));
        }
    }
    None
}

// ─── execute_set() ───────────────────────────────────────────────────────────

fn execute_set(
    json: &mut serde_json::Value,
    key: &str,
    values: &[String],
    args: &ConfigArgs,
) -> anyhow::Result<()> {
    // 1. Known single-value config key
    if let Some(vtype) = config_value_type(key) {
        match vtype {
            ConfigValueType::StringArray | ConfigValueType::EnumArray(_) => {
                if values.is_empty() {
                    anyhow::bail!("At least one value is required for \"{key}\"");
                }
                let normalized = validate_and_normalize_multi(key, values, &vtype)?;
                ensure_config_object(json);
                json["config"][key] = normalized;
                return Ok(());
            }
            _ => {
                if values.len() != 1 {
                    anyhow::bail!(
                        "Expected exactly one value for \"{key}\", got {}",
                        values.len()
                    );
                }
                let normalized = validate_and_normalize(key, &values[0], &vtype)?;
                ensure_config_object(json);
                json["config"][key] = normalized;
                return Ok(());
            }
        }
    }

    // 2. Dotted config subkeys: preferred-install.X, allow-plugins.X, platform.X
    if let Some((base, sub)) = split_dotted_config_key(key) {
        if values.len() != 1 {
            anyhow::bail!(
                "Expected exactly one value for \"{key}\", got {}",
                values.len()
            );
        }
        let value = &values[0];
        ensure_config_object(json);

        match base {
            "preferred-install" => {
                let lower = value.to_lowercase();
                if !["auto", "source", "dist"].contains(&lower.as_str()) {
                    anyhow::bail!(
                        "Invalid value \"{value}\" for \"{key}\". Must be one of: auto, source, dist"
                    );
                }
                json_set_nested(
                    json,
                    &format!("config.{base}.{sub}"),
                    serde_json::json!(lower),
                );
            }
            "allow-plugins" => {
                let normalized = normalize_bool(key, value)?;
                json_set_nested(json, &format!("config.{base}.{sub}"), normalized);
            }
            "platform" => {
                // value "false" → false (disable), otherwise string
                let val = if value == "false" {
                    serde_json::json!(false)
                } else {
                    serde_json::json!(value)
                };
                json_set_nested(json, &format!("config.{base}.{sub}"), val);
            }
            _ => unreachable!(),
        }
        return Ok(());
    }

    // 3. Package property
    if let Some(ptype) = package_property_type(key) {
        if args.global {
            anyhow::bail!(
                "Package property \"{key}\" cannot be set in the global config. Use a local composer.json."
            );
        }
        match ptype {
            ConfigValueType::StringArray | ConfigValueType::EnumArray(_) => {
                if values.is_empty() {
                    anyhow::bail!("At least one value is required for \"{key}\"");
                }
                let normalized = validate_and_normalize_multi(key, values, &ptype)?;
                if let Some(obj) = json.as_object_mut() {
                    obj.insert(key.to_string(), normalized);
                }
            }
            _ => {
                if values.len() != 1 {
                    anyhow::bail!(
                        "Expected exactly one value for \"{key}\", got {}",
                        values.len()
                    );
                }
                let normalized = validate_and_normalize(key, &values[0], &ptype)?;
                if let Some(obj) = json.as_object_mut() {
                    obj.insert(key.to_string(), normalized);
                }
            }
        }
        return Ok(());
    }

    // 4. Repository key
    if let Some(repo_name) = match_repository_key(key) {
        match values.len() {
            2 => {
                // type + url
                let repo_type = &values[0];
                let repo_url = &values[1];
                let entry = serde_json::json!({
                    "name": repo_name,
                    "type": repo_type,
                    "url": repo_url,
                });
                add_repository(json, repo_name, entry, args.append);
            }
            1 => {
                let v = &values[0];
                if v == "false" {
                    // Disable a repository
                    let entry = serde_json::json!({ repo_name: false });
                    add_repository(json, repo_name, entry, args.append);
                } else {
                    // Try to parse as JSON
                    let parsed: serde_json::Value = serde_json::from_str(v)
                        .map_err(|_| anyhow!("Invalid JSON for repository config: {v}"))?;
                    add_repository(json, repo_name, parsed, args.append);
                }
            }
            0 => {
                anyhow::bail!(
                    "At least one value (type url, false, or JSON) is required for repository \"{repo_name}\""
                );
            }
            _ => {
                anyhow::bail!(
                    "Too many values for repository \"{repo_name}\". Expected: <type> <url> or false or JSON"
                );
            }
        }
        return Ok(());
    }

    // 5. Extra key
    if let Some(sub) = key.strip_prefix("extra.") {
        if values.is_empty() {
            anyhow::bail!("A value is required for \"{key}\"");
        }
        let raw_value = &values[0];

        let new_value = if args.json {
            serde_json::from_str(raw_value)
                .map_err(|_| anyhow!("Invalid JSON value for \"{key}\": {raw_value}"))?
        } else {
            serde_json::json!(raw_value)
        };

        if args.merge {
            // Read existing value at path and merge
            let existing = get_nested(json, &format!("extra.{sub}")).cloned();
            let merged = merge_json_values(existing.as_ref(), &new_value)?;
            json_set_nested(json, &format!("extra.{sub}"), merged);
        } else {
            json_set_nested(json, &format!("extra.{sub}"), new_value);
        }
        return Ok(());
    }

    // 6. Suggest key
    if let Some(pkg_name) = key.strip_prefix("suggest.") {
        if values.is_empty() {
            anyhow::bail!("A value (reason) is required for \"{key}\"");
        }
        let reason = values.join(" ");
        // Ensure suggest object exists
        if !json["suggest"].is_object() {
            json_set_nested(json, "suggest", serde_json::json!({}));
        }
        json_set_nested(
            json,
            &format!("suggest.{pkg_name}"),
            serde_json::json!(reason),
        );
        return Ok(());
    }

    Err(anyhow!(
        "Setting \"{key}\" does not exist or is not supported"
    ))
}

/// Ensure `json["config"]` is an object.
fn ensure_config_object(json: &mut serde_json::Value) {
    if !json["config"].is_object()
        && let Some(obj) = json.as_object_mut()
    {
        obj.insert("config".to_string(), serde_json::json!({}));
    }
}

/// Get a value at a dot-separated path within a JSON Value.
fn get_nested<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let parts: Vec<&str> = path.splitn(2, '.').collect();
    if parts.len() == 1 {
        root.get(parts[0])
    } else {
        root.get(parts[0])
            .and_then(|child| get_nested(child, parts[1]))
    }
}

/// Merge two JSON values. Arrays are concatenated; objects are merged (new wins on conflict).
fn merge_json_values(
    existing: Option<&serde_json::Value>,
    new_value: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    match (existing, new_value) {
        (Some(serde_json::Value::Array(old)), serde_json::Value::Array(new)) => {
            let mut merged = old.clone();
            merged.extend(new.iter().cloned());
            Ok(serde_json::Value::Array(merged))
        }
        (Some(serde_json::Value::Object(old)), serde_json::Value::Object(new)) => {
            let mut merged = old.clone();
            for (k, v) in new {
                merged.insert(k.clone(), v.clone());
            }
            Ok(serde_json::Value::Object(merged))
        }
        (None, _) | (Some(_), _) => Ok(new_value.clone()),
    }
}

// ─── execute_read() ──────────────────────────────────────────────────────────

fn execute_read(
    args: &ConfigArgs,
    cli: &super::Cli,
    config_file_path: &Path,
) -> anyhow::Result<()> {
    // Build the effective config for config-section keys.
    let mut config = ComposerConfig::defaults();

    if args.global {
        let global_config_path = PathBuf::from(composer_home()).join("config.json");
        let overrides = load_config_section(&global_config_path)?;
        config.merge(&overrides);
    } else {
        let wd = working_dir(cli)?;
        let composer_json = wd.join("composer.json");
        let overrides = load_config_section(&composer_json)?;
        config.merge(&overrides);
    }

    config.resolve_references();

    // If --absolute is requested, resolve *-dir values to absolute paths.
    if args.absolute {
        let wd = working_dir(cli)?;
        let keys: Vec<String> = config.values.keys().cloned().collect();
        for key in keys {
            if key.ends_with("-dir")
                && let Some(serde_json::Value::String(s)) = config.values.get(&key).cloned()
            {
                let p = std::path::Path::new(&s);
                if p.is_relative() {
                    let abs = wd.join(p);
                    config.values.insert(
                        key,
                        serde_json::Value::String(abs.to_string_lossy().into_owned()),
                    );
                }
            }
        }
    }

    if args.list {
        for (key, value) in &config.values {
            println!("[{}] {}", key, render_value(value));
        }
        return Ok(());
    }

    match &args.setting_key {
        None => {
            eprintln!(
                "{}",
                mozart_core::console::error(
                    "No command specified. Use --list to show all config values, \
                     or provide a setting key."
                )
            );
            std::process::exit(1);
        }
        Some(key) => {
            // 1. Repository query
            if let Some(repo_name) = match_repository_key(key) {
                let raw = read_json_file(config_file_path, args.global)?;
                if let Some(repos) = raw["repositories"].as_array() {
                    for entry in repos {
                        if entry.get("name").and_then(|n| n.as_str()) == Some(repo_name) {
                            println!("{}", render_value(entry));
                            return Ok(());
                        }
                    }
                }
                return Err(anyhow!("Repository \"{}\" not found.", repo_name));
            }

            // 2. Extra or suggest dot-path query
            if key.starts_with("extra.") || key.starts_with("suggest.") {
                let raw = read_json_file(config_file_path, args.global)?;
                if let Some(v) = get_nested(&raw, key) {
                    println!("{}", render_value(v));
                    return Ok(());
                }
                return Err(anyhow!("Setting \"{}\" does not exist.", key));
            }

            // 3. Package property query
            if CONFIGURABLE_PACKAGE_PROPERTIES.contains(&key.as_str()) {
                let raw = read_json_file(config_file_path, args.global)?;
                if let Some(v) = raw.get(key.as_str()) {
                    println!("{}", render_value(v));
                    return Ok(());
                }
                // Fall through to config section lookup
            }

            // 4. Standard config key lookup
            match config.get(key) {
                Some(value) => {
                    println!("{}", render_value(value));
                }
                None => {
                    return Err(anyhow!("Setting \"{}\" does not exist.", key));
                }
            }
        }
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
        assert_eq!(render_value(&serde_json::Value::Null), "null");
    }

    #[test]
    fn test_render_value_array() {
        let v = serde_json::json!(["https", "ssh", "git"]);
        assert_eq!(render_value(&v), r#"["https","ssh","git"]"#);
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

    // ── match_repository_key ───────────────────────────────────────────────

    #[test]
    fn test_match_repository_key_full() {
        assert_eq!(match_repository_key("repositories.foo"), Some("foo"));
        assert_eq!(match_repository_key("repos.foo"), Some("foo"));
        assert_eq!(match_repository_key("repo.foo"), Some("foo"));
    }

    #[test]
    fn test_match_repository_key_no_match() {
        assert_eq!(match_repository_key("vendor-dir"), None);
        assert_eq!(match_repository_key("repositories."), None);
        assert_eq!(match_repository_key("sort-packages"), None);
    }

    // ── json_set_nested / json_remove_nested ───────────────────────────────

    #[test]
    fn test_json_set_nested_simple() {
        let mut root = serde_json::json!({});
        json_set_nested(&mut root, "foo", serde_json::json!("bar"));
        assert_eq!(root["foo"], serde_json::json!("bar"));
    }

    #[test]
    fn test_json_set_nested_deep() {
        let mut root = serde_json::json!({});
        json_set_nested(&mut root, "extra.foo.bar", serde_json::json!(42));
        assert_eq!(root["extra"]["foo"]["bar"], serde_json::json!(42));
    }

    #[test]
    fn test_json_set_nested_overwrites() {
        let mut root = serde_json::json!({"config": {"sort-packages": false}});
        json_set_nested(&mut root, "config.sort-packages", serde_json::json!(true));
        assert_eq!(root["config"]["sort-packages"], serde_json::json!(true));
    }

    #[test]
    fn test_json_remove_nested_simple() {
        let mut root = serde_json::json!({"foo": "bar"});
        let removed = json_remove_nested(&mut root, "foo");
        assert!(removed);
        assert!(root.get("foo").is_none());
    }

    #[test]
    fn test_json_remove_nested_deep() {
        let mut root = serde_json::json!({"config": {"sort-packages": true}});
        let removed = json_remove_nested(&mut root, "config.sort-packages");
        assert!(removed);
        assert!(root["config"].get("sort-packages").is_none());
    }

    #[test]
    fn test_json_remove_nested_nonexistent() {
        let mut root = serde_json::json!({"foo": "bar"});
        let removed = json_remove_nested(&mut root, "nonexistent");
        assert!(!removed);
    }

    // ── validate_and_normalize ─────────────────────────────────────────────

    #[test]
    fn test_validate_bool_true() {
        let result = validate_and_normalize("sort-packages", "true", &ConfigValueType::Bool);
        assert_eq!(result.unwrap(), serde_json::json!(true));
    }

    #[test]
    fn test_validate_bool_false() {
        let result = validate_and_normalize("sort-packages", "0", &ConfigValueType::Bool);
        assert_eq!(result.unwrap(), serde_json::json!(false));
    }

    #[test]
    fn test_validate_invalid_bool() {
        let result = validate_and_normalize("sort-packages", "maybe", &ConfigValueType::Bool);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_integer() {
        let result = validate_and_normalize("process-timeout", "600", &ConfigValueType::Integer);
        assert_eq!(result.unwrap(), serde_json::json!(600));
    }

    #[test]
    fn test_validate_invalid_integer() {
        let result = validate_and_normalize("process-timeout", "abc", &ConfigValueType::Integer);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_enum_valid() {
        let result = validate_and_normalize(
            "preferred-install",
            "source",
            &ConfigValueType::Enum(&["auto", "source", "dist"]),
        );
        assert_eq!(result.unwrap(), serde_json::json!("source"));
    }

    #[test]
    fn test_validate_enum_invalid() {
        let result = validate_and_normalize(
            "preferred-install",
            "invalid",
            &ConfigValueType::Enum(&["auto", "source", "dist"]),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_bool_or_enum_stash() {
        let result = validate_and_normalize(
            "discard-changes",
            "stash",
            &ConfigValueType::BoolOrEnum(&["stash"]),
        );
        assert_eq!(result.unwrap(), serde_json::json!("stash"));
    }

    #[test]
    fn test_validate_bool_or_enum_bool() {
        let result = validate_and_normalize(
            "discard-changes",
            "true",
            &ConfigValueType::BoolOrEnum(&["stash"]),
        );
        assert_eq!(result.unwrap(), serde_json::json!(true));
    }

    #[test]
    fn test_validate_autoloader_suffix_null() {
        let result = validate_and_normalize("autoloader-suffix", "null", &ConfigValueType::Str);
        assert_eq!(result.unwrap(), serde_json::Value::Null);
    }

    // ── validate_and_normalize_multi ───────────────────────────────────────

    #[test]
    fn test_validate_multi_string_array() {
        let values = vec!["a".to_string(), "b".to_string()];
        let result =
            validate_and_normalize_multi("github-domains", &values, &ConfigValueType::StringArray);
        assert_eq!(result.unwrap(), serde_json::json!(["a", "b"]));
    }

    #[test]
    fn test_validate_multi_enum_array_valid() {
        let values = vec!["https".to_string(), "ssh".to_string()];
        let result = validate_and_normalize_multi(
            "github-protocols",
            &values,
            &ConfigValueType::EnumArray(&["git", "https", "ssh"]),
        );
        assert_eq!(result.unwrap(), serde_json::json!(["https", "ssh"]));
    }

    #[test]
    fn test_validate_multi_enum_array_invalid() {
        let values = vec!["https".to_string(), "ftp".to_string()];
        let result = validate_and_normalize_multi(
            "github-protocols",
            &values,
            &ConfigValueType::EnumArray(&["git", "https", "ssh"]),
        );
        assert!(result.is_err());
    }

    // ── execute_set / execute_unset round-trips ────────────────────────────

    fn make_empty_json() -> serde_json::Value {
        serde_json::json!({})
    }

    fn make_config_args_default() -> ConfigArgs {
        ConfigArgs {
            setting_key: None,
            setting_value: vec![],
            global: false,
            editor: false,
            auth: false,
            unset: false,
            list: false,
            file: None,
            absolute: false,
            json: false,
            merge: false,
            append: false,
            source: false,
        }
    }

    #[test]
    fn test_set_bool_config_value() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "sort-packages", &["true".to_string()], &args).unwrap();
        assert_eq!(json["config"]["sort-packages"], serde_json::json!(true));
    }

    #[test]
    fn test_set_integer_config_value() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "process-timeout", &["600".to_string()], &args).unwrap();
        assert_eq!(json["config"]["process-timeout"], serde_json::json!(600));
    }

    #[test]
    fn test_set_string_config_value() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "vendor-dir", &["lib".to_string()], &args).unwrap();
        assert_eq!(json["config"]["vendor-dir"], serde_json::json!("lib"));
    }

    #[test]
    fn test_set_enum_config_value() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "preferred-install",
            &["source".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["config"]["preferred-install"],
            serde_json::json!("source")
        );
    }

    #[test]
    fn test_set_bool_or_enum_stash() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "discard-changes", &["stash".to_string()], &args).unwrap();
        assert_eq!(
            json["config"]["discard-changes"],
            serde_json::json!("stash")
        );
    }

    #[test]
    fn test_set_bool_or_enum_bool() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "discard-changes", &["true".to_string()], &args).unwrap();
        assert_eq!(json["config"]["discard-changes"], serde_json::json!(true));
    }

    #[test]
    fn test_set_multi_value() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "github-protocols",
            &["https".to_string(), "ssh".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["config"]["github-protocols"],
            serde_json::json!(["https", "ssh"])
        );
    }

    #[test]
    fn test_set_invalid_bool_value() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        let result = execute_set(&mut json, "sort-packages", &["maybe".to_string()], &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_invalid_enum_value() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        let result = execute_set(
            &mut json,
            "preferred-install",
            &["invalid".to_string()],
            &args,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_set_too_many_values() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        let result = execute_set(
            &mut json,
            "sort-packages",
            &["true".to_string(), "false".to_string()],
            &args,
        );
        assert!(result.is_err());
    }

    // ── unset tests ────────────────────────────────────────────────────────

    #[test]
    fn test_unset_config_value() {
        let mut json = serde_json::json!({"config": {"sort-packages": true}});
        let args = make_config_args_default();
        execute_unset(&mut json, "sort-packages", &args).unwrap();
        assert!(json["config"].get("sort-packages").is_none());
    }

    #[test]
    fn test_unset_nonexistent_key() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        let result = execute_unset(&mut json, "unknown-key-xyz", &args);
        assert!(result.is_err());
    }

    // ── package property tests ─────────────────────────────────────────────

    #[test]
    fn test_set_package_property_name() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "name", &["vendor/pkg".to_string()], &args).unwrap();
        assert_eq!(json["name"], serde_json::json!("vendor/pkg"));
    }

    #[test]
    fn test_set_minimum_stability() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "minimum-stability", &["dev".to_string()], &args).unwrap();
        assert_eq!(json["minimum-stability"], serde_json::json!("dev"));
    }

    #[test]
    fn test_set_prefer_stable() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "prefer-stable", &["true".to_string()], &args).unwrap();
        assert_eq!(json["prefer-stable"], serde_json::json!(true));
    }

    #[test]
    fn test_set_keywords() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "keywords",
            &["php".to_string(), "cli".to_string(), "tool".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(json["keywords"], serde_json::json!(["php", "cli", "tool"]));
    }

    #[test]
    fn test_set_package_property_global_error() {
        let mut json = make_empty_json();
        let mut args = make_config_args_default();
        args.global = true;
        let result = execute_set(&mut json, "name", &["vendor/pkg".to_string()], &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_unset_package_property() {
        let mut json = serde_json::json!({"description": "A test package"});
        let args = make_config_args_default();
        execute_unset(&mut json, "description", &args).unwrap();
        assert!(json.get("description").is_none());
    }

    // ── repository tests ───────────────────────────────────────────────────

    #[test]
    fn test_add_repository() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "repositories.foo",
            &["vcs".to_string(), "https://bar.com".to_string()],
            &args,
        )
        .unwrap();

        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["name"], serde_json::json!("foo"));
        assert_eq!(repos[0]["type"], serde_json::json!("vcs"));
        assert_eq!(repos[0]["url"], serde_json::json!("https://bar.com"));
    }

    #[test]
    fn test_add_repository_prepend() {
        let mut json = serde_json::json!({
            "repositories": [{"name": "existing", "type": "vcs", "url": "https://existing.com"}]
        });
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "repositories.new",
            &["vcs".to_string(), "https://new.com".to_string()],
            &args,
        )
        .unwrap();

        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], serde_json::json!("new"));
        assert_eq!(repos[1]["name"], serde_json::json!("existing"));
    }

    #[test]
    fn test_add_repository_append() {
        let mut json = serde_json::json!({
            "repositories": [{"name": "existing", "type": "vcs", "url": "https://existing.com"}]
        });
        let mut args = make_config_args_default();
        args.append = true;
        execute_set(
            &mut json,
            "repositories.new",
            &["vcs".to_string(), "https://new.com".to_string()],
            &args,
        )
        .unwrap();

        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], serde_json::json!("existing"));
        assert_eq!(repos[1]["name"], serde_json::json!("new"));
    }

    #[test]
    fn test_add_repository_replace_existing() {
        let mut json = serde_json::json!({
            "repositories": [{"name": "foo", "type": "vcs", "url": "https://old.com"}]
        });
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "repositories.foo",
            &["vcs".to_string(), "https://new.com".to_string()],
            &args,
        )
        .unwrap();

        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["url"], serde_json::json!("https://new.com"));
    }

    #[test]
    fn test_disable_repository() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "repositories.packagist.org",
            &["false".to_string()],
            &args,
        )
        .unwrap();

        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["packagist.org"], serde_json::json!(false));
    }

    #[test]
    fn test_remove_repository() {
        let mut json = serde_json::json!({
            "repositories": [{"name": "foo", "type": "vcs", "url": "https://bar.com"}]
        });
        let args = make_config_args_default();
        execute_unset(&mut json, "repo.foo", &args).unwrap();

        // Array removed when empty
        assert!(json.get("repositories").is_none());
    }

    #[test]
    fn test_repo_alias() {
        assert_eq!(match_repository_key("repo.foo"), Some("foo"));
        assert_eq!(match_repository_key("repos.foo"), Some("foo"));
        assert_eq!(match_repository_key("repositories.foo"), Some("foo"));
    }

    // ── extra/suggest tests ────────────────────────────────────────────────

    #[test]
    fn test_set_extra_property() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "extra.key", &["value".to_string()], &args).unwrap();
        assert_eq!(json["extra"]["key"], serde_json::json!("value"));
    }

    #[test]
    fn test_set_extra_json() {
        let mut json = make_empty_json();
        let mut args = make_config_args_default();
        args.json = true;
        execute_set(&mut json, "extra.key", &[r#"{"a":1}"#.to_string()], &args).unwrap();
        assert_eq!(json["extra"]["key"], serde_json::json!({"a": 1}));
    }

    #[test]
    fn test_set_extra_merge_objects() {
        let mut json = serde_json::json!({"extra": {"key": {"x": 1}}});
        let mut args = make_config_args_default();
        args.json = true;
        args.merge = true;
        execute_set(&mut json, "extra.key", &[r#"{"y":2}"#.to_string()], &args).unwrap();
        assert_eq!(json["extra"]["key"]["x"], serde_json::json!(1));
        assert_eq!(json["extra"]["key"]["y"], serde_json::json!(2));
    }

    #[test]
    fn test_set_extra_merge_arrays() {
        let mut json = serde_json::json!({"extra": {"key": [1, 2]}});
        let mut args = make_config_args_default();
        args.json = true;
        args.merge = true;
        execute_set(&mut json, "extra.key", &["[3, 4]".to_string()], &args).unwrap();
        assert_eq!(json["extra"]["key"], serde_json::json!([1, 2, 3, 4]));
    }

    #[test]
    fn test_set_suggest() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "suggest.vendor/pkg",
            &["for".to_string(), "testing".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["suggest"]["vendor/pkg"],
            serde_json::json!("for testing")
        );
    }

    #[test]
    fn test_unset_extra() {
        let mut json = serde_json::json!({"extra": {"key": "value"}});
        let args = make_config_args_default();
        execute_unset(&mut json, "extra.key", &args).unwrap();
        assert!(json["extra"].get("key").is_none());
    }

    // ── dotted config key tests ────────────────────────────────────────────

    #[test]
    fn test_set_platform_php() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(&mut json, "platform.php", &["8.1.0".to_string()], &args).unwrap();
        assert_eq!(
            json["config"]["platform"]["php"],
            serde_json::json!("8.1.0")
        );
    }

    #[test]
    fn test_unset_platform_php() {
        let mut json = serde_json::json!({"config": {"platform": {"php": "8.1.0"}}});
        let args = make_config_args_default();
        execute_unset(&mut json, "platform.php", &args).unwrap();
        assert!(json["config"]["platform"].get("php").is_none());
    }

    #[test]
    fn test_set_preferred_install_per_package() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "preferred-install.vendor/*",
            &["source".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["config"]["preferred-install"]["vendor/*"],
            serde_json::json!("source")
        );
    }

    #[test]
    fn test_set_allow_plugins() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "allow-plugins.vendor/plugin",
            &["true".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["config"]["allow-plugins"]["vendor/plugin"],
            serde_json::json!(true)
        );
    }

    // ── global config file tests ───────────────────────────────────────────

    #[test]
    fn test_global_config_creates_file() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let config_file = dir.path().join("config.json");

        // Start from an empty/nonexistent file
        let mut json = read_json_file(&config_file, true).unwrap();
        let args = make_config_args_default();
        execute_set(&mut json, "sort-packages", &["true".to_string()], &args).unwrap();
        write_json_file(&config_file, &json).unwrap();

        assert!(config_file.exists());
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_file).unwrap()).unwrap();
        assert_eq!(written["config"]["sort-packages"], serde_json::json!(true));
    }

    #[test]
    fn test_global_config_set_and_read() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let config_file = dir.path().join("config.json");

        // Write
        let mut json = read_json_file(&config_file, true).unwrap();
        let args = make_config_args_default();
        execute_set(&mut json, "vendor-dir", &["custom-lib".to_string()], &args).unwrap();
        write_json_file(&config_file, &json).unwrap();

        // Read back
        let json2 = read_json_file(&config_file, true).unwrap();
        assert_eq!(
            json2["config"]["vendor-dir"],
            serde_json::json!("custom-lib")
        );
    }

    // ── read_json_file default skeleton ───────────────────────────────────

    #[test]
    fn test_read_json_file_missing_global() {
        let path = std::path::Path::new("/tmp/nonexistent_global_abc123.json");
        let v = read_json_file(path, true).unwrap();
        assert!(v["config"].is_object());
    }

    #[test]
    fn test_read_json_file_missing_local() {
        let path = std::path::Path::new("/tmp/nonexistent_local_abc123.json");
        let v = read_json_file(path, false).unwrap();
        assert!(v.is_object());
        assert!(v.get("config").is_none());
    }
}
