use super::config_helpers::{
    add_repository, read_json_file, remove_repository, render_value, write_json_file,
};
use anyhow::anyhow;
use clap::Args;
use mozart_core::composer::composer_home;
use mozart_core::config::resolve_references;
use mozart_core::console_writeln;
use mozart_core::factory::create_config;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// Classification of config key value types for validation and normalization.
#[derive(Debug)]
enum ConfigValueType {
    /// Single boolean value (true/false/1/0)
    Bool,
    /// Single integer value
    Integer,
    /// Single string value (any string accepted)
    Str,
    /// File path string: accepts "null" to clear; validates existence
    FilePath,
    /// Directory path string: accepts "null" to clear; validates existence
    DirPath,
    /// Size string: integer with optional k/m/g suffix (e.g. "10MiB")
    SizeString,
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
        "cache-files-maxsize" => Some(ConfigValueType::SizeString),
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
        "cafile" => Some(ConfigValueType::FilePath),
        "capath" => Some(ConfigValueType::DirPath),
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
        ConfigValueType::FilePath => {
            if value == "null" {
                return Ok(serde_json::Value::Null);
            }
            if !std::path::Path::new(value).exists() {
                return Err(anyhow!("\"{value}\" does not exist for \"{key}\""));
            }
            Ok(serde_json::json!(value))
        }
        ConfigValueType::DirPath => {
            if value == "null" {
                return Ok(serde_json::Value::Null);
            }
            if !std::path::Path::new(value).is_dir() {
                return Err(anyhow!(
                    "\"{value}\" is not a directory or does not exist for \"{key}\""
                ));
            }
            Ok(serde_json::json!(value))
        }
        ConfigValueType::SizeString => {
            // Mirrors Composer's regex: /^\s*([0-9.]+)\s*(?:([kmg])(?:i?b)?)?\s*$/i
            let re =
                regex::Regex::new(r"(?i)^\s*[0-9]+(\.[0-9]*)?\s*(?:[kmg](?:i?b)?)?\s*$").unwrap();
            if re.is_match(value) {
                Ok(serde_json::json!(value))
            } else {
                Err(anyhow!(
                    "Invalid size string \"{value}\" for \"{key}\". \
                     Expected a number with optional k/m/g suffix, e.g. \"10MiB\""
                ))
            }
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
        ConfigValueType::StringArray | ConfigValueType::EnumArray(_) => Err(anyhow!(
            "\"{key}\" is a multi-value setting. Provide one or more values."
        )),
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

/// Determine which JSON file to read/write.
/// - `--global` → `$COMPOSER_HOME/config.json`
/// - `--file <path>` → user-specified file
/// - default → `<working_dir>/composer.json`
fn resolve_config_file_path(args: &ConfigArgs, cli: &super::Cli) -> anyhow::Result<PathBuf> {
    if args.global && args.file.is_some() {
        anyhow::bail!("Cannot combine --global and --file");
    }
    if args.global {
        return Ok(composer_home().join("config.json"));
    }
    if let Some(ref file) = args.file {
        return Ok(PathBuf::from(file));
    }
    Ok(cli.working_dir()?.join("composer.json"))
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

pub async fn execute(
    args: &ConfigArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
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
            anyhow::bail!("You can not combine a setting value with --unset");
        }
        return execute_write(args, cli, &config_file_path);
    }

    // 4b. Read mode
    execute_read(args, cli, &config_file_path, console)
}

fn execute_editor(args: &ConfigArgs, cli: &super::Cli) -> anyhow::Result<()> {
    // A8: --auth opens auth.json instead of the config file
    let file_path = if args.auth {
        if args.global {
            composer_home().join("auth.json")
        } else {
            cli.working_dir()?.join("auth.json")
        }
    } else {
        resolve_config_file_path(args, cli)?
    };

    // A7: Composer-compatible editor fallback chain
    #[cfg(target_os = "windows")]
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad".to_string());
    #[cfg(not(target_os = "windows"))]
    let editor = {
        if let Ok(ed) = std::env::var("EDITOR") {
            ed
        } else {
            let candidates = ["editor", "vim", "vi", "nano", "pico", "ed"];
            candidates
                .iter()
                .find(|&&cand| find_in_path(cand))
                .copied()
                .unwrap_or("vi")
                .to_string()
        }
    };

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

/// Check whether `name` exists as an executable file anywhere on PATH.
fn find_in_path(name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(name).is_file() {
                return true;
            }
        }
    }
    false
}

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

