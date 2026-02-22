//! Metadata minification and expansion for Composer-compatible package
//! repositories.
//!
//! This crate is a Rust port of the PHP
//! [`composer/metadata-minifier`](https://packagist.org/packages/composer/metadata-minifier)
//! library. It operates on raw JSON values (`serde_json::Value`) so that it
//! stays independent of any particular package struct definition.
//!
//! # Minified format
//!
//! A minified version list is a JSON array of objects where:
//!
//! * The **first** entry carries all fields in full.
//! * Each **subsequent** entry only contains the fields that *differ* from the
//!   previous expanded entry.
//! * The sentinel string `"__unset"` marks a field as *deleted*.
//!
//! A response that uses this encoding contains a top-level key
//! `"minified": "composer/2.0"`.

use serde_json::{Map, Value};

/// The sentinel value used by Composer's metadata minifier to mark a deleted
/// field.
const UNSET_SENTINEL: &str = "__unset";

/// Expand a minified version list back to full form.
///
/// Each entry in the returned list is a self-contained JSON object with all
/// fields present (i.e. no diff encoding).
///
/// If the input is empty the output is empty. If the input contains only one
/// entry it is returned as-is.
pub fn expand(versions: &[Value]) -> Vec<Value> {
    let mut expanded: Vec<Value> = Vec::with_capacity(versions.len());

    let Some((first, rest)) = versions.split_first() else {
        return expanded;
    };

    let Some(mut state) = first.as_object().cloned() else {
        expanded.push(first.clone());
        return expanded;
    };

    expanded.push(Value::Object(state.clone()));

    for diff in rest {
        let Some(diff_map) = diff.as_object() else {
            expanded.push(diff.clone());
            continue;
        };

        for (key, val) in diff_map {
            if val.as_str() == Some(UNSET_SENTINEL) {
                state.remove(key.as_str());
            } else {
                state.insert(key.clone(), val.clone());
            }
        }

        expanded.push(Value::Object(state.clone()));
    }

    expanded
}

