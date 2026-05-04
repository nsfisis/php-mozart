//! Metadata minification and expansion for Composer-compatible package
//! repositories.
//!
//! This crate is a Rust port of the PHP
//! [`composer/metadata-minifier`](https://packagist.org/packages/composer/metadata-minifier)
//! library. It operates on raw JSON values (`serde_json::Value`) so that it
//! stays independent of any particular package struct definition.
//!
//! MetadataMinifier::minify() is not ported because it is not used in Composer itself.
//! The function is mainly for package repositories.
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

use serde_json::Value;

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

    #[test]
    fn expand_empty() {
        assert!(expand(&[]).is_empty());
    }

    #[test]
    fn expand_single_version() {
        let versions = vec![json!({"name": "a/b", "version": "1.0.0"})];
        let expanded = expand(&versions);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0], versions[0]);
    }
}
