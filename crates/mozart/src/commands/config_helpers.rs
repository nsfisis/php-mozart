use anyhow::anyhow;
use mozart_core::composer::composer_home;
use std::path::{Path, PathBuf};

/// Read TLS-related options (`config.cafile`, `config.capath`) from the merged
/// global + local config. Local values override global. Relative paths are
/// resolved against the directory of the config file that defined them.
pub(crate) fn load_tls_options(cli: &super::Cli) -> mozart_core::http::TlsOptions {
    let mut opts = mozart_core::http::TlsOptions::default();

    let home = composer_home();
    apply_tls_from_file(&home.join("config.json"), &home, &mut opts);

    if let Ok(wd) = cli.working_dir() {
        apply_tls_from_file(&wd.join("composer.json"), &wd, &mut opts);
    }

    opts
}

fn apply_tls_from_file(path: &Path, base_dir: &Path, opts: &mut mozart_core::http::TlsOptions) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };
    let Some(cfg) = json.get("config").and_then(|v| v.as_object()) else {
        return;
    };
    if let Some(s) = cfg.get("cafile").and_then(|v| v.as_str())
        && !s.is_empty()
    {
        opts.cafile = Some(resolve_relative(s, base_dir));
    }
    if let Some(s) = cfg.get("capath").and_then(|v| v.as_str())
        && !s.is_empty()
    {
        opts.capath = Some(resolve_relative(s, base_dir));
    }
}

fn resolve_relative(path: &str, base: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
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

/// Normalize a `repositories` value into a `Vec<serde_json::Value>`.
///
/// Composer stores repositories as an associative object:
///   `{"foo": {"type": "vcs", "url": "..."}}`
/// Mozart stores them as an array of objects with a `"name"` field:
///   `[{"name": "foo", "type": "vcs", "url": "..."}]`
///
/// This function accepts either format and always returns the array-of-objects
/// representation so callers can treat them uniformly.
pub(crate) fn normalize_repositories(value: &serde_json::Value) -> Vec<serde_json::Value> {
    match value {
        serde_json::Value::Array(arr) => arr.clone(),
        serde_json::Value::Object(obj) => obj
            .iter()
            .map(|(key, val)| {
                if let serde_json::Value::Object(inner) = val {
                    // Regular repo entry: inject "name" from the key if absent.
                    let mut entry = serde_json::Map::new();
                    entry.insert("name".to_string(), serde_json::json!(key));
                    for (k, v) in inner {
                        if k != "name" {
                            entry.insert(k.clone(), v.clone());
                        }
                    }
                    serde_json::Value::Object(entry)
                } else {
                    // Boolean / scalar entry (e.g. `"packagist.org": false`).
                    serde_json::json!({key: val})
                }
            })
            .collect(),
        _ => vec![],
    }
}

/// Add a repository entry to the `repositories` array in json.
/// If `append` is true, push to end; otherwise insert at beginning.
/// Removes any existing entry with the same name first.
/// Handles both array and associative-object repository forms (A19).
pub(crate) fn add_repository(
    json: &mut serde_json::Value,
    name: &str,
    config: serde_json::Value,
    append: bool,
) {
    // Normalize assoc-keyed repositories to list form (A19)
    if json["repositories"].is_object() {
        let normalized = normalize_repositories(&json["repositories"].clone());
        json["repositories"] = serde_json::Value::Array(normalized);
    }
    if !json["repositories"].is_array() {
        json["repositories"] = serde_json::json!([]);
    }

    // Build the entry, injecting "name" when absent (A19, mirrors Composer 108-110)
    let entry = if config == serde_json::Value::Bool(false) {
        // Disable entry: {name: false}
        let mut m = serde_json::Map::new();
        m.insert(name.to_string(), serde_json::Value::Bool(false));
        serde_json::Value::Object(m)
    } else if let Some(obj) = config.as_object()
        && !obj.contains_key("name")
        && !name.is_empty()
    {
        let mut new_map = serde_json::Map::new();
        new_map.insert("name".to_string(), serde_json::json!(name));
        for (k, v) in obj {
            new_map.insert(k.clone(), v.clone());
        }
        serde_json::Value::Object(new_map)
    } else {
        config
    };

    // Remove stale entries (by name or {name: false} disable) (A19)
    if let Some(repos) = json["repositories"].as_array_mut() {
        repos.retain(|val| {
            if let Some(entry_name) = val.get("name").and_then(|n| n.as_str()) {
                entry_name != name
            } else {
                // {name: false} disable entry
                !val.as_object()
                    .map(|obj| obj.contains_key(name))
                    .unwrap_or(false)
            }
        });
    }

    let repos = json["repositories"].as_array_mut().unwrap();
    if append {
        repos.push(entry);
    } else {
        repos.insert(0, entry);
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
