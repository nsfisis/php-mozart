use clap::Args;
use mozart_core::console_format;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to composer.json file
    pub file: Option<String>,

    /// Skips checks for non-essential issues
    #[arg(long)]
    pub no_check_all: bool,

    /// Validates the lock file
    #[arg(long)]
    pub check_lock: bool,

    /// Skips lock file validation
    #[arg(long)]
    pub no_check_lock: bool,

    /// Skips publish-related checks
    #[arg(long)]
    pub no_check_publish: bool,

    /// Skips version constraint checks
    #[arg(long)]
    pub no_check_version: bool,

    /// Also validate all dependencies
    #[arg(short = 'A', long)]
    pub with_dependencies: bool,

    /// Return a non-zero exit code on warnings as well as errors
    #[arg(long)]
    pub strict: bool,
}

// ─── Result accumulator ─────────────────────────────────────────────────────

struct ValidationResult {
    errors: Vec<String>,
    publish_errors: Vec<String>,
    warnings: Vec<String>,
}

impl ValidationResult {
    fn new() -> Self {
        Self {
            errors: Vec::new(),
            publish_errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    fn has_publish_errors(&self) -> bool {
        !self.publish_errors.is_empty()
    }

    fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn should_check_lock(args: &ValidateArgs, manifest: &serde_json::Value) -> bool {
    let config_lock_enabled = manifest
        .get("config")
        .and_then(|c| c.get("lock"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    (!args.no_check_lock && config_lock_enabled) || args.check_lock
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub async fn execute(
    args: &ValidateArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // Determine which file to validate
    let file = match &args.file {
        Some(f) => PathBuf::from(f),
        None => working_dir.join("composer.json"),
    };

    // Validate-specific exit codes (matching Composer's behavior):
    //   3 = file not found or not readable
    //   2 = JSON parse error
    const VALIDATE_FILE_ERROR: i32 = 3;
    const VALIDATE_JSON_ERROR: i32 = 2;

    // Check file exists
    if !file.exists() {
        return Err(mozart_core::exit_code::bail(
            VALIDATE_FILE_ERROR,
            format!("{} not found.", file.display()),
        ));
    }

    // Read file content
    let content = match std::fs::read_to_string(&file) {
        Ok(c) => c,
        Err(_) => {
            return Err(mozart_core::exit_code::bail(
                VALIDATE_FILE_ERROR,
                format!("{} is not readable.", file.display()),
            ));
        }
    };

    // Parse JSON syntax
    let json_value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return Err(mozart_core::exit_code::bail(
                VALIDATE_JSON_ERROR,
                format!("{} does not contain valid JSON: {e}", file.display()),
            ));
        }
    };

    // Run manifest validations
    let mut result = ValidationResult::new();
    validate_manifest(&json_value, args, &mut result);

    // Check lock file freshness
    let mut lock_errors: Vec<String> = Vec::new();
    let check_lock = should_check_lock(args, &json_value);
    if check_lock {
        check_lock_freshness(&content, &file, &mut lock_errors);
    }

    // Output results
    let check_publish = !args.no_check_publish;
    output_result(&file, &result, check_publish, check_lock, &lock_errors);

    // Validate dependencies' composer.json files
    let (dep_errors, dep_warnings) = if args.with_dependencies {
        let vendor_dir = file.parent().unwrap_or(Path::new(".")).join("vendor");
        if vendor_dir.exists() {
            validate_dependencies(&vendor_dir, args, console)
        } else {
            console
                .info("No vendor directory found. Run `mozart install` to install dependencies.");
            (0, 0)
        }
    } else {
        (0, 0)
    };

    let mut exit_code = compute_exit_code(
        &result,
        &lock_errors,
        check_publish,
        check_lock,
        args.strict,
    );

    // Merge dependency validation results into exit code (matching Composer behavior)
    if dep_errors > 0 {
        exit_code = exit_code.max(2);
    } else if dep_warnings > 0 && args.strict {
        exit_code = exit_code.max(1);
    }

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code));
    }

    Ok(())
}

// ─── Manifest validation ─────────────────────────────────────────────────────

fn validate_manifest(
    manifest: &serde_json::Value,
    args: &ValidateArgs,
    result: &mut ValidationResult,
) {
    let obj = match manifest.as_object() {
        Some(o) => o,
        None => {
            result
                .errors
                .push("composer.json must be a JSON object".to_string());
            return;
        }
    };

    check_name(obj, result);
    check_license(obj, result);

    if !args.no_check_version {
        check_version_field(obj, result);
    }

    check_package_type(obj, result);
    check_require_overlap(obj, result);
    check_provide_replace_overlap(obj, result);
    check_commit_references(obj, result);
    check_empty_psr_prefixes(obj, result);
    check_minimum_stability(obj, result);
    check_scripts_orphans(obj, result);
}

// ─── Individual checks ───────────────────────────────────────────────────────

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
            // Uppercase characters are a publish error
            if name.chars().any(|c| c.is_ascii_uppercase()) {
                let suggested = name
                    .split('/')
                    .map(mozart_core::validation::sanitize_package_name_component)
                    .collect::<Vec<_>>()
                    .join("/");
                result.publish_errors.push(format!(
                    "Name \"{name}\" does not match the best practice (e.g. lower-cased/with-dashes). \
                     We suggest using \"{suggested}\" instead. As such you will not be able to submit it to Packagist."
                ));
            }

