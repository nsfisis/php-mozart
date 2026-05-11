//! Manifest validator. Rust port of `Composer\Util\ConfigValidator`.
//!
//! Holds the cross-command checks that both `mozart validate` and
//! `mozart diagnose` run against a `composer.json`. The split mirrors
//! Composer's: `ValidateCommand` and `DiagnoseCommand` each `new
//! ConfigValidator(...)`; neither depends on the other.

use crate::validation;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static DEPRECATED_GPL_OR_LATER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[AL]?GPL-[123](\.[01])?\+$").unwrap());

static DEPRECATED_GPL_BARE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[AL]?GPL-[123](\.[01])?$").unwrap());

/// Per-call validator flags, mirroring Composer's `$flags` bitfield on
/// `ConfigValidator::validate`.
#[derive(Debug, Clone, Copy)]
pub struct ValidatorOptions {
    /// Mirrors Composer's `CHECK_VERSION` flag — when set, warn that the
    /// `version` field is present (since Packagist derives versions from
    /// VCS tags).
    pub check_version: bool,
}

impl Default for ValidatorOptions {
    fn default() -> Self {
        // Composer defaults to `CHECK_VERSION` enabled.
        Self {
            check_version: true,
        }
    }
}

/// Validation outcome: `(errors, publishErrors, warnings)` in Composer's
/// PHP wording. `errors` block install/usage; `publish_errors` only block
/// publishing on Packagist; `warnings` are advisory.
#[derive(Debug, Default, Clone)]
pub struct ValidationResult {
    pub errors: Vec<String>,
    pub publish_errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationResult {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn has_publish_errors(&self) -> bool {
        !self.publish_errors.is_empty()
    }

    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Run every per-manifest check. Mirrors the body of
/// `Composer\Util\ConfigValidator::validate` after JSON parse.
pub fn validate_manifest(
    manifest: &serde_json::Value,
    options: &ValidatorOptions,
) -> ValidationResult {
    let mut result = ValidationResult::new();
    let obj = match manifest.as_object() {
        Some(o) => o,
        None => {
            result
                .errors
                .push("composer.json must be a JSON object".to_string());
            return result;
        }
    };

    check_name(obj, &mut result);
    check_license(obj, &mut result);

    if options.check_version {
        check_version_field(obj, &mut result);
    }

    check_package_type(obj, &mut result);
    check_require_overlap(obj, &mut result);
    check_provide_replace_overlap(obj, &mut result);
    check_commit_references(obj, &mut result);
    check_empty_psr_prefixes(obj, &mut result);
    check_minimum_stability(obj, &mut result);
    check_scripts_orphans(obj, &mut result);

    result
}

/// Check the "name" field: must be present (for published packages) and lowercase.
fn check_name(obj: &serde_json::Map<String, serde_json::Value>, result: &mut ValidationResult) {
    match obj.get("name").and_then(|v| v.as_str()) {
        None => {
            result.publish_errors.push(
                "The name property is not set. This is required for published packages."
                    .to_string(),
            );
        }
        Some(name) => {
            if name.chars().any(|c| c.is_ascii_uppercase()) {
                let suggested = name
                    .split('/')
                    .map(validation::sanitize_package_name_component)
                    .collect::<Vec<_>>()
                    .join("/");
                result.publish_errors.push(format!(
                    "Name \"{name}\" does not match the best practice (e.g. lower-cased/with-dashes). \
                     We suggest using \"{suggested}\" instead. As such you will not be able to submit it to Packagist."
                ));
            }

            if !name.is_empty() && !validation::validate_package_name(name) && !name.contains('/') {
                result.errors.push(format!(
                    "The name \"{name}\" is invalid, it should be in the format \"vendor/package\"."
                ));
            }
        }
    }
}

/// Check the "license" field. Mirrors:
///   * Composer's `Util\ConfigValidator::validate()` — "No license" + deprecation
///     warnings.
///   * Composer's `Package\Loader\ValidatingArrayLoader::load()` license block —
///     type-shape warnings, SPDX expression validity, and extra-spaces detection.
///     The validity/extra-spaces checks are gated on the `time` field: only
///     releases without a date or within the last 8 days are checked.
fn check_license(obj: &serde_json::Map<String, serde_json::Value>, result: &mut ValidationResult) {
    let no_license_msg = "No license specified, it is recommended to do so. \
         For closed-source software you may use \"proprietary\" as license."
        .to_string();

    let raw_entries: Vec<&serde_json::Value> = match obj.get("license") {
        None | Some(serde_json::Value::Null) => {
            result.warnings.push(no_license_msg);
            return;
        }
        Some(v @ serde_json::Value::String(s)) => {
            if s.is_empty() {
                result.warnings.push(no_license_msg);
                return;
            }
            vec![v]
        }
        Some(serde_json::Value::Array(arr)) => {
            if arr.is_empty() {
                result.warnings.push(no_license_msg);
                return;
            }
            arr.iter().collect()
        }
        Some(other) => {
            result.errors.push(format!(
                "License must be a string or array of strings, got {}.",
                serde_json::to_string(other).unwrap_or_default()
            ));
            return;
        }
    };

    let mut licenses: Vec<&str> = Vec::with_capacity(raw_entries.len());
    for v in raw_entries {
        match v.as_str() {
            Some(s) => licenses.push(s),
            None => result.warnings.push(format!(
                "License {} should be a string.",
                serde_json::to_string(v).unwrap_or_default()
            )),
        }
    }

    let spdx = mozart_spdx_licenses::spdx();
    for license in &licenses {
        if *license == "proprietary" {
            continue;
        }
        let Some(info) = spdx.get_license_by_identifier(license) else {
            continue;
        };
        if !info.deprecated {
            continue;
        }
        let warning = if DEPRECATED_GPL_OR_LATER_RE.is_match(license) {
            let suggested = format!("{}-or-later", license.replace('+', ""));
            format!(
                "License \"{license}\" is a deprecated SPDX license identifier, use \"{suggested}\" instead"
            )
        } else if DEPRECATED_GPL_BARE_RE.is_match(license) {
            format!(
                "License \"{license}\" is a deprecated SPDX license identifier, use \"{license}-only\" or \"{license}-or-later\" instead"
            )
        } else {
            format!(
                "License \"{license}\" is a deprecated SPDX license identifier, see https://spdx.org/licenses/"
            )
        };
        result.warnings.push(warning);
    }

    let release_ts = obj
        .get("time")
        .and_then(|v| v.as_str())
        .and_then(parse_iso_time_to_unix);
    let cutoff = current_unix_time().saturating_sub(8 * 86_400);
    let in_window = release_ts.is_none_or(|ts| ts >= cutoff);
    if !in_window {
        return;
    }
    for license in &licenses {
        if *license == "proprietary" {
            continue;
        }
        let to_validate = license.replace("proprietary", "MIT");
        if validation::validate_license(&to_validate) {
            continue;
        }
        let quoted = serde_json::to_string(license).unwrap_or_else(|_| format!("\"{license}\""));
        if validation::validate_license(to_validate.trim()) {
            result.warnings.push(format!(
                "License {quoted} must not contain extra spaces, make sure to trim it."
            ));
        } else {
            result.warnings.push(format!(
                "License {quoted} is not a valid SPDX license identifier, see https://spdx.org/licenses/ if you use an open license.\n\
                 If the software is closed-source, you may use \"proprietary\" as license."
            ));
        }
    }
}

/// Current time as a Unix timestamp (UTC seconds since epoch). 0 if the
/// system clock is set before the epoch.
fn current_unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse a Composer-style `time` string into a Unix timestamp.
///
/// Accepts `YYYY-MM-DD HH:MM:SS` (Composer's typical output) and
/// `YYYY-MM-DDTHH:MM:SS` with an optional `Z` or numeric offset suffix. The
/// timezone suffix is parsed when present; absent suffixes are treated as
/// UTC, matching `new DateTime($time, new DateTimeZone('UTC'))`.
fn parse_iso_time_to_unix(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let n = |start: usize, len: usize| -> Option<i64> {
        std::str::from_utf8(&bytes[start..start + len])
            .ok()?
            .parse()
            .ok()
    };
    let year = n(0, 4)? as i32;
    if bytes[4] != b'-' {
        return None;
    }
    let month = n(5, 2)? as i32;
    if bytes[7] != b'-' {
        return None;
    }
    let day = n(8, 2)? as i32;
    if bytes[10] != b' ' && bytes[10] != b'T' {
        return None;
    }
    let hour = n(11, 2)?;
    if bytes[13] != b':' {
        return None;
    }
    let minute = n(14, 2)?;
    if bytes[16] != b':' {
        return None;
    }
    let second = n(17, 2)?;

    let mut tz_offset_seconds: i64 = 0;
    if bytes.len() > 19 {
        let suffix = &s[19..];
        if suffix == "Z" {
            tz_offset_seconds = 0;
        } else {
            let body = suffix
                .strip_prefix('+')
                .or_else(|| suffix.strip_prefix('-'))?;
            let sign = if suffix.starts_with('+') { 1 } else { -1 };
            let body: String = body.chars().filter(|c| *c != ':').collect();
            if body.len() < 4 {
                return None;
            }
            let oh: i64 = body.get(0..2)?.parse().ok()?;
            let om: i64 = body.get(2..4)?.parse().ok()?;
            tz_offset_seconds = sign * (oh * 3600 + om * 60);
        }
    }

    let utc = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - tz_offset_seconds;
    Some(utc)
}

/// Howard Hinnant's `days_from_civil`: returns days since 1970-01-01 for a
/// proleptic Gregorian (year, month, day). Handles negative years correctly.
fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y / 400 } else { (y - 399) / 400 };
    let yoe = (y - era * 400) as i64;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64) * 146_097 + doe - 719_468
}

