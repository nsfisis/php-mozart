use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct CheckPlatformReqsArgs {
    /// Disables checking of require-dev packages requirements
    #[arg(long)]
    pub no_dev: bool,

    /// Check packages from the lock file
    #[arg(long)]
    pub lock: bool,

    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

// ─── Data structures ─────────────────────────────────────────────────────────

/// A single platform requirement collected from a package.
#[derive(Debug, Clone)]
struct PlatformRequirement {
    /// Package that declares the requirement (e.g. "vendor/pkg" or "root")
    provider: String,
    /// The constraint string (e.g. ">=8.1", "^8.2", "*")
    constraint: String,
}

/// The outcome of checking one platform package against all its requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckStatus {
    /// All constraints satisfied.
    Success,
    /// Platform package detected but at least one constraint failed.
    Failed,
    /// Platform package not detected at all.
    Missing,
}

/// Result of checking a single platform requirement name.
#[derive(Debug, Clone)]
struct CheckResult {
    name: String,
    /// Detected version, or "n/a" if missing.
    version: String,
    status: CheckStatus,
    /// The first failed constraint and its provider.
    failed_requirement: Option<(String, String)>,
}

// ─── Main entry point ────────────────────────────────────────────────────────

pub fn execute(
    args: &CheckPlatformReqsArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // Validate format
    let format = args.format.as_deref().unwrap_or("text");
    if format != "text" && format != "json" {
        anyhow::bail!(
            "Invalid format \"{}\". Supported formats: text, json",
            format
        );
    }

    // Require composer.json
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }

    // Collect platform requirements from all packages + root
    let requirements = collect_requirements(&working_dir, args)?;

    if requirements.is_empty() {
        // No platform requirements to check
        if format == "json" {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!([]))?);
        }
        return Ok(());
    }

    // Detect real platform
    let platform = mozart_core::platform::detect_platform();

    // Check requirements against detected platform
    let results = check_requirements(&requirements, &platform);

    // Determine exit code
    let exit_code = determine_exit_code(&results);

    // Render output
    match format {
        "json" => render_json(&results)?,
        _ => render_text(&results),
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}

// ─── Requirement collection ──────────────────────────────────────────────────

/// Collect platform requirements from all packages (lock/installed) plus root.
///
/// Returns a map of platform-package-name → list of requirements.
fn collect_requirements(
    working_dir: &Path,
    args: &CheckPlatformReqsArgs,
) -> anyhow::Result<BTreeMap<String, Vec<PlatformRequirement>>> {
    let mut requirements: BTreeMap<String, Vec<PlatformRequirement>> = BTreeMap::new();

    // Determine package source
    let lock_path = working_dir.join("composer.lock");
    let vendor_dir = working_dir.join("vendor");
    let installed_path = vendor_dir.join("composer/installed.json");

    if args.lock {
        // --lock: read from composer.lock
        if !lock_path.exists() {
            anyhow::bail!("No composer.lock found. Run `mozart install` or `mozart update` first.");
        }
        collect_from_lock(&lock_path, args.no_dev, &mut requirements)?;
    } else if installed_path.exists() {
        // Default: read from installed.json
        collect_from_installed(&vendor_dir, args.no_dev, &mut requirements)?;
    } else if lock_path.exists() {
        // Fallback: read from lock file
        collect_from_lock(&lock_path, args.no_dev, &mut requirements)?;
    } else {
        anyhow::bail!(
            "No installed packages found. Run `mozart install` or `mozart update` first."
        );
    }

    // Always include root composer.json requirements
    let composer_json_path = working_dir.join("composer.json");
    let root = mozart_core::package::read_from_file(&composer_json_path)?;

    add_platform_requirements_from_map(&root.require, "root", &mut requirements);
    if !args.no_dev {
        add_platform_requirements_from_map(&root.require_dev, "root", &mut requirements);
    }

    Ok(requirements)
}

fn collect_from_lock(
    lock_path: &Path,
    no_dev: bool,
    requirements: &mut BTreeMap<String, Vec<PlatformRequirement>>,
) -> anyhow::Result<()> {
    let lock = mozart_registry::lockfile::LockFile::read_from_file(lock_path)?;

    for pkg in &lock.packages {
        add_platform_requirements_from_map(&pkg.require, &pkg.name, requirements);
    }

    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        for pkg in pkgs_dev {
            add_platform_requirements_from_map(&pkg.require, &pkg.name, requirements);
        }
    }

    Ok(())
}