            // Must contain a slash (vendor/package format)
            if !name.is_empty()
                && !mozart_core::validation::validate_package_name(name)
                && !name.contains('/')
            {
                result.errors.push(format!(
                    "The name \"{name}\" is invalid, it should be in the format \"vendor/package\"."
                ));
            }
        }
    }
}

/// Check the "license" field: warn if absent.
fn check_license(obj: &serde_json::Map<String, serde_json::Value>, result: &mut ValidationResult) {
    if obj.get("license").is_none() {
        result.warnings.push(
            "No license specified, it is recommended to do so. \
             For closed-source software you may use \"proprietary\" as license."
                .to_string(),
        );
    }
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
        && !mozart_core::validation::validate_stability(stability)
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
    let script_keys: std::collections::HashSet<&str> = obj
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

// ─── Dependency validation ───────────────────────────────────────────────

fn validate_dependencies(
    vendor_dir: &Path,
    args: &ValidateArgs,
    console: &mozart_core::console::Console,
) -> (u32, u32) {
    let mut dep_errors = 0u32;
    let mut dep_warnings = 0u32;
    let mut dep_count = 0u32;

    // Walk vendor/<vendor>/<package>/composer.json
    let Ok(vendors) = std::fs::read_dir(vendor_dir) else {
        return (0, 0);
    };

    for vendor_entry in vendors.flatten() {
        if !vendor_entry.path().is_dir() {
            continue;
        }
        // Skip non-package dirs (bin, composer, autoload files, etc.)
        let vendor_name = vendor_entry.file_name();
        let vendor_str = vendor_name.to_string_lossy();
        if vendor_str.starts_with('.') || vendor_str == "bin" || vendor_str == "composer" {
            continue;
        }

        let Ok(packages) = std::fs::read_dir(vendor_entry.path()) else {
            continue;
        };

        for pkg_entry in packages.flatten() {
            if !pkg_entry.path().is_dir() {
                continue;
            }

            let dep_composer = pkg_entry.path().join("composer.json");
            if !dep_composer.exists() {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&dep_composer) else {
                continue;
            };

            let Ok(json_value) = serde_json::from_str::<serde_json::Value>(&content) else {
                dep_errors += 1;
                let pkg_name =
                    format!("{}/{}", vendor_str, pkg_entry.file_name().to_string_lossy());
                eprintln!(
                    "{}",
                    console_format!(
                        "<warning>{pkg_name}: composer.json contains invalid JSON</warning>"
                    )
                );
                continue;
            };

            let mut result = ValidationResult::new();
            validate_manifest(&json_value, args, &mut result);

            dep_count += 1;

            if result.has_errors() || result.has_warnings() {
                let pkg_name =
                    format!("{}/{}", vendor_str, pkg_entry.file_name().to_string_lossy());

                for e in &result.errors {
                    eprintln!("{}", console_format!("<error>{pkg_name}: {e}</error>"));
                    dep_errors += 1;
                }
                for w in &result.warnings {
                    eprintln!("{}", console_format!("<warning>{pkg_name}: {w}</warning>"));
                    dep_warnings += 1;
                }
            }
        }
    }

    if dep_count > 0 {
        console.info(&format!(
            "Validated {} dependenc{}: {} error(s), {} warning(s)",
            dep_count,
            if dep_count == 1 { "y" } else { "ies" },
            dep_errors,
            dep_warnings
        ));
    }

    (dep_errors, dep_warnings)
}

// ─── Lock file freshness ─────────────────────────────────────────────────────

fn check_lock_freshness(
    composer_json_content: &str,
    composer_json_path: &Path,
    lock_errors: &mut Vec<String>,
) {
    let lock_path = composer_json_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("composer.lock");

    if !lock_path.exists() {
        // No lock file is not an error for validate — it's optional
        return;
    }

    match mozart_registry::lockfile::LockFile::read_from_file(&lock_path) {
        Ok(lock) => {
            if !lock.is_fresh(composer_json_content) {
                lock_errors.push(
                    "- The lock file is not up to date with the latest changes in composer.json, \
                     it is recommended that you run `mozart update` or `mozart update <package name>`."
                        .to_string(),
                );
            }
        }
        Err(e) => {
            lock_errors.push(format!("- The lock file could not be read: {e}"));
        }
    }
}

// ─── Output ──────────────────────────────────────────────────────────────────

fn output_result(
    file: &Path,
    result: &ValidationResult,
    check_publish: bool,
    check_lock: bool,
    lock_errors: &[String],
) {
    let name = file.display().to_string();

    // Print header message
    if result.has_errors() {
        eprintln!(
            "{}",
            console_format!(
                "<error>{name} is invalid, the following errors/warnings were found:</error>"
            )
        );
    } else if result.has_publish_errors() && check_publish {
        eprintln!(
            "{}",
            console_format!("<info>{name} is valid for simple usage with Composer but has</info>")
        );
        eprintln!(
            "{}",
            mozart_core::console::info(
                "strict errors that make it unable to be published as a package"
            )
        );
        eprintln!(
            "{}",
            mozart_core::console::warning(
                "See https://getcomposer.org/doc/04-schema.md for details on the schema"
            )
        );
    } else if result.has_warnings() {
        eprintln!(
            "{}",
            console_format!("<info>{name} is valid, but with a few warnings</info>")
        );
        eprintln!(
            "{}",
            mozart_core::console::warning(
                "See https://getcomposer.org/doc/04-schema.md for details on the schema"
            )
        );
    } else if !lock_errors.is_empty() {
        let kind = if check_lock { "errors" } else { "warnings" };
        println!(
            "{}",
            console_format!("<info>{name} is valid but your composer.lock has some {kind}</info>")
        );
    } else {
        println!("{}", console_format!("<info>{name} is valid</info>"));
    }

    // Collect error and warning message lines
    let mut all_errors: Vec<String> = Vec::new();
    let mut all_warnings: Vec<String> = Vec::new();

    if !result.errors.is_empty() {
        all_errors.push("# General errors".to_string());
        for e in &result.errors {
            all_errors.push(format!("- {e}"));
        }
    }

    if !result.warnings.is_empty() {
        all_warnings.push("# General warnings".to_string());
        for w in &result.warnings {
            all_warnings.push(format!("- {w}"));
        }
    }

    // Publish errors: shown as errors if check_publish is true
    if check_publish && !result.publish_errors.is_empty() {
        all_errors.push("# Publish errors".to_string());
        for e in &result.publish_errors {
            all_errors.push(format!("- {e}"));
        }
    }

    // Lock errors: shown as errors or warnings depending on check_lock
    if !lock_errors.is_empty() {
        if check_lock {
            all_errors.push("# Lock file errors".to_string());
            all_errors.extend_from_slice(lock_errors);
        } else {
            all_warnings.push("# Lock file warnings".to_string());
            all_warnings.extend_from_slice(lock_errors);
        }
    }

    // Print errors
    for msg in &all_errors {
        if msg.starts_with('#') {
            eprintln!("{}", mozart_core::console::error(msg));
        } else {
            eprintln!("{msg}");
        }
    }

    // Print warnings
    for msg in &all_warnings {
        if msg.starts_with('#') {
            eprintln!("{}", mozart_core::console::warning(msg));
        } else {
            eprintln!("{msg}");
        }
    }
}

// ─── Exit code ───────────────────────────────────────────────────────────────

/// Compute the exit code following Composer's convention:
/// 0 = valid, 1 = warnings (only with --strict), 2 = errors, 3 = file unreadable (handled earlier)
fn compute_exit_code(
    result: &ValidationResult,
    lock_errors: &[String],
    check_publish: bool,
    check_lock: bool,
    strict: bool,
) -> i32 {
    let has_errors = result.has_errors()
        || (check_publish && result.has_publish_errors())
        || (check_lock && !lock_errors.is_empty());

    if has_errors {
        return 2;
    }

    let has_warnings = result.has_warnings() || (!check_lock && !lock_errors.is_empty());

    if strict && has_warnings {
        return 1;
    }

    0
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_args() -> ValidateArgs {
        ValidateArgs {
            file: None,
            no_check_all: false,
            check_lock: false,
            no_check_lock: false,
            no_check_publish: false,
            no_check_version: false,
            with_dependencies: false,
            strict: false,
        }
    }

    fn parse_and_validate(json: &str, args: &ValidateArgs) -> ValidationResult {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut result = ValidationResult::new();
        validate_manifest(&value, args, &mut result);
        result
    }

    // ── check_name ─────────────────────────────────────────────────────────

    #[test]
    fn test_validate_missing_name_is_publish_error() {
        let json = r#"{"require": {"php": ">=8.1"}, "license": "MIT"}"#;
        let result = parse_and_validate(json, &make_args());
        assert!(result.errors.is_empty());
        assert!(!result.publish_errors.is_empty());
        assert!(result.publish_errors[0].contains("name property is not set"));
    }

    #[test]
    fn test_validate_uppercase_name_publish_error() {
        let json = r#"{"name": "Vendor/Package", "license": "MIT"}"#;
        let result = parse_and_validate(json, &make_args());
        assert!(!result.publish_errors.is_empty());
        assert!(result.publish_errors[0].contains("does not match the best practice"));
        assert!(result.publish_errors[0].contains("vendor/package"));
    }

    #[test]
    fn test_validate_uppercase_name_camel_case_to_dashes() {
        let json = r#"{"name": "MyCompany/MyLibrary", "license": "MIT"}"#;
        let result = parse_and_validate(json, &make_args());
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
        let result = parse_and_validate(json, &make_args());
        assert!(result.publish_errors.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_name_without_slash_is_error() {
        let json = r#"{"name": "novendor", "license": "MIT"}"#;
        let result = parse_and_validate(json, &make_args());
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("vendor/package"));
    }

    // ── check_license ──────────────────────────────────────────────────────

    #[test]
    fn test_validate_missing_license_warns() {
        let json = r#"{"name": "vendor/pkg"}"#;
        let result = parse_and_validate(json, &make_args());
        assert!(!result.warnings.is_empty());
        assert!(result.warnings.iter().any(|w| w.contains("No license")));
    }

    #[test]
    fn test_validate_present_license_no_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT"}"#;
        let result = parse_and_validate(json, &make_args());
        assert!(!result.warnings.iter().any(|w| w.contains("No license")));
    }

    // ── check_version_field ────────────────────────────────────────────────

    #[test]
    fn test_validate_version_field_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "version": "1.0.0"}"#;
        let result = parse_and_validate(json, &make_args());
        assert!(result.warnings.iter().any(|w| w.contains("version field")));
    }

    #[test]
    fn test_validate_no_check_version_suppresses_warning() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "version": "1.0.0"}"#;
        let mut args = make_args();
        args.no_check_version = true;
        let result = parse_and_validate(json, &args);
        assert!(!result.warnings.iter().any(|w| w.contains("version field")));
    }

    // ── check_package_type ─────────────────────────────────────────────────

    #[test]
    fn test_validate_deprecated_type_warns() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "type": "composer-installer"}"#;
        let result = parse_and_validate(json, &make_args());
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
        let result = parse_and_validate(json, &make_args());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("composer-installer"))
        );
    }

    // ── check_require_overlap ──────────────────────────────────────────────

    #[test]
    fn test_validate_require_overlap_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"monolog/monolog": "^3.0"},
            "require-dev": {"monolog/monolog": "^3.0"}
        }"#;
        let result = parse_and_validate(json, &make_args());
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
        let result = parse_and_validate(json, &make_args());
        assert!(!result.warnings.iter().any(|w| w.contains("required both")));
    }

    // ── check_provide_replace_overlap ──────────────────────────────────────

    #[test]
    fn test_validate_provide_replace_overlap_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"psr/log": "^3.0"},
            "provide": {"psr/log": "^3.0"}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("also listed in provide"))
        );
    }

    // ── check_commit_references ────────────────────────────────────────────

    #[test]
    fn test_validate_commit_ref_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"foo/bar": "dev-main#abc123"}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(result.warnings.iter().any(|w| w.contains("commit-ref")));
    }

    #[test]
    fn test_validate_normal_constraint_no_commit_warning() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "require": {"foo/bar": "^1.0"}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(!result.warnings.iter().any(|w| w.contains("commit-ref")));
    }

    // ── check_empty_psr_prefixes ───────────────────────────────────────────

    #[test]
    fn test_validate_empty_psr4_prefix_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "autoload": {"psr-4": {"": "src/"}}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(result.warnings.iter().any(|w| w.contains("psr-4")));
    }

    #[test]
    fn test_validate_empty_psr0_prefix_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "autoload": {"psr-0": {"": "src/"}}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(result.warnings.iter().any(|w| w.contains("psr-0")));
    }

    #[test]
    fn test_validate_named_psr4_prefix_no_warning() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "autoload": {"psr-4": {"Vendor\\Pkg\\": "src/"}}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(!result.warnings.iter().any(|w| w.contains("psr-4")));
    }

    // ── check_minimum_stability ────────────────────────────────────────────

    #[test]
    fn test_validate_invalid_stability_errors() {
        let json = r#"{"name": "vendor/pkg", "license": "MIT", "minimum-stability": "invalid"}"#;
        let result = parse_and_validate(json, &make_args());
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
            let result = parse_and_validate(&json, &make_args());
            assert!(
                !result
                    .errors
                    .iter()
                    .any(|e| e.contains("minimum-stability")),
                "stability '{stab}' should be valid"
            );
        }
    }

    // ── validate_manifest with non-object ──────────────────────────────────

    #[test]
    fn test_validate_non_object_json_errors() {
        let value = serde_json::json!([1, 2, 3]);
        let mut result = ValidationResult::new();
        validate_manifest(&value, &make_args(), &mut result);
        assert!(result.errors.iter().any(|e| e.contains("JSON object")));
    }

    // ── compute_exit_code ─────────────────────────────────────────────────

    #[test]
    fn test_compute_exit_code_no_issues() {
        let result = ValidationResult::new();
        assert_eq!(compute_exit_code(&result, &[], true, true, false), 0);
    }

    #[test]
    fn test_compute_exit_code_errors() {
        let mut result = ValidationResult::new();
        result.errors.push("some error".to_string());
        assert_eq!(compute_exit_code(&result, &[], true, true, false), 2);
    }

    #[test]
    fn test_compute_exit_code_publish_errors_counted() {
        let mut result = ValidationResult::new();
        result.publish_errors.push("publish error".to_string());
        assert_eq!(compute_exit_code(&result, &[], true, true, false), 2);
    }

    #[test]
    fn test_compute_exit_code_publish_errors_not_checked() {
        let mut result = ValidationResult::new();
        result.publish_errors.push("publish error".to_string());
        // check_publish = false → publish errors don't count
        assert_eq!(compute_exit_code(&result, &[], false, true, false), 0);
    }

    #[test]
    fn test_compute_exit_code_lock_errors_counted() {
        let result = ValidationResult::new();
        let lock_errors = vec!["lock stale".to_string()];
        assert_eq!(
            compute_exit_code(&result, &lock_errors, true, true, false),
            2
        );
    }

    #[test]
    fn test_compute_exit_code_lock_errors_not_checked() {
        let result = ValidationResult::new();
        let lock_errors = vec!["lock stale".to_string()];
        // check_lock = false → lock errors become warnings, not counted unless strict
        assert_eq!(
            compute_exit_code(&result, &lock_errors, true, false, false),
            0
        );
    }

    #[test]
    fn test_compute_exit_code_strict_warnings() {
        let mut result = ValidationResult::new();
        result.warnings.push("some warning".to_string());
        assert_eq!(compute_exit_code(&result, &[], true, true, true), 1);
    }

    #[test]
    fn test_compute_exit_code_warnings_not_strict() {
        let mut result = ValidationResult::new();
        result.warnings.push("some warning".to_string());
        assert_eq!(compute_exit_code(&result, &[], true, true, false), 0);
    }

    // ── check_lock_freshness ───────────────────────────────────────────────

    #[test]
    fn test_check_lock_freshness_no_lock_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let composer_json_path = dir.path().join("composer.json");
        let content = r#"{"name": "vendor/pkg", "require": {}}"#;
        std::fs::write(&composer_json_path, content).unwrap();

        let mut lock_errors: Vec<String> = Vec::new();
        check_lock_freshness(content, &composer_json_path, &mut lock_errors);
        // No lock file → no errors
        assert!(lock_errors.is_empty());
    }

    #[test]
    fn test_check_lock_freshness_fresh_lock() {
        use mozart_registry::lockfile::LockFile;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_json_path = dir.path().join("composer.json");
        let content = r#"{"name": "vendor/pkg", "require": {"php": ">=8.1"}}"#;
        std::fs::write(&composer_json_path, content).unwrap();

        let hash = LockFile::compute_content_hash(content).unwrap();
        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: hash,
            packages: vec![],
            packages_dev: Some(vec![]),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: None,
        };
        let lock_path = dir.path().join("composer.lock");
        lock.write_to_file(&lock_path).unwrap();

        let mut lock_errors: Vec<String> = Vec::new();
        check_lock_freshness(content, &composer_json_path, &mut lock_errors);
        assert!(
            lock_errors.is_empty(),
            "fresh lock should produce no errors"
        );
    }

    #[test]
    fn test_check_lock_freshness_stale_lock() {
        use mozart_registry::lockfile::LockFile;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let composer_json_path = dir.path().join("composer.json");
        let original_content = r#"{"name": "vendor/pkg", "require": {"php": ">=8.1"}}"#;
        let modified_content = r#"{"name": "vendor/pkg", "require": {"php": ">=8.2"}}"#;

        // Write original content
        std::fs::write(&composer_json_path, original_content).unwrap();

        // Create lock file based on original content
        let hash = LockFile::compute_content_hash(original_content).unwrap();
        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: hash,
            packages: vec![],
            packages_dev: Some(vec![]),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: None,
        };
        let lock_path = dir.path().join("composer.lock");
        lock.write_to_file(&lock_path).unwrap();

        // Now check against modified content (lock is stale)
        let mut lock_errors: Vec<String> = Vec::new();
        check_lock_freshness(modified_content, &composer_json_path, &mut lock_errors);
        assert!(
            !lock_errors.is_empty(),
            "stale lock should produce a lock error"
        );
        assert!(lock_errors[0].contains("not up to date"));
    }

    // ── check_scripts_orphans ──────────────────────────────────────────────

    #[test]
    fn test_validate_scripts_descriptions_orphan_warns() {
        let json = r#"{
            "name": "vendor/pkg",
            "license": "MIT",
            "scripts": {"build": "make build"},
            "scripts-descriptions": {"build": "Build the project", "nonexistent": "Ghost script"}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("nonexistent") && w.contains("scripts-descriptions")),
            "expected orphan warning for scripts-descriptions, got: {:?}",
            result.warnings
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
        let result = parse_and_validate(json, &make_args());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("ghost") && w.contains("scripts-aliases")),
            "expected orphan warning for scripts-aliases, got: {:?}",
            result.warnings
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
        let result = parse_and_validate(json, &make_args());
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.contains("scripts-descriptions") || w.contains("scripts-aliases")),
            "should produce no orphan warnings when all keys match, got: {:?}",
            result.warnings
        );
    }

    // ── should_check_lock ──────────────────────────────────────────────────

    #[test]
    fn test_should_check_lock_config_false_disables() {
        let args = make_args();
        let manifest = serde_json::json!({"config": {"lock": false}});
        assert!(!should_check_lock(&args, &manifest));
    }

    #[test]
    fn test_should_check_lock_config_false_overridden_by_flag() {
        let mut args = make_args();
        args.check_lock = true;
        let manifest = serde_json::json!({"config": {"lock": false}});
        assert!(should_check_lock(&args, &manifest));
    }

    #[test]
    fn test_should_check_lock_defaults_to_true() {
        let args = make_args();
        let manifest = serde_json::json!({"name": "vendor/pkg"});
        assert!(should_check_lock(&args, &manifest));
    }

    // ── Full manifest: valid package ───────────────────────────────────────

    #[test]
    fn test_validate_no_errors_on_valid_package() {
        let json = r#"{
            "name": "vendor/package",
            "description": "A test package",
            "license": "MIT",
            "require": {"php": ">=8.1"}
        }"#;
        let result = parse_and_validate(json, &make_args());
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert!(
            result.publish_errors.is_empty(),
            "publish errors: {:?}",
            result.publish_errors
        );
        // Only the version-field warning might appear — but we have no version field here
        assert!(
            !result.warnings.iter().any(|w| w.contains("version field")),
            "unexpected version warning"
        );
    }
}
