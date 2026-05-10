use crate::composer::Composer;
use clap::Args;
use mozart_core::config_validator::{ValidationResult, ValidatorOptions, validate_manifest};
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_core::package::RootPackageData;
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

/// Mirrors Composer's `($checkLock && lock-config) || --check-lock` formula.
fn should_check_lock(args: &ValidateArgs, config_lock: bool) -> bool {
    (!args.no_check_lock && config_lock) || args.check_lock
}

pub async fn execute(
    args: &ValidateArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
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

    // Load the Composer project state (optional — used for typed config,
    // locker, and the repository/installation managers). Mirrors
    // `ValidateCommand::createComposerInstance($file)`.
    let composer = Composer::try_load_from_file(&file).ok().flatten();

    // Determine whether to check the lock file using the typed config when
    // available, falling back to a raw JSON read for paths where the Composer
    // instance could not be initialised.
    let config_lock = composer
        .as_ref()
        .map(|c| c.config().lock)
        .unwrap_or_else(|| {
            json_value
                .get("config")
                .and_then(|c| c.get("lock"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        });

    // Run manifest validations
    let result = validate_manifest(&json_value, &options_from_args(args));

    // Check lock file freshness and surface missing-requirement diagnostics.
    let mut lock_errors: Vec<String> = Vec::new();
    let check_lock = should_check_lock(args, config_lock);
    if check_lock {
        let root_package = composer.as_ref().map(|c| c.package());
        check_lock_freshness(&content, &file, root_package, &mut lock_errors);
    }

    // Output results
    let check_publish = !args.no_check_publish;
    let file_name = file.display().to_string();
    output_result(
        io.clone(),
        &file_name,
        &result,
        check_publish,
        check_lock,
        &lock_errors,
    );

    // Validate dependencies' composer.json files
    let (dep_errors, dep_warnings) = if args.with_dependencies {
        let vendor_dir = file.parent().unwrap_or(Path::new(".")).join("vendor");
        if let Some(comp) = &composer {
            validate_dependencies(comp, args, io.clone())
        } else if vendor_dir.exists() {
            validate_dependencies_vendor_walk(&vendor_dir, args, io.clone())
        } else {
            io.lock()
                .unwrap()
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

/// Walk the installed packages via `RepositoryManager` + `InstallationManager`,
/// mirroring Composer's `--with-dependencies` path. Skips metapackages.
fn validate_dependencies(
    composer: &Composer,
    args: &ValidateArgs,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> (u32, u32) {
    let mut dep_errors = 0u32;
    let mut dep_warnings = 0u32;

    for package in composer
        .repository_manager()
        .local_repository()
        .get_canonical_packages()
    {
        // Mirrors Composer: `if ($package->getType() === 'metapackage') { continue; }`
        if package.package_type() == Some("metapackage") {
            continue;
        }

        let Some(install_path) = composer.installation_manager().get_install_path(package) else {
            continue;
        };

        let dep_composer = install_path.join("composer.json");
        if !dep_composer.exists() {
            continue;
        }

        let Ok(dep_content) = std::fs::read_to_string(&dep_composer) else {
            continue;
        };

        let dep_result = match serde_json::from_str::<serde_json::Value>(&dep_content) {
            Ok(json_value) => validate_manifest(&json_value, &options_from_args(args)),
            Err(_) => {
                // Invalid JSON — report as error using outputResult
                let mut err_result = ValidationResult::new();
                err_result
                    .errors
                    .push("composer.json contains invalid JSON".to_string());
                err_result
            }
        };

        if dep_result.has_errors() {
            dep_errors += dep_result.errors.len() as u32;
        }
        if dep_result.has_warnings() {
            dep_warnings += dep_result.warnings.len() as u32;
        }

        // Per-dep rendering — same header format as the root file
        output_result(
            io.clone(),
            package.pretty_name(),
            &dep_result,
            false, // check_publish: false for deps, matching Composer
            false, // check_lock: no lock checking for deps
            &[],
        );
    }

    (dep_errors, dep_warnings)
}

/// Fallback vendor walk used when a `Composer` instance is unavailable.
/// Iterates `vendor/<vendor>/<package>/composer.json` directly.
fn validate_dependencies_vendor_walk(
    vendor_dir: &Path,
    args: &ValidateArgs,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> (u32, u32) {
    let mut dep_errors = 0u32;
    let mut dep_warnings = 0u32;

    let Ok(vendors) = std::fs::read_dir(vendor_dir) else {
        return (0, 0);
    };

    for vendor_entry in vendors.flatten() {
        if !vendor_entry.path().is_dir() {
            continue;
        }
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

            let pkg_name = format!("{}/{}", vendor_str, pkg_entry.file_name().to_string_lossy());

            let dep_result = match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(json_value) => validate_manifest(&json_value, &options_from_args(args)),
                Err(_) => {
                    let mut err_result = ValidationResult::new();
                    err_result
                        .errors
                        .push("composer.json contains invalid JSON".to_string());
                    err_result
                }
            };

            if dep_result.has_errors() {
                dep_errors += dep_result.errors.len() as u32;
            }
            if dep_result.has_warnings() {
                dep_warnings += dep_result.warnings.len() as u32;
            }

            output_result(io.clone(), &pkg_name, &dep_result, false, false, &[]);
        }
    }

    (dep_errors, dep_warnings)
}

/// Check lock-file freshness and surface missing-requirement diagnostics.
///
/// Mirrors Composer's sequence in `ValidateCommand::execute`:
/// 1. `$locker->isLocked() && !$locker->isFresh()` → push stale-lock error.
/// 2. `$locker->getMissingRequirementInfo($composer->getPackage(), true)` → push
///    any missing-requirement bullets when the root package is available.
fn check_lock_freshness(
    composer_json_content: &str,
    composer_json_path: &Path,
    root_package: Option<&RootPackageData>,
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

    match mozart_core::repository::lockfile::LockFile::read_from_file(&lock_path) {
        Ok(lock) => {
            if !lock.is_fresh(composer_json_content) {
                lock_errors.push(
                    "- The lock file is not up to date with the latest changes in composer.json, \
                     it is recommended that you run `mozart update` or `mozart update <package name>`."
                        .to_string(),
                );
            }
            // Surface any missing-requirement diagnostics from the lock file,
            // mirroring `$locker->getMissingRequirementInfo($composer->getPackage(), true)`.
            if let Some(pkg) = root_package {
                let missing = lock.get_missing_requirement_info(pkg, true);
                lock_errors.extend(missing);
            }
        }
        Err(e) => {
            lock_errors.push(format!("- The lock file could not be read: {e}"));
        }
    }
}

/// Render the validation result for one file/package to the console.
/// Mirrors Composer's `ValidateCommand::outputResult()`.
///
/// `name` is either the file path (root file) or the package's pretty name
/// (dependency), matching how Composer calls `outputResult($io, $file, …)`
/// for the root and `outputResult($io, $package->getPrettyName(), …)` for deps.
fn output_result(
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    name: &str,
    result: &ValidationResult,
    check_publish: bool,
    check_lock: bool,
    lock_errors: &[String],
) {
    // Print header message
    if result.has_errors() {
        io.lock().unwrap().error(&console_format!(
            "<error>{name} is invalid, the following errors/warnings were found:</error>"
        ));
    } else if result.has_publish_errors() && check_publish {
        io.lock().unwrap().info(&console_format!(
            "<info>{name} is valid for simple usage with Composer but has</info>"
        ));
        io.lock().unwrap().info(&console_format!(
            "<info>strict errors that make it unable to be published as a package</info>"
        ));
        io.lock().unwrap().info(&console_format!(
            "<warning>See https://getcomposer.org/doc/04-schema.md for details on the schema</warning>"
        ));
    } else if result.has_warnings() {
        io.lock().unwrap().info(&console_format!(
            "<info>{name} is valid, but with a few warnings</info>"
        ));
        io.lock().unwrap().info(&console_format!(
            "<warning>See https://getcomposer.org/doc/04-schema.md for details on the schema</warning>"
        ));
    } else if !lock_errors.is_empty() {
        let kind = if check_lock { "errors" } else { "warnings" };
        console_writeln!(
            io,
            "<info>{name} is valid but your composer.lock has some {kind}</info>",
        );
    } else {
        console_writeln!(io, "<info>{name} i valid</info>");
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
        io.lock().unwrap().error(msg);
    }

    for msg in &all_warnings {
        if msg.starts_with('#') {
            io.lock()
                .unwrap()
                .info(&console_format!("<warning>{msg}</warning>"));
        } else {
            io.lock().unwrap().info(msg);
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
        check_lock_freshness(content, &composer_json_path, None, &mut lock_errors);
        // No lock file → no errors
        assert!(lock_errors.is_empty());
    }

    #[test]
    fn test_check_lock_freshness_fresh_lock() {
        use mozart_core::repository::lockfile::LockFile;
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
        check_lock_freshness(content, &composer_json_path, None, &mut lock_errors);
        assert!(
            lock_errors.is_empty(),
            "fresh lock should produce no errors"
        );
    }

    #[test]
    fn test_check_lock_freshness_stale_lock() {
        use mozart_core::repository::lockfile::LockFile;
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
        check_lock_freshness(
            modified_content,
            &composer_json_path,
            None,
            &mut lock_errors,
        );
        assert!(
            !lock_errors.is_empty(),
            "stale lock should produce a lock error"
        );
        assert!(lock_errors[0].contains("not up to date"));
    }

    #[test]
    fn test_should_check_lock_config_false_disables() {
        let args = make_args();
        assert!(!should_check_lock(&args, false));
    }

    #[test]
    fn test_should_check_lock_config_false_overridden_by_flag() {
        let mut args = make_args();
        args.check_lock = true;
        assert!(should_check_lock(&args, false));
    }

    #[test]
    fn test_should_check_lock_defaults_to_true() {
        let args = make_args();
        assert!(should_check_lock(&args, true));
    }
}
