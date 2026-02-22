use anyhow::anyhow;
use std::path::{Path, PathBuf};

/// Return the Composer home directory, respecting `COMPOSER_HOME` and
/// falling back to the platform default using Composer-compatible logic.
///
/// On Unix:
/// - If XDG is in use (any `XDG_*` env var exists, or `/etc/xdg` exists),
///   prefer `$XDG_CONFIG_HOME/composer` (or `$HOME/.config/composer`).
/// - Always include `$HOME/.composer` as a fallback candidate.
/// - Return the first candidate directory that exists on disk;
///   if none exist, return the first candidate.
pub(crate) fn composer_home() -> PathBuf {
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

/// Check whether XDG base directories are in use:
/// any env var starting with `XDG_` exists, OR `/etc/xdg` directory exists.
fn use_xdg() -> bool {
    std::env::vars().any(|(k, _)| k.starts_with("XDG_"))
        || std::path::Path::new("/etc/xdg").is_dir()
}

/// Build the working directory path, preferring `--working-dir` over `cwd`.
pub(crate) fn working_dir(cli: &super::Cli) -> anyhow::Result<PathBuf> {
    match &cli.working_dir {
        Some(d) => Ok(PathBuf::from(d)),
        None => Ok(std::env::current_dir()?),
    }
}

/// Read a JSON file as `serde_json::Value`.
/// If the file does not exist, return a default skeleton:
/// `{"config": {}}` for global files, `{}` for local.
pub(crate) fn read_json_file(path: &Path, is_global: bool) -> anyhow::Result<serde_json::Value> {
    if !path.exists() {
        if is_global {
            return Ok(serde_json::json!({"config": {}}));
        }
        return Ok(serde_json::json!({}));
    }
    let content = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow!("Failed to parse JSON from {}: {}", path.display(), e))?;
    Ok(value)
}

/// Write a `serde_json::Value` back to a file with 4-space indentation + trailing newline.
pub(crate) fn write_json_file(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    mozart_core::package::write_to_file(value, path)?;
    Ok(())
}

/// Add a repository entry to the `repositories` array in json.
/// If `append` is true, push to end; otherwise insert at beginning.
/// Removes any existing entry with the same name first.
pub(crate) fn add_repository(
    json: &mut serde_json::Value,
    name: &str,
    config: serde_json::Value,
    append: bool,
) {
    if !json["repositories"].is_array() {
        json["repositories"] = serde_json::json!([]);
    }

    remove_repository(json, name);

    let repos = json["repositories"].as_array_mut().unwrap();
    if append {
        repos.push(config);
    } else {
        repos.insert(0, config);
    }
}

/// Remove a repository entry by name from the `repositories` array.
pub(crate) fn remove_repository(json: &mut serde_json::Value, name: &str) {
    if let Some(repos) = json["repositories"].as_array_mut() {
        repos.retain(|entry| {
            if let Some(entry_name) = entry.get("name").and_then(|n| n.as_str()) {
                entry_name != name
            } else {
                let disabled_key_matches = entry
                    .as_object()
                    .map(|obj| obj.contains_key(name))
                    .unwrap_or(false);
                !disabled_key_matches
            }
        });
    }
}

/// Insert a repository entry before or after a named repository.
/// Returns an error if the target repository is not found.
pub(crate) fn insert_repository(
    json: &mut serde_json::Value,
    name: &str,
    config: serde_json::Value,
    target: &str,
    before: bool,
) -> anyhow::Result<()> {
    if !json["repositories"].is_array() {
        json["repositories"] = serde_json::json!([]);
    }

    remove_repository(json, name);

    let repos = json["repositories"].as_array_mut().unwrap();

    let pos = repos
        .iter()
        .position(|entry| {
            entry.get("name").and_then(|n| n.as_str()) == Some(target)
                || entry
                    .as_object()
                    .map(|obj| obj.contains_key(target))
                    .unwrap_or(false)
        })
        .ok_or_else(|| anyhow!("Repository \"{target}\" not found"))?;

    let insert_pos = if before { pos } else { pos + 1 };
    repos.insert(insert_pos, config);
    Ok(())
}

/// Render a `serde_json::Value` as a human-readable string suitable for
/// single-line display (matching Composer's behaviour).
pub(crate) fn render_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(_) => {
            serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
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