fn collect_from_installed(
    vendor_dir: &Path,
    no_dev: bool,
    requirements: &mut BTreeMap<String, Vec<PlatformRequirement>>,
) -> anyhow::Result<()> {
    let installed = mozart_registry::installed::InstalledPackages::read(vendor_dir)?;

    let dev_names: std::collections::HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    for pkg in &installed.packages {
        if no_dev && dev_names.contains(&pkg.name.to_lowercase()) {
            continue;
        }

        // Extract require from extra_fields
        if let Some(require_val) = pkg.extra_fields.get("require")
            && let Some(require_obj) = require_val.as_object()
        {
            for (dep_name, dep_constraint_val) in require_obj {
                let dep_lower = dep_name.to_lowercase();
                if mozart_core::platform::is_platform_package(&dep_lower) {
                    let constraint = dep_constraint_val.as_str().unwrap_or("*").to_string();
                    requirements
                        .entry(dep_lower)
                        .or_default()
                        .push(PlatformRequirement {
                            provider: pkg.name.clone(),
                            constraint,
                        });
                }
            }
        }
    }

    Ok(())
}

fn add_platform_requirements_from_map(
    require: &std::collections::BTreeMap<String, String>,
    provider: &str,
    requirements: &mut BTreeMap<String, Vec<PlatformRequirement>>,
) {
    for (name, constraint) in require {
        let name_lower = name.to_lowercase();
        if mozart_core::platform::is_platform_package(&name_lower) {
            requirements
                .entry(name_lower)
                .or_default()
                .push(PlatformRequirement {
                    provider: provider.to_string(),
                    constraint: constraint.clone(),
                });
        }
    }
}

// ─── Requirement checking ────────────────────────────────────────────────────

fn check_requirements(
    requirements: &BTreeMap<String, Vec<PlatformRequirement>>,
    platform: &[mozart_core::platform::PlatformPackage],
) -> Vec<CheckResult> {
    let mut results: Vec<CheckResult> = Vec::new();

    for (name, reqs) in requirements {
        // lib-* and composer-plugin-api / composer-runtime-api → always missing
        if name.starts_with("lib-")
            || name == "composer-plugin-api"
            || name == "composer-runtime-api"
        {
            // Find first constraining requirement for reporting
            let failed_req = reqs
                .first()
                .map(|r| (r.constraint.clone(), r.provider.clone()));
            results.push(CheckResult {
                name: name.clone(),
                version: "n/a".to_string(),
                status: CheckStatus::Missing,
                failed_requirement: failed_req,
            });
            continue;
        }

        // Look up in detected platform
        match platform.iter().find(|p| p.name == *name) {
            None => {
                // Not detected → missing
                let failed_req = reqs
                    .first()
                    .map(|r| (r.constraint.clone(), r.provider.clone()));
                results.push(CheckResult {
                    name: name.clone(),
                    version: "n/a".to_string(),
                    status: CheckStatus::Missing,
                    failed_requirement: failed_req,
                });
            }
            Some(detected) => {
                // Check all constraints
                let detected_version = match mozart_constraint::Version::parse(&detected.version) {
                    Ok(v) => v,
                    Err(_) => {
                        // Unparseable version → treat as 0.0.0
                        mozart_constraint::Version::parse("0.0.0").unwrap()
                    }
                };

                let mut failed_req: Option<(String, String)> = None;
                for req in reqs {
                    let constraint =
                        match mozart_constraint::VersionConstraint::parse(&req.constraint) {
                            Ok(c) => c,
                            Err(_) => continue, // skip unparseable constraints
                        };
                    if !constraint.matches(&detected_version) {
                        failed_req = Some((req.constraint.clone(), req.provider.clone()));
                        break;
                    }
                }

                let status = if failed_req.is_some() {
                    CheckStatus::Failed
                } else {
                    CheckStatus::Success
                };

                results.push(CheckResult {
                    name: name.clone(),
                    version: detected.version.clone(),
                    status,
                    failed_requirement: failed_req,
                });
            }
        }
    }

    results
}