/// Warn if the "version" field is present.
fn check_version_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    if obj.contains_key("version") {
        result.warnings.push(
            "The version field is present, it is recommended to leave it out \
             if the package is published on Packagist."
                .to_string(),
        );
    }
}

/// Warn if the package type is the deprecated "composer-installer".
fn check_package_type(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    if let Some(pkg_type) = obj.get("type").and_then(|v| v.as_str())
        && pkg_type == "composer-installer"
    {
        result.warnings.push(
            "The package type 'composer-installer' is deprecated. \
             Please distribute your custom installers as plugins from now on. \
             See https://getcomposer.org/doc/articles/plugins.md for plugin documentation."
                .to_string(),
        );
    }
}

/// Warn if the same package appears in both require and require-dev.
fn check_require_overlap(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    let require = obj.get("require").and_then(|v| v.as_object());
    let require_dev = obj.get("require-dev").and_then(|v| v.as_object());

    if let (Some(req), Some(req_dev)) = (require, require_dev) {
        let mut overlaps: Vec<&str> = Vec::new();
        for key in req.keys() {
            if req_dev.contains_key(key) {
                overlaps.push(key.as_str());
            }
        }
        if !overlaps.is_empty() {
            let plural = if overlaps.len() > 1 { "are" } else { "is" };
            result.warnings.push(format!(
                "{} {plural} required both in require and require-dev, \
                 this can lead to unexpected behavior",
                overlaps.join(", "),
            ));
        }
    }
}

