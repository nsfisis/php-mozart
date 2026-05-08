use std::path::{Path, PathBuf};

use anyhow::anyhow;

pub struct JsonConfigSource {
    path: PathBuf,
    auth_config: bool,
}

impl JsonConfigSource {
    pub fn new(path: impl Into<PathBuf>, auth_config: bool) -> Self {
        Self {
            path: path.into(),
            auth_config,
        }
    }

    pub fn name(&self) -> &Path {
        &self.path
    }

    pub fn read(&self) -> anyhow::Result<serde_json::Value> {
        if !self.path.exists() {
            return if self.auth_config {
                Ok(serde_json::json!({}))
            } else {
                Ok(serde_json::json!({"config": {}}))
            };
        }
        let content = std::fs::read_to_string(&self.path)?;
        serde_json::from_str(&content)
            .map_err(|e| anyhow!("Failed to parse JSON from {}: {}", self.path.display(), e))
    }

    fn write(&self, value: &serde_json::Value) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        crate::package::write_to_file(value, &self.path)
    }

    /// Convert assoc-keyed `repositories` object to list format in-place.
    /// Mirrors the normalization in Composer's `addRepository` / `insertRepository` fallback.
    fn normalize_to_list(root: &mut serde_json::Value) {
        if !root["repositories"].is_object() {
            return;
        }
        let obj = root["repositories"].as_object().unwrap().clone();
        let list: Vec<serde_json::Value> = obj
            .iter()
            .map(|(key, val)| {
                if let Some(inner) = val.as_object() {
                    let mut entry = serde_json::Map::new();
                    if !inner.contains_key("name") {
                        entry.insert(
                            "name".to_string(),
                            serde_json::Value::String(key.clone()),
                        );
                    }
                    for (k, v) in inner {
                        entry.insert(k.clone(), v.clone());
                    }
                    serde_json::Value::Object(entry)
                } else {
                    let mut m = serde_json::Map::new();
                    m.insert(key.clone(), val.clone());
                    serde_json::Value::Object(m)
                }
            })
            .collect();
        root["repositories"] = serde_json::Value::Array(list);
    }

    fn make_disabled(name: &str) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert(name.to_string(), serde_json::Value::Bool(false));
        serde_json::Value::Object(m)
    }

    fn is_disabled_entry(val: &serde_json::Value, name: &str) -> bool {
        val.as_object()
            .map(|obj| obj.len() == 1 && obj.get(name) == Some(&serde_json::Value::Bool(false)))
            .unwrap_or(false)
    }

    fn cleanup_empty_repos(root: &mut serde_json::Value) {
        let is_empty = match &root["repositories"] {
            serde_json::Value::Array(a) => a.is_empty(),
            serde_json::Value::Object(o) => o.is_empty(),
            _ => false,
        };
        if is_empty
            && let Some(obj) = root.as_object_mut()
        {
            obj.remove("repositories");
        }
    }

    /// Mirror of Composer's `JsonConfigSource::addRepository`.
    ///
    /// When `config` is `Value::Bool(false)`, writes a `{name: false}` disable entry.
    /// Otherwise injects `"name"` into the config object if absent, removes duplicate
    /// entries by name, then prepends or appends depending on `append`.
    pub fn add_repository(
        &self,
        name: &str,
        config: &serde_json::Value,
        append: bool,
    ) -> anyhow::Result<()> {
        // TODO: JsonManipulator fast path to preserve original formatting
        let mut root = self.read()?;
        Self::normalize_to_list(&mut root);

        if !root["repositories"].is_array() {
            root["repositories"] = serde_json::json!([]);
        }

        if config == &serde_json::Value::Bool(false) {
            // Find any existing entry that has the repo by name, or an existing disable entry
            let (match_by_name, already_disabled) = {
                let repos = root["repositories"].as_array().unwrap();
                let mut by_name: Option<usize> = None;
                let mut disabled = false;
                for (i, repo) in repos.iter().enumerate() {
                    if repo.get("name").and_then(|n| n.as_str()) == Some(name) {
                        by_name = Some(i);
                        break;
                    }
                    if Self::is_disabled_entry(repo, name) {
                        disabled = true;
                        break;
                    }
                }
                (by_name, disabled)
            };

            if already_disabled {
                return Ok(());
            }
            if let Some(idx) = match_by_name {
                root["repositories"][idx] = Self::make_disabled(name);
            } else {
                root["repositories"]
                    .as_array_mut()
                    .unwrap()
                    .push(Self::make_disabled(name));
            }
        } else {
            let mut entry = config.clone();
            if let Some(obj) = config.as_object()
                && !obj.contains_key("name")
                && !name.is_empty()
            {
                let mut new_map = serde_json::Map::new();
                new_map.insert(
                    "name".to_string(),
                    serde_json::Value::String(name.to_string()),
                );
                for (k, v) in obj {
                    new_map.insert(k.clone(), v.clone());
                }
                entry = serde_json::Value::Object(new_map);
            }

            let repos = root["repositories"].as_array_mut().unwrap();
            repos.retain(|val| {
                val.get("name").and_then(|n| n.as_str()) != Some(name)
                    && !Self::is_disabled_entry(val, name)
            });
            if append {
                repos.push(entry);
            } else {
                repos.insert(0, entry);
            }
        }

        Self::cleanup_empty_repos(&mut root);
        self.write(&root)
    }

    /// Mirror of Composer's `JsonConfigSource::insertRepository`.
    ///
    /// `offset = 0` inserts before `reference_name`; `offset = 1` inserts after.
    pub fn insert_repository(
        &self,
        name: &str,
        config: &serde_json::Value,
        reference_name: &str,
        offset: u32,
    ) -> anyhow::Result<()> {
        // TODO: JsonManipulator fast path to preserve original formatting
        let mut root = self.read()?;
        Self::normalize_to_list(&mut root);

        if !root["repositories"].is_array() {
            root["repositories"] = serde_json::json!([]);
        }

        {
            let repos = root["repositories"].as_array_mut().unwrap();
            repos.retain(|val| {
                val.get("name").and_then(|n| n.as_str()) != Some(name)
                    && !Self::is_disabled_entry(val, name)
            });
        }

        let index_to_insert = {
            let repos = root["repositories"].as_array().unwrap();
            repos
                .iter()
                .position(|repo| {
                    repo.get("name").and_then(|n| n.as_str()) == Some(reference_name)
                        || Self::is_disabled_entry(repo, reference_name)
                })
                .ok_or_else(|| {
                    anyhow!(
                        "The referenced repository \"{}\" does not exist.",
                        reference_name
                    )
                })?
        };

        let mut entry = config.clone();
        if let Some(obj) = config.as_object()
            && !obj.contains_key("name")
            && !name.is_empty()
        {
            let mut new_map = serde_json::Map::new();
            new_map.insert(
                "name".to_string(),
                serde_json::Value::String(name.to_string()),
            );
            for (k, v) in obj {
                new_map.insert(k.clone(), v.clone());
            }
            entry = serde_json::Value::Object(new_map);
        }

        root["repositories"]
            .as_array_mut()
            .unwrap()
            .insert(index_to_insert + offset as usize, entry);

        Self::cleanup_empty_repos(&mut root);
        self.write(&root)
    }

    /// Mirror of Composer's `JsonConfigSource::setRepositoryUrl`.
    ///
    /// Handles both assoc-keyed and list-format repositories without converting
    /// between the two shapes (preserves existing format).
    pub fn set_repository_url(&self, name: &str, url: &str) -> anyhow::Result<()> {
        // TODO: JsonManipulator fast path to preserve original formatting
        let mut root = self.read()?;
        let url_val = serde_json::Value::String(url.to_string());

        // Assoc-keyed fast path (mirrors Composer's `if ($name === $index)` branch)
        let in_assoc = root["repositories"]
            .as_object()
            .and_then(|obj| obj.get(name))
            .and_then(|v| v.as_object())
            .is_some();
        if in_assoc {
            root["repositories"][name]["url"] = url_val;
            return self.write(&root);
        }

        // List format: find entry by `name` field
        let idx = root["repositories"].as_array().and_then(|repos| {
            repos.iter().position(|repo| {
                repo.get("name").and_then(|n| n.as_str()) == Some(name)
            })
        });

        match idx {
            Some(i) => {
                root["repositories"][i]["url"] = url_val;
                self.write(&root)
            }
            None => Err(anyhow!("Repository \"{}\" not found", name)),
        }
    }

    /// Mirror of Composer's `JsonConfigSource::removeRepository`.
    ///
    /// Handles assoc-keyed and list-format repositories. Removes the `repositories`
    /// key entirely when the list becomes empty (mirrors Composer L219–221).
    pub fn remove_repository(&self, name: &str) -> anyhow::Result<()> {
        // TODO: JsonManipulator fast path to preserve original formatting
        let mut root = self.read()?;

        // Assoc-keyed format
        let in_assoc = root["repositories"]
            .as_object()
            .map(|obj| obj.contains_key(name))
            .unwrap_or(false);
        if in_assoc {
            root["repositories"].as_object_mut().unwrap().remove(name);
            Self::cleanup_empty_repos(&mut root);
            return self.write(&root);
        }

        // List format
        if let Some(repos) = root["repositories"].as_array_mut() {
            repos.retain(|val| {
                val.get("name").and_then(|n| n.as_str()) != Some(name)
                    && !Self::is_disabled_entry(val, name)
            });
        }

        Self::cleanup_empty_repos(&mut root);
        self.write(&root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn source(dir: &TempDir, filename: &str) -> (JsonConfigSource, std::path::PathBuf) {
        let path = dir.path().join(filename);
        (JsonConfigSource::new(path.clone(), false), path)
    }

    #[test]
    fn add_repository_prepend() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(&path, r#"{"repositories":[{"name":"a","type":"vcs","url":"https://a.com"}]}"#).unwrap();
        src.add_repository(
            "b",
            &serde_json::json!({"type": "vcs", "url": "https://b.com"}),
            false,
        )
        .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["repositories"][0]["name"], "b");
        assert_eq!(json["repositories"][1]["name"], "a");
    }

    #[test]
    fn add_repository_append() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(&path, r#"{"repositories":[{"name":"a","type":"vcs","url":"https://a.com"}]}"#).unwrap();
        src.add_repository(
            "b",
            &serde_json::json!({"type": "vcs", "url": "https://b.com"}),
            true,
        )
        .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["repositories"][0]["name"], "a");
        assert_eq!(json["repositories"][1]["name"], "b");
    }

    #[test]
    fn add_repository_disable() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(&path, "{}").unwrap();
        src.add_repository("packagist.org", &serde_json::Value::Bool(false), true)
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["repositories"][0]["packagist.org"], false);
    }

    #[test]
    fn add_repository_disable_already_disabled_is_noop() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(
            &path,
            r#"{"repositories":[{"packagist.org":false}]}"#,
        )
        .unwrap();
        src.add_repository("packagist.org", &serde_json::Value::Bool(false), true)
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Still just one entry
        assert_eq!(json["repositories"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn remove_repository_list_format() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(
            &path,
            r#"{"repositories":[{"name":"foo","type":"vcs","url":"https://foo.com"}]}"#,
        )
        .unwrap();
        src.remove_repository("foo").unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(json.get("repositories").is_none());
    }

    #[test]
    fn remove_repository_assoc_format() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(
            &path,
            r#"{"repositories":{"foo":{"type":"vcs","url":"https://foo.com"}}}"#,
        )
        .unwrap();
        src.remove_repository("foo").unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(json.get("repositories").is_none());
    }

    #[test]
    fn insert_repository_before() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(
            &path,
            r#"{"repositories":[{"name":"a","type":"vcs","url":"https://a.com"},{"name":"b","type":"vcs","url":"https://b.com"}]}"#,
        )
        .unwrap();
        src.insert_repository(
            "new",
            &serde_json::json!({"type": "vcs", "url": "https://new.com"}),
            "b",
            0,
        )
        .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["repositories"][0]["name"], "a");
        assert_eq!(json["repositories"][1]["name"], "new");
        assert_eq!(json["repositories"][2]["name"], "b");
    }

    #[test]
    fn insert_repository_after() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(
            &path,
            r#"{"repositories":[{"name":"a","type":"vcs","url":"https://a.com"},{"name":"b","type":"vcs","url":"https://b.com"}]}"#,
        )
        .unwrap();
        src.insert_repository(
            "new",
            &serde_json::json!({"type": "vcs", "url": "https://new.com"}),
            "a",
            1,
        )
        .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["repositories"][0]["name"], "a");
        assert_eq!(json["repositories"][1]["name"], "new");
        assert_eq!(json["repositories"][2]["name"], "b");
    }

    #[test]
    fn insert_repository_reference_not_found() {
        let dir = TempDir::new().unwrap();
        let (src, path) = source(&dir, "composer.json");
        std::fs::write(&path, r#"{"repositories":[]}"#).unwrap();
        let result = src.insert_repository(
            "new",
            &serde_json::json!({"type": "vcs", "url": "https://new.com"}),
            "nonexistent",
            0,
        );
        assert!(result.is_err());
    }
}
