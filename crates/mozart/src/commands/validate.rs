use clap::Args;
use mozart_core::config_validator::{ValidationResult, ValidatorOptions, validate_manifest};
use mozart_core::console_format;
use mozart_core::console_writeln;
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

fn options_from_args(args: &ValidateArgs) -> ValidatorOptions {
    ValidatorOptions {
        check_version: !args.no_check_version,
    }
}

fn should_check_lock(args: &ValidateArgs, manifest: &serde_json::Value) -> bool {
    let config_lock_enabled = manifest
        .get("config")
        .and_then(|c| c.get("lock"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    (!args.no_check_lock && config_lock_enabled) || args.check_lock
}

pub async fn execute(
    args: &ValidateArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

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
    let result = validate_manifest(&json_value, &options_from_args(args));

    // Check lock file freshness
    let mut lock_errors: Vec<String> = Vec::new();
    let check_lock = should_check_lock(args, &json_value);
    if check_lock {
        check_lock_freshness(&content, &file, &mut lock_errors);
    }

    // Output results
    let check_publish = !args.no_check_publish;
    output_result(
        console,
        &file,
        &result,
        check_publish,
        check_lock,
        &lock_errors,
    );

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
                console.info(&console_format!(
                    "<warning>{pkg_name}: composer.json contains invalid JSON</warning>"
                ));
                continue;
            };

            let result = validate_manifest(&json_value, &options_from_args(args));

            dep_count += 1;

            if result.has_errors() || result.has_warnings() {
                let pkg_name =
                    format!("{}/{}", vendor_str, pkg_entry.file_name().to_string_lossy());

                for e in &result.errors {
                    console.error(&console_format!("<error>{pkg_name}: {e}</error>"));
                    dep_errors += 1;
                }
                for w in &result.warnings {
                    console.info(&console_format!("<warning>{pkg_name}: {w}</warning>"));
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

fn output_result(
    console: &mozart_core::console::Console,
    file: &Path,
    result: &ValidationResult,
    check_publish: bool,
    check_lock: bool,
    lock_errors: &[String],
) {
    let name = file.display().to_string();

    // Print header message
    if result.has_errors() {
        console.error(&console_format!(
            "<error>{name} is invalid, the following errors/warnings were found:</error>"
        ));
    } else if result.has_publish_errors() && check_publish {
        console.info(&console_format!(
            "<info>{name} is valid for simple usage with Composer but has</info>"
        ));
        console.info(&console_format!(
            "<info>strict errors that make it unable to be published as a package</info>"
        ));
        console.info(&console_format!(
            "<warning>See https://getcomposer.org/doc/04-schema.md for details on the schema</warning>"
        ));
    } else if result.has_warnings() {
        console.info(&console_format!(
            "<info>{name} is valid, but with a few warnings</info>"
        ));
        console.info(&console_format!(
            "<warning>See https://getcomposer.org/doc/04-schema.md for details on the schema</warning>"
        ));
    } else if !lock_errors.is_empty() {
        let kind = if check_lock { "errors" } else { "warnings" };
        console_writeln!(
            console,
            &console_format!("<info>{name} is valid but your composer.lock has some {kind}</info>"),
        );
    } else {
        console_writeln!(console, &console_format!("<info>{name} is valid</info>"),);
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
        console.error(msg);
    }

    for msg in &all_warnings {
        if msg.starts_with('#') {
            console.info(&console_format!("<warning>{msg}</warning>"));
        } else {
            console.info(msg);
        }
    }
}

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
}