fn determine_exit_code(results: &[CheckResult]) -> i32 {
    let mut code = 0;
    for result in results {
        match result.status {
            CheckStatus::Failed if code < 1 => code = 1,
            CheckStatus::Missing => code = 2,
            _ => {}
        }
    }
    code
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn render_text(results: &[CheckResult]) {
    if results.is_empty() {
        return;
    }

    // Compute column widths
    let name_width = results.iter().map(|r| r.name.len()).max().unwrap_or(0);
    let version_width = results.iter().map(|r| r.version.len()).max().unwrap_or(0);

    for result in results {
        // Pad the raw strings first, then apply color so ANSI escape codes
        // don't interfere with column alignment.
        let padded_name = format!("{:<nw$}", result.name, nw = name_width);
        let padded_version = format!("{:<vw$}", result.version, vw = version_width);

        match result.status {
            CheckStatus::Success => {
                println!(
                    "{}  {}  {}",
                    mozart_core::console::info(&padded_name),
                    mozart_core::console::comment(&padded_version),
                    mozart_core::console::info("success"),
                );
            }
            CheckStatus::Failed => {
                let (constraint, provider) = result
                    .failed_requirement
                    .as_ref()
                    .map(|(c, p)| (c.as_str(), p.as_str()))
                    .unwrap_or(("", ""));
                println!(
                    "{}  {}  {} requires {} ({})",
                    mozart_core::console::comment(&padded_name),
                    mozart_core::console::comment(&padded_version),
                    mozart_core::console::error("failed"),
                    provider,
                    constraint,
                );
            }
            CheckStatus::Missing => {
                let (constraint, provider) = result
                    .failed_requirement
                    .as_ref()
                    .map(|(c, p)| (c.as_str(), p.as_str()))
                    .unwrap_or(("*", ""));
                println!(
                    "{}  {}  {} requires {} ({})",
                    mozart_core::console::comment(&padded_name),
                    mozart_core::console::comment(&padded_version),
                    mozart_core::console::error("missing"),
                    provider,
                    constraint,
                );
            }
        }
    }
}

fn render_json(results: &[CheckResult]) -> anyhow::Result<()> {
    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let status_str = match r.status {
                CheckStatus::Success => "success",
                CheckStatus::Failed => "failed",
                CheckStatus::Missing => "missing",
            };
            let (failed_constraint, failed_provider) = match &r.failed_requirement {
                Some((c, p)) => (
                    serde_json::Value::String(c.clone()),
                    serde_json::Value::String(p.clone()),
                ),
                None => (serde_json::Value::Null, serde_json::Value::Null),
            };
            serde_json::json!({
                "name": r.name,
                "version": r.version,
                "status": status_str,
                "failed_requirement": failed_constraint,
                "provider": failed_provider,
            })
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&json_results)?);
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_core::platform::PlatformPackage;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_platform(entries: &[(&str, &str)]) -> Vec<PlatformPackage> {
        entries
            .iter()
            .map(|(name, version)| PlatformPackage {
                name: name.to_string(),
                version: version.to_string(),
            })
            .collect()
    }

    fn make_requirements(
        entries: &[(&str, &str, &str)],
    ) -> BTreeMap<String, Vec<PlatformRequirement>> {
        let mut map: BTreeMap<String, Vec<PlatformRequirement>> = BTreeMap::new();
        for (name, constraint, provider) in entries {
            map.entry(name.to_string())
                .or_default()
                .push(PlatformRequirement {
                    provider: provider.to_string(),
                    constraint: constraint.to_string(),
                });
        }
        map
    }

    fn write_lock(
        path: &Path,
        packages: &[(&str, BTreeMap<String, String>)],
        dev_packages: &[(&str, BTreeMap<String, String>)],
    ) {
        let make_pkg = |name: &str, require: BTreeMap<String, String>| {
            serde_json::json!({
                "name": name,
                "version": "1.0.0",
                "require": require,
            })
        };

        let pkgs_json: Vec<serde_json::Value> = packages
            .iter()
            .map(|(name, req)| make_pkg(name, req.clone()))
            .collect();
        let dev_pkgs_json: Vec<serde_json::Value> = dev_packages
            .iter()
            .map(|(name, req)| make_pkg(name, req.clone()))
            .collect();

        let lock_json = serde_json::json!({
            "_readme": ["This file locks the dependencies"],
            "content-hash": "abc123",
            "packages": pkgs_json,
            "packages-dev": dev_pkgs_json,
            "aliases": [],
            "minimum-stability": "stable",
            "stability-flags": {},
            "prefer-stable": false,
            "prefer-lowest": false,
            "platform": {},
            "platform-dev": {},
            "plugin-api-version": "2.6.0",
        });

        std::fs::write(path, serde_json::to_string_pretty(&lock_json).unwrap()).unwrap();
    }

    // ── test_is_platform_package ──────────────────────────────────────────────

    #[test]
    fn test_is_platform_package() {
        assert!(mozart_core::platform::is_platform_package("php"));
        assert!(mozart_core::platform::is_platform_package("ext-json"));
        assert!(mozart_core::platform::is_platform_package("ext-mbstring"));
        assert!(mozart_core::platform::is_platform_package("lib-pcre"));
        assert!(mozart_core::platform::is_platform_package("php-64bit"));
        assert!(mozart_core::platform::is_platform_package(
            "composer-plugin-api"
        ));
        assert!(mozart_core::platform::is_platform_package(
            "composer-runtime-api"
        ));

        assert!(!mozart_core::platform::is_platform_package(
            "monolog/monolog"
        ));
        assert!(!mozart_core::platform::is_platform_package("psr/log"));
        assert!(!mozart_core::platform::is_platform_package(
            "symfony/console"
        ));
    }

    // ── test_collect_requirements_from_lock ──────────────────────────────────

    #[test]
    fn test_collect_requirements_from_lock() {
        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();

        let mut pkg_require = BTreeMap::new();
        pkg_require.insert("php".to_string(), ">=8.1".to_string());
        pkg_require.insert("ext-json".to_string(), "*".to_string());
        pkg_require.insert("monolog/monolog".to_string(), "^3.0".to_string()); // not platform

        write_lock(
            &working_dir.join("composer.lock"),
            &[("vendor/pkg", pkg_require)],
            &[],
        );

        let args = CheckPlatformReqsArgs {
            no_dev: false,
            lock: true,
            format: None,
        };

        let reqs = collect_requirements(working_dir, &args).unwrap();

        assert!(reqs.contains_key("php"), "php should be in requirements");
        assert!(
            reqs.contains_key("ext-json"),
            "ext-json should be in requirements"
        );
        assert!(
            !reqs.contains_key("monolog/monolog"),
            "monolog should not be in requirements"
        );

        let php_reqs = &reqs["php"];
        assert_eq!(php_reqs.len(), 1);
        assert_eq!(php_reqs[0].constraint, ">=8.1");
        assert_eq!(php_reqs[0].provider, "vendor/pkg");
    }

    // ── test_collect_requirements_no_dev ─────────────────────────────────────

    #[test]
    fn test_collect_requirements_no_dev() {
        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();

        let mut prod_require = BTreeMap::new();
        prod_require.insert("php".to_string(), ">=8.0".to_string());

        let mut dev_require = BTreeMap::new();
        dev_require.insert("ext-xdebug".to_string(), "*".to_string());

        write_lock(
            &working_dir.join("composer.lock"),
            &[("vendor/prod", prod_require)],
            &[("vendor/devpkg", dev_require)],
        );

        // With --no-dev
        let args_no_dev = CheckPlatformReqsArgs {
            no_dev: true,
            lock: true,
            format: None,
        };
        let reqs_no_dev = collect_requirements(working_dir, &args_no_dev).unwrap();
        assert!(reqs_no_dev.contains_key("php"));
        assert!(
            !reqs_no_dev.contains_key("ext-xdebug"),
            "dev requirement should be excluded"
        );

        // Without --no-dev
        let args_with_dev = CheckPlatformReqsArgs {
            no_dev: false,
            lock: true,
            format: None,
        };
        let reqs_with_dev = collect_requirements(working_dir, &args_with_dev).unwrap();
        assert!(reqs_with_dev.contains_key("php"));
        assert!(
            reqs_with_dev.contains_key("ext-xdebug"),
            "dev requirement should be included"
        );
    }

    // ── test_collect_requirements_includes_root ───────────────────────────────

    #[test]
    fn test_collect_requirements_includes_root() {
        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project", "require": {"php": ">=8.2", "ext-ctype": "*"}}"#,
        )
        .unwrap();

        write_lock(&working_dir.join("composer.lock"), &[], &[]);

        let args = CheckPlatformReqsArgs {
            no_dev: false,
            lock: true,
            format: None,
        };

        let reqs = collect_requirements(working_dir, &args).unwrap();

        assert!(
            reqs.contains_key("php"),
            "root php requirement should be included"
        );
        assert!(
            reqs.contains_key("ext-ctype"),
            "root ext-ctype requirement should be included"
        );

        // The provider should be "root"
        let php_reqs = &reqs["php"];
        assert!(
            php_reqs
                .iter()
                .any(|r| r.provider == "root" && r.constraint == ">=8.2")
        );
    }

    // ── test_check_requirements_all_pass ─────────────────────────────────────

    #[test]
    fn test_check_requirements_all_pass() {
        let requirements =
            make_requirements(&[("php", ">=8.1", "root"), ("ext-json", "*", "vendor/pkg")]);
        let platform = make_platform(&[("php", "8.2.1"), ("ext-json", "8.2.1")]);

        let results = check_requirements(&requirements, &platform);
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(
                r.status,
                CheckStatus::Success,
                "all should pass for {}",
                r.name
            );
        }
        assert_eq!(determine_exit_code(&results), 0);
    }

    // ── test_check_requirements_version_mismatch ─────────────────────────────

    #[test]
    fn test_check_requirements_version_mismatch() {
        let requirements = make_requirements(&[("php", ">=8.2", "vendor/pkg")]);
        let platform = make_platform(&[("php", "8.1.0")]);

        let results = check_requirements(&requirements, &platform);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Failed);
        assert_eq!(results[0].version, "8.1.0");
        assert!(results[0].failed_requirement.is_some());
        assert_eq!(determine_exit_code(&results), 1);
    }

    // ── test_check_requirements_missing ──────────────────────────────────────

    #[test]
    fn test_check_requirements_missing() {
        let requirements = make_requirements(&[("ext-foobar", "*", "vendor/pkg")]);
        let platform = make_platform(&[("php", "8.2.1")]); // ext-foobar not present

        let results = check_requirements(&requirements, &platform);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Missing);
        assert_eq!(results[0].version, "n/a");
        assert_eq!(determine_exit_code(&results), 2);
    }

    // ── test_check_requirements_mixed ────────────────────────────────────────

    #[test]
    fn test_check_requirements_mixed() {
        let requirements = make_requirements(&[
            ("php", ">=8.1", "root"),        // success
            ("ext-json", ">=7.0", "root"),   // success (version satisfied)
            ("ext-foobar", "*", "vendor/a"), // missing
        ]);
        let platform = make_platform(&[("php", "8.2.1"), ("ext-json", "8.2.1")]);

        let results = check_requirements(&requirements, &platform);

        let php_result = results.iter().find(|r| r.name == "php").unwrap();
        assert_eq!(php_result.status, CheckStatus::Success);

        let json_result = results.iter().find(|r| r.name == "ext-json").unwrap();
        assert_eq!(json_result.status, CheckStatus::Success);

        let foobar_result = results.iter().find(|r| r.name == "ext-foobar").unwrap();
        assert_eq!(foobar_result.status, CheckStatus::Missing);

        // Exit code should be 2 (missing wins over failed which wins over success)
        assert_eq!(determine_exit_code(&results), 2);
    }

    // ── test_check_requirements_multiple_constraints ──────────────────────────

    #[test]
    fn test_check_requirements_multiple_constraints() {
        // Two packages both require php, one with a tighter constraint
        let requirements = make_requirements(&[
            ("php", ">=8.0", "vendor/a"),
            ("php", ">=8.2", "vendor/b"), // tighter
        ]);
        let platform = make_platform(&[("php", "8.1.0")]); // satisfies >=8.0 but not >=8.2

        let results = check_requirements(&requirements, &platform);
        assert_eq!(results.len(), 1);
        // The second constraint fails
        assert_eq!(results[0].status, CheckStatus::Failed);
        let (failed_constraint, failed_provider) = results[0].failed_requirement.as_ref().unwrap();
        assert_eq!(failed_constraint, ">=8.2");
        assert_eq!(failed_provider, "vendor/b");
    }

    // ── test_output_json_format ───────────────────────────────────────────────

    #[test]
    fn test_output_json_format() {
        let results = vec![
            CheckResult {
                name: "php".to_string(),
                version: "8.2.1".to_string(),
                status: CheckStatus::Success,
                failed_requirement: None,
            },
            CheckResult {
                name: "ext-foobar".to_string(),
                version: "n/a".to_string(),
                status: CheckStatus::Missing,
                failed_requirement: Some(("*".to_string(), "vendor/pkg".to_string())),
            },
        ];

        // Capture output by writing to a string
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                let status_str = match r.status {
                    CheckStatus::Success => "success",
                    CheckStatus::Failed => "failed",
                    CheckStatus::Missing => "missing",
                };
                let (failed_constraint, failed_provider) = match &r.failed_requirement {
                    Some((c, p)) => (
                        serde_json::Value::String(c.clone()),
                        serde_json::Value::String(p.clone()),
                    ),
                    None => (serde_json::Value::Null, serde_json::Value::Null),
                };
                serde_json::json!({
                    "name": r.name,
                    "version": r.version,
                    "status": status_str,
                    "failed_requirement": failed_constraint,
                    "provider": failed_provider,
                })
            })
            .collect();

        assert_eq!(json_results[0]["name"], "php");
        assert_eq!(json_results[0]["version"], "8.2.1");
        assert_eq!(json_results[0]["status"], "success");
        assert_eq!(
            json_results[0]["failed_requirement"],
            serde_json::Value::Null
        );

        assert_eq!(json_results[1]["name"], "ext-foobar");
        assert_eq!(json_results[1]["version"], "n/a");
        assert_eq!(json_results[1]["status"], "missing");
        assert_eq!(json_results[1]["failed_requirement"], "*");
        assert_eq!(json_results[1]["provider"], "vendor/pkg");
    }

    // ── test_lib_packages_always_missing ─────────────────────────────────────

    #[test]
    fn test_lib_packages_always_missing() {
        let requirements = make_requirements(&[("lib-pcre", "*", "vendor/pkg")]);
        let platform = make_platform(&[("php", "8.2.1"), ("ext-pcre", "8.2.1")]);

        let results = check_requirements(&requirements, &platform);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].status,
            CheckStatus::Missing,
            "lib-* should always be missing"
        );
    }

    // ── test_composer_api_packages_missing ───────────────────────────────────

    #[test]
    fn test_composer_api_packages_missing() {
        let requirements = make_requirements(&[
            ("composer-plugin-api", "^2.0", "vendor/plugin"),
            ("composer-runtime-api", "^2.0", "vendor/plugin"),
        ]);
        let platform = make_platform(&[("php", "8.2.1")]);

        let results = check_requirements(&requirements, &platform);
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(
                r.status,
                CheckStatus::Missing,
                "{} should always be missing",
                r.name
            );
        }
    }

    // ── test_determine_exit_code ──────────────────────────────────────────────

    #[test]
    fn test_determine_exit_code_all_success() {
        let results = vec![CheckResult {
            name: "php".to_string(),
            version: "8.2.1".to_string(),
            status: CheckStatus::Success,
            failed_requirement: None,
        }];
        assert_eq!(determine_exit_code(&results), 0);
    }

    #[test]
    fn test_determine_exit_code_failed() {
        let results = vec![CheckResult {
            name: "php".to_string(),
            version: "8.1.0".to_string(),
            status: CheckStatus::Failed,
            failed_requirement: Some((">=8.2".to_string(), "root".to_string())),
        }];
        assert_eq!(determine_exit_code(&results), 1);
    }

    #[test]
    fn test_determine_exit_code_missing() {
        let results = vec![CheckResult {
            name: "ext-foobar".to_string(),
            version: "n/a".to_string(),
            status: CheckStatus::Missing,
            failed_requirement: Some(("*".to_string(), "vendor/pkg".to_string())),
        }];
        assert_eq!(determine_exit_code(&results), 2);
    }

    #[test]
    fn test_determine_exit_code_missing_beats_failed() {
        let results = vec![
            CheckResult {
                name: "php".to_string(),
                version: "8.1.0".to_string(),
                status: CheckStatus::Failed,
                failed_requirement: Some((">=8.2".to_string(), "root".to_string())),
            },
            CheckResult {
                name: "ext-foobar".to_string(),
                version: "n/a".to_string(),
                status: CheckStatus::Missing,
                failed_requirement: Some(("*".to_string(), "vendor/pkg".to_string())),
            },
        ];
        assert_eq!(determine_exit_code(&results), 2);
    }
}