/// Warn if a package listed in provide/replace is also in require/require-dev.
fn check_provide_replace_overlap(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    for link_type in &["provide", "replace"] {
        if let Some(links) = obj.get(*link_type).and_then(|v| v.as_object()) {
            for require_type in &["require", "require-dev"] {
                if let Some(requires) = obj.get(*require_type).and_then(|v| v.as_object()) {
                    for provide_name in links.keys() {
                        if requires.contains_key(provide_name) {
                            result.warnings.push(format!(
                                "The package {provide_name} in {require_type} is also listed in \
                                 {link_type} which satisfies the requirement. Remove it from \
                                 {link_type} if you wish to install it."
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// Warn about version constraints containing '#' (commit references).
fn check_commit_references(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    for section in &["require", "require-dev"] {
        if let Some(deps) = obj.get(*section).and_then(|v| v.as_object()) {
            for (package, version) in deps {
                if let Some(v) = version.as_str()
                    && v.contains('#')
                {
                    result.warnings.push(format!(
                        "The package \"{package}\" is pointing to a commit-ref, \
                         this is bad practice and can cause unforeseen issues."
                    ));
                }
            }
        }
    }
}

/// Warn about empty PSR-0/PSR-4 namespace prefixes (performance impact).
fn check_empty_psr_prefixes(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    if let Some(autoload) = obj.get("autoload").and_then(|v| v.as_object()) {
        if let Some(psr0) = autoload.get("psr-0").and_then(|v| v.as_object())
            && psr0.contains_key("")
        {
            result.warnings.push(
                "Defining autoload.psr-0 with an empty namespace prefix is a bad idea \
                 for performance"
                    .to_string(),
            );
        }
        if let Some(psr4) = autoload.get("psr-4").and_then(|v| v.as_object())
            && psr4.contains_key("")
        {
            result.warnings.push(
                "Defining autoload.psr-4 with an empty namespace prefix is a bad idea \
                 for performance"
                    .to_string(),
            );
        }
    }
}

/// Check minimum-stability value if present.
fn check_minimum_stability(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    if let Some(stability) = obj.get("minimum-stability").and_then(|v| v.as_str())
        && !validation::validate_stability(stability)
    {
        result.errors.push(format!(
            "The minimum-stability \"{stability}\" is invalid. \
             Must be one of: dev, alpha, beta, rc, stable."
        ));
    }
}

/// Warn about keys in scripts-descriptions or scripts-aliases that have no matching script.
fn check_scripts_orphans(
    obj: &serde_json::Map<String, serde_json::Value>,
    result: &mut ValidationResult,
) {
    let script_keys: HashSet<&str> = obj
        .get("scripts")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();

    if let Some(descriptions) = obj.get("scripts-descriptions").and_then(|v| v.as_object()) {
        for key in descriptions.keys() {
            if !script_keys.contains(key.as_str()) {
                result.warnings.push(format!(
                    "Description for non-existent script \"{key}\" found in \"scripts-descriptions\""
                ));
            }
        }
    }

    if let Some(aliases) = obj.get("scripts-aliases").and_then(|v| v.as_object()) {
        for key in aliases.keys() {
            if !script_keys.contains(key.as_str()) {
                result.warnings.push(format!(
                    "Aliases for non-existent script \"{key}\" found in \"scripts-aliases\""
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_validate(json: &str, options: &ValidatorOptions) -> ValidationResult {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        validate_manifest(&value, options)
    }

    fn default_options() -> ValidatorOptions {
        ValidatorOptions::default()
    }

    #[test]
    fn test_validate_missing_name_is_publish_error() {
        let json = r#"{"require": {"php": ">=8.1"}, "license": "MIT"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.errors.is_empty());
        assert!(!result.publish_errors.is_empty());
        assert!(result.publish_errors[0].contains("name property is not set"));
    }

    #[test]
    fn test_validate_uppercase_name_publish_error() {
        let json = r#"{"name": "Vendor/Package", "license": "MIT"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.publish_errors.is_empty());
        assert!(result.publish_errors[0].contains("does not match the best practice"));
        assert!(result.publish_errors[0].contains("vendor/package"));
    }

    #[test]
    fn test_validate_uppercase_name_camel_case_to_dashes() {
        let json = r#"{"name": "MyCompany/MyLibrary", "license": "MIT"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.publish_errors.is_empty());
        assert!(
            result.publish_errors[0].contains("my-company/my-library"),
            "expected CamelCase-to-dashes conversion, got: {}",
            result.publish_errors[0]
        );
    }

    #[test]
    fn test_validate_valid_name_no_publish_error() {
        let json = r#"{"name": "vendor/package", "license": "MIT"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.publish_errors.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_name_without_slash_is_error() {
        let json = r#"{"name": "novendor", "license": "MIT"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("vendor/package"));
    }

    #[test]
    fn test_validate_missing_license_warns() {
        let json = r#"{"name": "vendor/pkg"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.warnings.is_empty());
        assert!(result.warnings.iter().any(|w| w.contains("No license")));
    }

    #[test]
    fn test_validate_present_license_no_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.warnings.iter().any(|w| w.contains("No license")));
    }

    #[test]
    fn test_validate_empty_license_string_warns() {
        let json = r#"{"name": "vendor/pkg", "license": ""}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.warnings.iter().any(|w| w.contains("No license")));
    }

    #[test]
    fn test_validate_empty_license_array_warns() {
        let json = r#"{"name": "vendor/pkg", "license": []}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.warnings.iter().any(|w| w.contains("No license")));
    }

    #[test]
    fn test_validate_proprietary_license_no_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "proprietary"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.warnings.iter().any(|w| w.contains("license")));
    }

    #[test]
    fn test_validate_unknown_license_no_deprecation_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "totally-not-a-license"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("deprecated SPDX"))
        );
    }

    #[test]
    fn test_validate_deprecated_gpl_bare_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "GPL-2.0"}"#;
        let result = parse_and_validate(json, &default_options());
        let warning = result
            .warnings
            .iter()
            .find(|w| w.contains("deprecated SPDX"))
            .expect("expected deprecation warning");
        assert!(warning.contains(r#""GPL-2.0""#), "got: {warning}");
        assert!(warning.contains("\"GPL-2.0-only\""), "got: {warning}");
        assert!(warning.contains("\"GPL-2.0-or-later\""), "got: {warning}");
    }

    #[test]
    fn test_validate_deprecated_gpl_or_later_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "GPL-3.0+"}"#;
        let result = parse_and_validate(json, &default_options());
        let warning = result
            .warnings
            .iter()
            .find(|w| w.contains("deprecated SPDX"))
            .expect("expected deprecation warning");
        assert!(warning.contains(r#""GPL-3.0+""#), "got: {warning}");
        assert!(warning.contains("\"GPL-3.0-or-later\""), "got: {warning}");
    }

    #[test]
    fn test_validate_deprecated_agpl_bare_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "AGPL-1.0"}"#;
        let result = parse_and_validate(json, &default_options());
        let warning = result
            .warnings
            .iter()
            .find(|w| w.contains("deprecated SPDX"))
            .expect("expected deprecation warning");
        assert!(warning.contains("\"AGPL-1.0-only\""), "got: {warning}");
        assert!(warning.contains("\"AGPL-1.0-or-later\""), "got: {warning}");
    }

    #[test]
    fn test_validate_deprecated_non_gpl_uses_generic_message() {
        let json = r#"{"name": "vendor/pkg", "license": "eCos-2.0"}"#;
        let result = parse_and_validate(json, &default_options());
        if let Some(warning) = result
            .warnings
            .iter()
            .find(|w| w.contains("deprecated SPDX"))
        {
            assert!(
                warning.contains("https://spdx.org/licenses/"),
                "expected generic message, got: {warning}"
            );
        }
    }

    #[test]
    fn test_validate_array_license_checks_each() {
        let json = r#"{"name": "vendor/pkg", "license": ["MIT", "GPL-2.0"]}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("deprecated SPDX") && w.contains("GPL-2.0")),
            "expected deprecation warning for GPL-2.0 in array form, got: {:?}",
            result.warnings,
        );
    }

    #[test]
    fn test_validate_non_deprecated_license_no_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("deprecated SPDX")),
            "MIT is not deprecated, should not warn",
        );
    }

    #[test]
    fn test_validate_license_wrong_type_errors() {
        let json = r#"{"name": "vendor/pkg", "license": 42}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result.errors.iter().any(|e| e
                .contains("License must be a string or array of strings")
                && e.contains("42")),
            "got: {:?}",
            result.errors
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("License must be")),
            "wrong-type license must not appear as warning, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_license_array_non_string_entry_warns() {
        let json = r#"{"name": "vendor/pkg", "license": ["MIT", 42]}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("License 42 should be a string")),
            "got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_invalid_spdx_license_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "totally-not-a-license"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("\"totally-not-a-license\"")
                    && w.contains("not a valid SPDX license identifier")),
            "got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_license_extra_spaces_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "  MIT  "}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("must not contain extra spaces")),
            "got: {:?}",
            result.warnings
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("not a valid SPDX")),
            "extra-spaces case should not also emit invalid-SPDX, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_license_proprietary_in_expression_validates() {
        let json = r#"{"name": "vendor/pkg", "license": "(MIT OR proprietary)"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("not a valid SPDX")),
            "got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_license_old_release_skips_validity_check() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "totally-not-a-license",
            "time": "1970-01-01 00:00:00"
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("not a valid SPDX")),
            "old release should not produce SPDX validity warning, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_license_recent_release_validates() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "totally-not-a-license",
            "time": "9999-01-01 00:00:00"
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("not a valid SPDX")),
            "recent release should produce SPDX validity warning, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_license_unparseable_time_treated_as_null() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "totally-not-a-license",
            "time": "not-a-date"
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("not a valid SPDX")),
            "unparseable time should be treated as null → validate, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_parse_iso_time_basic() {
        assert_eq!(parse_iso_time_to_unix("1970-01-01 00:00:00"), Some(0));
        assert_eq!(
            parse_iso_time_to_unix("2023-12-15 13:45:30"),
            Some(1_702_647_930)
        );
    }

    #[test]
    fn test_parse_iso_time_t_separator() {
        assert_eq!(
            parse_iso_time_to_unix("2023-12-15T13:45:30"),
            Some(1_702_647_930)
        );
        assert_eq!(
            parse_iso_time_to_unix("2023-12-15T13:45:30Z"),
            Some(1_702_647_930)
        );
    }

    #[test]
    fn test_parse_iso_time_with_offset() {
        assert_eq!(
            parse_iso_time_to_unix("2023-12-15T13:45:30+05:00"),
            Some(1_702_647_930 - 5 * 3600)
        );
        assert_eq!(
            parse_iso_time_to_unix("2023-12-15T13:45:30-05:00"),
            Some(1_702_647_930 + 5 * 3600)
        );
    }

    #[test]
    fn test_parse_iso_time_invalid() {
        assert_eq!(parse_iso_time_to_unix(""), None);
        assert_eq!(parse_iso_time_to_unix("not-a-date"), None);
        assert_eq!(parse_iso_time_to_unix("2023-12-15"), None);
        assert_eq!(parse_iso_time_to_unix("2023/12/15 13:45:30"), None);
    }

    #[test]
    fn test_validate_version_field_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "version": "1.0.0"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.warnings.iter().any(|w| w.contains("version field")));
    }

    #[test]
    fn test_validate_check_version_disabled_suppresses_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "version": "1.0.0"}"#;
        let options = ValidatorOptions {
            check_version: false,
        };
        let result = parse_and_validate(json, &options);
        assert!(!result.warnings.iter().any(|w| w.contains("version field")));
    }

    #[test]
    fn test_validate_deprecated_type_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "type": "composer-installer"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("composer-installer"))
        );
    }

    #[test]
    fn test_validate_normal_type_no_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "type": "library"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("composer-installer"))
        );
    }

    #[test]
    fn test_validate_require_overlap_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"monolog/monolog": "^3.0"},
            "require-dev": {"monolog/monolog": "^3.0"}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("required both in require and require-dev"))
        );
    }

    #[test]
    fn test_validate_no_require_overlap_no_warning() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"monolog/monolog": "^3.0"},
            "require-dev": {"phpunit/phpunit": "^10.0"}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.warnings.iter().any(|w| w.contains("required both")));
    }

    #[test]
    fn test_validate_provide_replace_overlap_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"psr/log": "^3.0"},
            "provide": {"psr/log": "^3.0"}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("also listed in provide"))
        );
    }

    #[test]
    fn test_validate_commit_ref_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"foo/bar": "dev-main#abc123"}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.warnings.iter().any(|w| w.contains("commit-ref")));
    }

    #[test]
    fn test_validate_normal_constraint_no_commit_warning() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"foo/bar": "^1.0"}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.warnings.iter().any(|w| w.contains("commit-ref")));
    }

    #[test]
    fn test_validate_empty_psr4_prefix_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "autoload": {"psr-4": {"": "src/"}}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.warnings.iter().any(|w| w.contains("psr-4")));
    }

    #[test]
    fn test_validate_empty_psr0_prefix_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "autoload": {"psr-0": {"": "src/"}}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.warnings.iter().any(|w| w.contains("psr-0")));
    }

    #[test]
    fn test_validate_named_psr4_prefix_no_warning() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "autoload": {"psr-4": {"Vendor\\Pkg\\": "src/"}}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.warnings.iter().any(|w| w.contains("psr-4")));
    }

    #[test]
    fn test_validate_invalid_stability_errors() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "minimum-stability": "invalid"}"#;
        let result = parse_and_validate(json, &default_options());
        assert!(!result.errors.is_empty());
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("minimum-stability"))
        );
    }

    #[test]
    fn test_validate_valid_stability_no_error() {
        for stab in &["dev", "alpha", "beta", "rc", "stable"] {
            let json = format!(
                r#"{{"name": "vendor/pkg", "license": "MIT", "minimum-stability": "{stab}"}}"#
            );
            let result = parse_and_validate(&json, &default_options());
            assert!(
                !result
                    .errors
                    .iter()
                    .any(|e| e.contains("minimum-stability")),
                "stability '{stab}' should be valid"
            );
        }
    }

    #[test]
    fn test_validate_non_object_json_errors() {
        let value = serde_json::json!([1, 2, 3]);
        let result = validate_manifest(&value, &default_options());
        assert!(result.errors.iter().any(|e| e.contains("JSON object")));
    }

    #[test]
    fn test_validate_scripts_descriptions_orphan_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "scripts": {"build": "make build"},
            "scripts-descriptions": {"build": "Build the project", "nonexistent": "Ghost script"}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("nonexistent") && w.contains("scripts-descriptions")),
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("\"build\"") && w.contains("scripts-descriptions")),
            "should not warn about existing script 'build'"
        );
    }

    #[test]
    fn test_validate_scripts_aliases_orphan_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "scripts": {"build": "make build"},
            "scripts-aliases": {"build": ["b"], "ghost": ["g"]}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("ghost") && w.contains("scripts-aliases")),
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("\"build\"") && w.contains("scripts-aliases")),
            "should not warn about existing script 'build'"
        );
    }

    #[test]
    fn test_validate_scripts_valid_no_orphan_warning() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "scripts": {"build": "make build", "test": "phpunit"},
            "scripts-descriptions": {"build": "Build the project", "test": "Run tests"},
            "scripts-aliases": {"build": ["b"], "test": ["t"]}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("scripts-descriptions") || w.contains("scripts-aliases")),
            "should produce no orphan warnings when all keys match, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn test_validate_no_errors_on_valid_package() {
        let json = r#"{
            "name": "vendor/package",
            "description": "A test package",
            "license": "MIT",
            "require": {"php": ">=8.1"}
        }"#;
        let result = parse_and_validate(json, &default_options());
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert!(
            result.publish_errors.is_empty(),
            "publish errors: {:?}",
            result.publish_errors
        );
        assert!(
            !result.warnings.iter().any(|w| w.contains("version field")),
            "unexpected version warning"
        );
    }
}