fn execute_unset(json: &mut serde_json::Value, key: &str, args: &ConfigArgs) -> anyhow::Result<()> {
    // 1. Repository key
    if let Some(repo_name) = match_repository_key(key) {
        remove_repository(json, repo_name);
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

    // 2. Bare top-level extra / suggest / audit (A5)
    if key == "extra" || key == "suggest" {
        if let Some(obj) = json.as_object_mut() {
            obj.remove(key);
        }
        return Ok(());
    }
    if key == "audit" {
        // Mirror Composer 560-564: unset config.audit
        json_remove_nested(json, "config.audit");
        return Ok(());
    }

    // 3. Dotted config subkeys: preferred-install.X, allow-plugins.X, platform.X, audit.X
    if let Some((base, sub)) = split_dotted_config_key(key) {
        let path = format!("config.{base}.{sub}");
        json_remove_nested(json, &path);
        return Ok(());
    }

    // 4. Known top-level config key
    if config_value_type(key).is_some() {
        // A13: disable-tls re-enable message
        if key == "disable-tls" {
            let was_disabled = json
                .get("config")
                .and_then(|c| c.get("disable-tls"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if was_disabled {
                eprintln!("You are now running Mozart with SSL/TLS protection enabled.");
            }
        }
        json_remove_nested(json, &format!("config.{key}"));
        return Ok(());
    }

    // 5. Package property
    if CONFIGURABLE_PACKAGE_PROPERTIES.contains(&key) {
        if args.global {
            anyhow::bail!("Package property \"{key}\" cannot be unset in the global config");
        }
        if let Some(obj) = json.as_object_mut() {
            obj.remove(key);
        }
        return Ok(());
    }

    // 6. Extra dot-path (extra.X, suggest.X, or scripts.X) (A3)
    if key.starts_with("extra.") || key.starts_with("suggest.") || key.starts_with("scripts.") {
        json_remove_nested(json, key);
        return Ok(());
    }

    // 7. Top-level fallback: single-segment unknown key (A4)
    if !key.contains('.') {
        if let Some(obj) = json.as_object_mut() {
            obj.remove(key);
        }
        return Ok(());
    }

    Err(anyhow!(
        "Setting \"{key}\" does not exist or is not supported"
    ))
}

/// Split a dotted config subkey like `preferred-install.vendor/*` into
/// `("preferred-install", "vendor/*")` for the supported dotted config keys.
fn split_dotted_config_key(key: &str) -> Option<(&str, &str)> {
    for base in &["preferred-install", "allow-plugins", "platform", "audit"] {
        if let Some(suffix) = key.strip_prefix(&format!("{base}."))
            && !suffix.is_empty()
        {
            return Some((base, suffix));
        }
    }
    None
}

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
                // A6: disable-tls user-visible warning
                if key == "disable-tls" {
                    let was_enabled = !json
                        .get("config")
                        .and_then(|c| c.get("disable-tls"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if normalized == serde_json::json!(true) && was_enabled {
                        eprintln!("You are now running Mozart with SSL/TLS protection disabled.");
                    }
                }
                ensure_config_object(json);
                json["config"][key] = normalized;
                return Ok(());
            }
        }
    }

    // 2. Dotted config subkeys: preferred-install.X, allow-plugins.X, platform.X
    //    audit.X (except audit.ignore / audit.ignore-abandoned which are handled below)
    if let Some((base, sub)) = split_dotted_config_key(key) {
        // audit.ignore and audit.ignore-abandoned need JSON/merge support; skip here
        if base == "audit" && (sub == "ignore" || sub == "ignore-abandoned") {
            // fall through to the dedicated audit.ignore handler below
        } else {
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
                    let val = if value == "false" {
                        serde_json::json!(false)
                    } else {
                        serde_json::json!(value)
                    };
                    json_set_nested(json, &format!("config.{base}.{sub}"), val);
                }
                "audit" => {
                    // Other audit.X sub-keys (not ignore/ignore-abandoned) — simple set
                    json_set_nested(
                        json,
                        &format!("config.{base}.{sub}"),
                        serde_json::json!(value),
                    );
                }
                _ => unreachable!(),
            }
            return Ok(());
        }
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

    // 4. Repository key (including repositories.<name>.url sub-path — A16)
    // Check for repositories.<name>.url pattern first
    for prefix in &["repositories.", "repos.", "repo."] {
        if let Some(rest) = key.strip_prefix(prefix)
            && let Some(dot_pos) = rest.find('.')
        {
            let repo_name = &rest[..dot_pos];
            let field = &rest[dot_pos + 1..];
            if field == "url" {
                if values.len() != 1 {
                    anyhow::bail!("Expected exactly one value for \"{key}\"");
                }
                let url = &values[0];
                // Find entry by name and update its url
                if let Some(repos) = json["repositories"].as_array_mut() {
                    let found = repos.iter_mut().any(|entry| {
                        if entry.get("name").and_then(|n| n.as_str()) == Some(repo_name) {
                            entry["url"] = serde_json::json!(url);
                            true
                        } else {
                            false
                        }
                    });
                    if !found {
                        anyhow::bail!("Repository \"{repo_name}\" not found");
                    }
                } else {
                    anyhow::bail!("Repository \"{repo_name}\" not found");
                }
                return Ok(());
            }
            break;
        }
    }

    if let Some(repo_name) = match_repository_key(key) {
        match values.len() {
            2 => {
                // type + url
                let repo_type = &values[0];
                let repo_url = &values[1];
                let entry = serde_json::json!({
                    "type": repo_type,
                    "url": repo_url,
                });
                add_repository(json, repo_name, entry, args.append);
            }
            1 => {
                let v = &values[0];
                if v == "false" {
                    add_repository(json, repo_name, serde_json::Value::Bool(false), args.append);
                } else {
                    let parsed: serde_json::Value = serde_json::from_str(v)
                        .map_err(|_| anyhow!("Invalid JSON for repository config: {v}"))?;
                    add_repository(json, repo_name, parsed, args.append);
                }
            }
            0 => {
                anyhow::bail!(
                    "You must pass the type and a url. Example: php composer.phar config repositories.foo vcs https://bar.com"
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

    // 7. audit.ignore / audit.ignore-abandoned (A2)
    if key == "audit.ignore" || key == "audit.ignore-abandoned" {
        let sub = key.strip_prefix("audit.").unwrap();
        if values.is_empty() {
            anyhow::bail!("A value is required for \"{key}\"");
        }
        let raw_value = &values[0];

        let new_value: serde_json::Value = if args.json {
            serde_json::from_str(raw_value)
                .map_err(|_| anyhow!("Invalid JSON value for \"{key}\": {raw_value}"))?
        } else {
            serde_json::json!(raw_value)
        };

        if args.merge {
            let existing = get_nested(json, &format!("config.audit.{sub}")).cloned();
            match (&existing, &new_value) {
                (Some(serde_json::Value::Array(_)), serde_json::Value::Object(_))
                | (Some(serde_json::Value::Object(_)), serde_json::Value::Array(_)) => {
                    anyhow::bail!(
                        "Could not merge audit.{sub}: cannot merge an array with an object"
                    );
                }
                _ => {}
            }
            let merged = merge_json_values(existing.as_ref(), &new_value)?;
            ensure_config_object(json);
            json_set_nested(json, &format!("config.audit.{sub}"), merged);
        } else {
            ensure_config_object(json);
            json_set_nested(json, &format!("config.audit.{sub}"), new_value);
        }
        return Ok(());
    }

    // 8. scripts.X (A3)
    if let Some(script_name) = key.strip_prefix("scripts.") {
        if values.is_empty() {
            anyhow::bail!("A value is required for \"{key}\"");
        }
        let val = if values.len() == 1 {
            serde_json::json!(&values[0])
        } else {
            serde_json::Value::Array(values.iter().map(|v| serde_json::json!(v)).collect())
        };
        json_set_nested(json, &format!("scripts.{script_name}"), val);
        return Ok(());
    }

    // 9. Auth key stub (A1) — full implementation deferred to JsonConfigSource
    let auth_prefixes = [
        "bitbucket-oauth.",
        "github-oauth.",
        "gitlab-oauth.",
        "gitlab-token.",
        "http-basic.",
        "custom-headers.",
        "bearer.",
        "forgejo-token.",
    ];
    if auth_prefixes.iter().any(|p| key.starts_with(p)) {
        anyhow::bail!(
            "Auth credentials must be stored in auth.json \
             (auth.json support is not yet fully implemented in Mozart)"
        );
    }

    Err(anyhow!(
        "Setting \"{key}\" does not exist or is not supported by this command"
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
            // Mirrors PHP `+` operator semantics: existing keys win
            let mut merged = new.clone();
            for (k, v) in old {
                merged.insert(k.clone(), v.clone());
            }
            Ok(serde_json::Value::Object(merged))
        }
        (None, _) | (Some(_), _) => Ok(new_value.clone()),
    }
}

fn execute_read(
    args: &ConfigArgs,
    cli: &super::Cli,
    config_file_path: &Path,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // Build the effective config for config-section keys.
    // Global baseline (defaults + platform dirs + $COMPOSER_HOME/config.json),
    // then overlay project config on top when not in --global mode.
    let mut config = create_config()?;

    if !args.global {
        let wd = cli.working_dir()?;
        let composer_json = wd.join("composer.json");
        let overrides = load_config_section(&composer_json)?;
        config.merge(&overrides)?;
    }

    resolve_references(&mut config);

    // If --absolute is requested, resolve *-dir values to absolute paths.
    if args.absolute {
        let wd = cli.working_dir()?;
        config.make_dirs_absolute(&wd);
    }

    if args.list {
        for (key, value) in config.entries() {
            console_writeln!(
                console,
                mozart_core::console::Verbosity::Quiet,
                "[{}] {}",
                key,
                render_value(&value),
            );
        }
        return Ok(());
    }

    match &args.setting_key {
        // A9: Mirror Composer 220-223: silently return 0 when no setting-key given
        None => {
            return Ok(());
        }
        Some(key) => {
            // 1. Repository query
            if let Some(repo_name) = match_repository_key(key) {
                let raw = read_json_file(config_file_path, args.global)?;
                if let Some(repos) = raw["repositories"].as_array() {
                    for entry in repos {
                        if entry.get("name").and_then(|n| n.as_str()) == Some(repo_name) {
                            console_writeln!(
                                console,
                                mozart_core::console::Verbosity::Quiet,
                                "{}",
                                &render_value(entry),
                            );
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
                    console_writeln!(
                        console,
                        mozart_core::console::Verbosity::Quiet,
                        "{}",
                        &render_value(v),
                    );
                    return Ok(());
                }
                return Err(anyhow!("Setting \"{}\" does not exist.", key));
            }

            // 3. Package property query
            if CONFIGURABLE_PACKAGE_PROPERTIES.contains(&key.as_str()) {
                let raw = read_json_file(config_file_path, args.global)?;
                if let Some(v) = raw.get(key.as_str()) {
                    console_writeln!(
                        console,
                        mozart_core::console::Verbosity::Quiet,
                        "{}",
                        &render_value(v),
                    );
                    return Ok(());
                }
                // Fall through to config section lookup
            }

            // 4. Standard config key lookup
            match config.get(key) {
                Some(value) => {
                    console_writeln!(
                        console,
                        mozart_core::console::Verbosity::Quiet,
                        "{}",
                        &render_value(&value),
                    );
                }
                None => {
                    return Err(anyhow!("Setting \"{}\" does not exist.", key));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_core::config::Config;

    #[test]
    fn test_defaults_contain_expected_keys() {
        let cfg = Config::default();

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
            assert!(cfg.get(*key).is_some(), "defaults missing key: {key}");
        }
    }

    #[test]
    fn test_defaults_values_correct() {
        let cfg = Config::default();

        assert_eq!(cfg.process_timeout, 300);
        assert_eq!(cfg.preferred_install, serde_json::json!("dist"));
        assert_eq!(cfg.vendor_dir, "vendor");
        assert_eq!(cfg.github_protocols, vec!["https", "ssh", "git"]);
        assert_eq!(cfg.secure_http, true);
        assert_eq!(cfg.lock, true);
        assert_eq!(cfg.autoloader_suffix, None);
    }

    #[test]
    fn test_merge_overrides_existing_key() {
        let mut cfg = Config::default();

        let mut overrides = BTreeMap::new();
        overrides.insert("vendor-dir".to_string(), serde_json::json!("packages"));
        overrides.insert("sort-packages".to_string(), serde_json::json!(true));

        cfg.merge(&overrides).unwrap();

        assert_eq!(cfg.vendor_dir, "packages");
        assert_eq!(cfg.sort_packages, true);
    }

    #[test]
    fn test_merge_adds_new_key() {
        let mut cfg = Config::default();

        let mut overrides = BTreeMap::new();
        overrides.insert("custom-key".to_string(), serde_json::json!("custom-value"));

        cfg.merge(&overrides).unwrap();

        assert_eq!(cfg.extra["custom-key"], serde_json::json!("custom-value"));
    }

    #[test]
    fn test_merge_empty_overrides_leaves_defaults_intact() {
        let mut cfg = Config::default();
        let original_vendor = cfg.vendor_dir.clone();

        cfg.merge(&BTreeMap::new()).unwrap();

        assert_eq!(cfg.vendor_dir, original_vendor);
    }

    #[test]
    fn test_reference_resolution_bin_dir() {
        let mut cfg = Config::default();
        // bin-dir default is "{$vendor-dir}/bin"; vendor-dir default is "vendor"
        resolve_references(&mut cfg);

        assert_eq!(cfg.bin_dir, "vendor/bin");
    }

    #[test]
    fn test_reference_resolution_custom_vendor_dir() {
        let mut cfg = Config::default();

        cfg.vendor_dir = "lib".to_string();
        resolve_references(&mut cfg);

        assert_eq!(cfg.bin_dir, "lib/bin");
    }

    #[test]
    fn test_reference_resolution_cache_dirs() {
        let mut cfg = Config::default();
        // Inject a predictable home so the test is environment-independent.
        cfg.cache_dir = "/home/user/.cache/composer".to_string();
        resolve_references(&mut cfg);

        assert_eq!(cfg.cache_files_dir, "/home/user/.cache/composer/files");
        assert_eq!(cfg.cache_repo_dir, "/home/user/.cache/composer/repo");
        assert_eq!(cfg.cache_vcs_dir, "/home/user/.cache/composer/vcs");
    }

    #[test]
    fn test_reference_resolution_no_change_for_non_string() {
        let mut cfg = Config::default();
        let before = cfg.process_timeout;
        resolve_references(&mut cfg);
        assert_eq!(cfg.process_timeout, before);
    }

    #[test]
    fn test_get_existing_key() {
        let cfg = Config::default();
        let value = cfg.get("vendor-dir");
        assert!(value.is_some());
        assert_eq!(value.unwrap(), serde_json::json!("vendor"));
    }

    #[test]
    fn test_get_nonexistent_key_returns_none() {
        let cfg = Config::default();
        assert!(cfg.get("does-not-exist").is_none());
    }

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
        let mut cfg = Config::default();
        cfg.merge(&overrides).unwrap();
        resolve_references(&mut cfg);

        assert_eq!(cfg.vendor_dir, "custom_vendor");
        assert_eq!(cfg.sort_packages, true);
        // bin-dir should have resolved against the overridden vendor-dir
        assert_eq!(cfg.bin_dir, "custom_vendor/bin");
    }

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

    #[test]
    fn test_unset_config_value() {
        let mut json = serde_json::json!({"config": {"sort-packages": true}});
        let args = make_config_args_default();
        execute_unset(&mut json, "sort-packages", &args).unwrap();
        assert!(json["config"].get("sort-packages").is_none());
    }

    #[test]
    fn test_unset_nonexistent_key() {
        // A4: unknown top-level single-segment key is silently removed (mirrors Composer 920-924)
        let mut json = make_empty_json();
        let args = make_config_args_default();
        let result = execute_unset(&mut json, "unknown-key-xyz", &args);
        assert!(result.is_ok());
    }

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

    // --- A2: audit.ignore / audit.ignore-abandoned ---

    #[test]
    fn test_set_audit_ignore_simple() {
        let mut json = make_empty_json();
        let mut args = make_config_args_default();
        args.json = true;
        execute_set(
            &mut json,
            "audit.ignore",
            &[r#"["CVE-2024-AAAA"]"#.to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["config"]["audit"]["ignore"],
            serde_json::json!(["CVE-2024-AAAA"])
        );
    }

    #[test]
    fn test_set_audit_ignore_merge_arrays() {
        let mut json = serde_json::json!({"config": {"audit": {"ignore": ["CVE-2024-AAAA"]}}});
        let mut args = make_config_args_default();
        args.json = true;
        args.merge = true;
        execute_set(
            &mut json,
            "audit.ignore",
            &[r#"["CVE-2024-XXXX"]"#.to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["config"]["audit"]["ignore"],
            serde_json::json!(["CVE-2024-AAAA", "CVE-2024-XXXX"])
        );
    }

    #[test]
    fn test_set_audit_ignore_merge_list_object_error() {
        let mut json = serde_json::json!({"config": {"audit": {"ignore": ["CVE-2024-AAAA"]}}});
        let mut args = make_config_args_default();
        args.json = true;
        args.merge = true;
        let result = execute_set(
            &mut json,
            "audit.ignore",
            &[r#"{"pkg/name": "reason"}"#.to_string()],
            &args,
        );
        assert!(result.is_err());
    }

    // --- A3: scripts.X ---

    #[test]
    fn test_set_scripts_single() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "scripts.post-install-cmd",
            &["echo done".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["scripts"]["post-install-cmd"],
            serde_json::json!("echo done")
        );
    }

    #[test]
    fn test_set_scripts_multi() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "scripts.post-install-cmd",
            &["echo a".to_string(), "echo b".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["scripts"]["post-install-cmd"],
            serde_json::json!(["echo a", "echo b"])
        );
    }

    #[test]
    fn test_unset_scripts() {
        let mut json = serde_json::json!({"scripts": {"post-install-cmd": "echo done"}});
        let args = make_config_args_default();
        execute_unset(&mut json, "scripts.post-install-cmd", &args).unwrap();
        assert!(json["scripts"].get("post-install-cmd").is_none());
    }

    // --- A4: top-level --unset fallback ---

    #[test]
    fn test_unset_unknown_top_level_key_succeeds() {
        let mut json = serde_json::json!({"my-custom-field": "value"});
        let args = make_config_args_default();
        execute_unset(&mut json, "my-custom-field", &args).unwrap();
        assert!(json.get("my-custom-field").is_none());
    }

    // --- A5: bare extra / suggest / audit ---

    #[test]
    fn test_unset_extra_bare() {
        let mut json = serde_json::json!({"extra": {"key": "value"}});
        let args = make_config_args_default();
        execute_unset(&mut json, "extra", &args).unwrap();
        assert!(json.get("extra").is_none());
    }

    #[test]
    fn test_unset_suggest_bare() {
        let mut json = serde_json::json!({"suggest": {"vendor/pkg": "reason"}});
        let args = make_config_args_default();
        execute_unset(&mut json, "suggest", &args).unwrap();
        assert!(json.get("suggest").is_none());
    }

    #[test]
    fn test_unset_audit_bare() {
        let mut json = serde_json::json!({"config": {"audit": {"abandoned": "report"}}});
        let args = make_config_args_default();
        execute_unset(&mut json, "audit", &args).unwrap();
        assert!(json["config"].get("audit").is_none());
    }

    // --- A10: cache-files-maxsize validation ---

    #[test]
    fn test_cache_files_maxsize_valid() {
        for v in &["512M", "512MB", "512MiB", "1g", "1GiB", "100", "1.5k"] {
            let result =
                validate_and_normalize("cache-files-maxsize", v, &ConfigValueType::SizeString);
            assert!(result.is_ok(), "expected ok for {v}");
        }
    }

    #[test]
    fn test_cache_files_maxsize_invalid() {
        let result =
            validate_and_normalize("cache-files-maxsize", "abc", &ConfigValueType::SizeString);
        assert!(result.is_err());
    }

    // --- A14: merge_json_values existing-wins ---

    #[test]
    fn test_merge_objects_existing_wins() {
        // Composer PHP `+` semantics: existing keys take precedence
        let existing = serde_json::json!({"a": 1, "b": 2});
        let new_val = serde_json::json!({"a": 99, "c": 3});
        let result = merge_json_values(Some(&existing), &new_val).unwrap();
        assert_eq!(result["a"], serde_json::json!(1)); // existing wins
        assert_eq!(result["b"], serde_json::json!(2));
        assert_eq!(result["c"], serde_json::json!(3)); // new key added
    }

    // --- A11: cafile / capath null clearing ---

    #[test]
    fn test_cafile_null_clears() {
        let result = validate_and_normalize("cafile", "null", &ConfigValueType::FilePath);
        assert_eq!(result.unwrap(), serde_json::Value::Null);
    }

    #[test]
    fn test_capath_null_clears() {
        let result = validate_and_normalize("capath", "null", &ConfigValueType::DirPath);
        assert_eq!(result.unwrap(), serde_json::Value::Null);
    }

    // --- A16: repositories.<name>.url ---

    #[test]
    fn test_set_repository_url() {
        let mut json = serde_json::json!({
            "repositories": [{"name": "foo", "type": "vcs", "url": "https://old.com"}]
        });
        let args = make_config_args_default();
        execute_set(
            &mut json,
            "repositories.foo.url",
            &["https://new.com".to_string()],
            &args,
        )
        .unwrap();
        assert_eq!(
            json["repositories"][0]["url"],
            serde_json::json!("https://new.com")
        );
    }

    #[test]
    fn test_set_repository_url_not_found() {
        let mut json = serde_json::json!({"repositories": []});
        let args = make_config_args_default();
        let result = execute_set(
            &mut json,
            "repositories.nonexistent.url",
            &["https://x.com".to_string()],
            &args,
        );
        assert!(result.is_err());
    }

    // --- A19: add_repository with name injection and assoc-form normalization ---

    #[test]
    fn test_add_repository_injects_name() {
        let mut json = make_empty_json();
        let args = make_config_args_default();
        // Passing config without "name" field
        execute_set(
            &mut json,
            "repositories.myrepo",
            &["vcs".to_string(), "https://example.com".to_string()],
            &args,
        )
        .unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], serde_json::json!("myrepo"));
    }
}