/// Minify a list of fully-expanded version objects into diff form.
///
/// The first entry is emitted in full. Each subsequent entry only contains
/// fields that changed compared to the previous one plus `"__unset"` markers
/// for fields that were removed.
pub fn minify(versions: &[Value]) -> Vec<Value> {
    let mut minified: Vec<Value> = Vec::with_capacity(versions.len());

    let Some((first, rest)) = versions.split_first() else {
        return minified;
    };

    let Some(mut last_known) = first.as_object().cloned() else {
        minified.push(first.clone());
        return minified;
    };

    minified.push(Value::Object(last_known.clone()));

    for version in rest {
        let Some(current) = version.as_object() else {
            minified.push(version.clone());
            continue;
        };

        let mut diff = Map::new();

        // Add changed or new fields.
        for (key, val) in current {
            match last_known.get(key) {
                Some(prev) if prev == val => {} // unchanged — omit
                _ => {
                    diff.insert(key.clone(), val.clone());
                    last_known.insert(key.clone(), val.clone());
                }
            }
        }

        // Mark deleted fields.
        let removed: Vec<String> = last_known
            .keys()
            .filter(|k| !current.contains_key(k.as_str()))
            .cloned()
            .collect();
        for key in &removed {
            diff.insert(key.clone(), Value::String(UNSET_SENTINEL.to_string()));
            last_known.remove(key.as_str());
        }

        minified.push(Value::Object(diff));
    }

    minified
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Mirrors the canonical Composer MetadataMinifierTest.
    #[test]
    fn expand_matches_composer_test() {
        let minified = vec![
            json!({
                "name": "foo/bar",
                "version": "2.0.0",
                "version_normalized": "2.0.0.0",
                "type": "library",
                "scripts": {"foo": ["bar"]},
                "license": ["MIT"]
            }),
            json!({
                "version": "1.2.0",
                "version_normalized": "1.2.0.0",
                "license": ["GPL"],
                "homepage": "https://example.org",
                "scripts": "__unset"
            }),
            json!({
                "version": "1.0.0",
                "version_normalized": "1.0.0.0",
                "homepage": "__unset"
            }),
        ];

        let expanded = expand(&minified);
        assert_eq!(expanded.len(), 3);

        // Version 2.0.0 — unchanged.
        assert_eq!(expanded[0]["name"], "foo/bar");
        assert_eq!(expanded[0]["version"], "2.0.0");
        assert_eq!(expanded[0]["type"], "library");
        assert_eq!(expanded[0]["scripts"], json!({"foo": ["bar"]}));
        assert_eq!(expanded[0]["license"], json!(["MIT"]));

        // Version 1.2.0 — inherits name, type; license changed; homepage
        // added; scripts removed.
        assert_eq!(expanded[1]["name"], "foo/bar");
        assert_eq!(expanded[1]["version"], "1.2.0");
        assert_eq!(expanded[1]["type"], "library");
        assert_eq!(expanded[1]["license"], json!(["GPL"]));
        assert_eq!(expanded[1]["homepage"], "https://example.org");
        assert!(expanded[1].get("scripts").is_none());

        // Version 1.0.0 — inherits from 1.2.0; homepage removed.
        assert_eq!(expanded[2]["name"], "foo/bar");
        assert_eq!(expanded[2]["version"], "1.0.0");
        assert_eq!(expanded[2]["type"], "library");
        assert_eq!(expanded[2]["license"], json!(["GPL"]));
        assert!(expanded[2].get("homepage").is_none());
        assert!(expanded[2].get("scripts").is_none());
    }

    /// Mirrors the canonical Composer MetadataMinifierTest.
    #[test]
    fn minify_matches_composer_test() {
        let full = vec![
            json!({
                "name": "foo/bar",
                "version": "2.0.0",
                "version_normalized": "2.0.0.0",
                "type": "library",
                "scripts": {"foo": ["bar"]},
                "license": ["MIT"]
            }),
            json!({
                "name": "foo/bar",
                "version": "1.2.0",
                "version_normalized": "1.2.0.0",
                "type": "library",
                "license": ["GPL"],
                "homepage": "https://example.org"
            }),
            json!({
                "name": "foo/bar",
                "version": "1.0.0",
                "version_normalized": "1.0.0.0",
                "type": "library",
                "license": ["GPL"]
            }),
        ];

        let minified = minify(&full);
        assert_eq!(minified.len(), 3);

        // First entry — unchanged.
        assert_eq!(minified[0], full[0]);

        // Second entry — only diffs.
        let diff1 = minified[1].as_object().unwrap();
        assert_eq!(diff1["version"], "1.2.0");
        assert_eq!(diff1["version_normalized"], "1.2.0.0");
        assert_eq!(diff1["license"], json!(["GPL"]));
        assert_eq!(diff1["homepage"], "https://example.org");
        assert_eq!(diff1["scripts"], "__unset");
        assert!(!diff1.contains_key("name"));
        assert!(!diff1.contains_key("type"));

        // Third entry — only diffs.
        let diff2 = minified[2].as_object().unwrap();
        assert_eq!(diff2["version"], "1.0.0");
        assert_eq!(diff2["version_normalized"], "1.0.0.0");
        assert_eq!(diff2["homepage"], "__unset");
        assert!(!diff2.contains_key("name"));
        assert!(!diff2.contains_key("type"));
        assert!(!diff2.contains_key("license"));
    }

    #[test]
    fn roundtrip_expand_minify() {
        let full = vec![
            json!({"name": "a/b", "version": "2.0.0", "require": {"php": ">=8.0"}}),
            json!({"name": "a/b", "version": "1.0.0", "require": {"php": ">=7.4"}}),
        ];

        let minified = minify(&full);
        let expanded = expand(&minified);
        assert_eq!(expanded, full);
    }

    #[test]
    fn expand_empty() {
        assert!(expand(&[]).is_empty());
    }

    #[test]
    fn minify_empty() {
        assert!(minify(&[]).is_empty());
    }

    #[test]
    fn expand_single_version() {
        let versions = vec![json!({"name": "a/b", "version": "1.0.0"})];
        let expanded = expand(&versions);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0], versions[0]);
    }

    #[test]
    fn minify_single_version() {
        let versions = vec![json!({"name": "a/b", "version": "1.0.0"})];
        let minified = minify(&versions);
        assert_eq!(minified.len(), 1);
        assert_eq!(minified[0], versions[0]);
    }
}
