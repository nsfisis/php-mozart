use std::path::Path;

use clap::Args;
use indexmap::IndexMap;
use mozart_core::advisory::{AbandonedHandling, AuditConfig, AuditFormat};
use mozart_core::composer::Composer;
use mozart_registry::advisory::{Auditor, PackageInfo};
use mozart_registry::cache::{Cache, build_cache_config};
use mozart_registry::repository::RepositorySet;

#[derive(Args)]
pub struct AuditArgs {
    /// Disables auditing of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Output format (table, plain, json, summary)
    #[arg(short, long)]
    pub format: Option<String>,

    /// Audit packages from the lock file instead of installed
    #[arg(long)]
    pub locked: bool,

    /// Behavior on abandoned packages (ignore, report, fail)
    #[arg(long)]
    pub abandoned: Option<String>,

    /// Ignore advisories of a given severity (low, medium, high, critical)
    #[arg(long)]
    pub ignore_severity: Vec<String>,

    /// Ignore advisories from unreachable repositories
    #[arg(long)]
    pub ignore_unreachable: bool,
}

pub async fn execute(
    args: &AuditArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

    // Load Composer state (reads composer.json + config)
    let composer = Composer::require(&working_dir)?;

    // Parse audit config from composer.json's config.audit section
    let audit_config = AuditConfig::from_config(composer.config(), true, AuditFormat::Table);

    // Resolve format: CLI arg > config default (table)
    let format = match args.format.as_deref() {
        Some(f) => match AuditFormat::from_str(f) {
            Some(fmt) => fmt,
            None => anyhow::bail!(
                "Invalid format \"{f}\". Supported formats: table, plain, json, summary"
            ),
        },
        None => audit_config.audit_format,
    };

    // Resolve --abandoned: CLI > config
    let abandoned = match args.abandoned.as_deref() {
        Some(s) => match AbandonedHandling::from_str(s) {
            Some(h) => h,
            None => anyhow::bail!(
                "Invalid abandoned value \"{s}\". Supported values: ignore, report, fail"
            ),
        },
        None => audit_config.audit_abandoned,
    };

    // Merge CLI --ignore-severity with config's ignore_severity_for_audit
    let mut ignore_severities: IndexMap<String, Option<String>> =
        audit_config.ignore_severity_for_audit.clone();
    for sev in &args.ignore_severity {
        ignore_severities.entry(sev.clone()).or_insert(None);
    }

    // OR CLI --ignore-unreachable with config
    let ignore_unreachable = args.ignore_unreachable || audit_config.ignore_unreachable;

    // Load packages
    let packages = get_packages(&composer, args)?;

    if packages.is_empty() {
        console.info("No packages - skipping audit.");
        return Ok(());
    }

    // Build repository set
    let repo_cache = Cache::repo(&build_cache_config(cli.no_cache));
    let repo_set = RepositorySet::with_packagist(repo_cache);

    // Run audit
    let exit_code = Auditor::new()
        .audit(
            console,
            &repo_set,
            &packages,
            format,
            false,
            &audit_config.ignore_list_for_audit,
            abandoned,
            &ignore_severities,
            ignore_unreachable,
            &audit_config.ignore_abandoned_for_audit,
        )
        .await?;

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code as i32));
    }

    Ok(())
}

fn get_packages(composer: &Composer, args: &AuditArgs) -> anyhow::Result<Vec<PackageInfo>> {
    if args.locked {
        load_locked_packages(composer.project_dir(), args.no_dev)
    } else {
        load_installed_packages(composer.project_dir(), args.no_dev)
    }
}

fn load_installed_packages(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<PackageInfo>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    let dev_names: indexmap::IndexSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    let packages = installed
        .packages
        .iter()
        .filter(|p| {
            if no_dev && dev_names.contains(&p.name.to_lowercase()) {
                return false;
            }
            true
        })
        .map(|p| {
            let abandoned_raw = p.extra_fields.get("abandoned").cloned();
            PackageInfo {
                name: p.name.clone(),
                version: p.version.clone(),
                version_normalized: p.version_normalized.clone(),
                abandoned_raw,
            }
        })
        .collect();

    Ok(packages)
}

fn load_locked_packages(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<PackageInfo>> {
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "A valid composer.json and composer.lock file is required to run this command with --locked"
        );
    }

    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut all_packages: Vec<&mozart_registry::lockfile::LockedPackage> =
        lock.packages.iter().collect();

    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        all_packages.extend(pkgs_dev.iter());
    }

    let packages = all_packages
        .iter()
        .map(|p| {
            let abandoned_raw = p.extra_fields.get("abandoned").cloned();
            PackageInfo {
                name: p.name.clone(),
                version: p.version.clone(),
                version_normalized: p.version_normalized.clone(),
                abandoned_raw,
            }
        })
        .collect();

    Ok(packages)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use mozart_registry::lockfile::{LockFile, LockedPackage};

    fn make_pkg(name: &str, version: &str, version_normalized: Option<&str>) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: version_normalized.map(|s| s.to_string()),
            abandoned_raw: None,
        }
    }

    fn make_pkg_abandoned(name: &str, version: &str, replacement: Option<&str>) -> PackageInfo {
        let abandoned_raw = match replacement {
            Some(r) => Some(serde_json::Value::String(r.to_string())),
            None => Some(serde_json::Value::Bool(true)),
        };
        PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: None,
            abandoned_raw,
        }
    }

    #[test]
    fn test_load_installed_packages() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        let mut installed = mozart_registry::installed::InstalledPackages::new();
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "1.5.0".to_string(),
            version_normalized: Some("1.5.0.0".to_string()),
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: BTreeMap::new(),
        });
        installed.write(&vendor_dir).unwrap();

        let packages = load_installed_packages(working_dir, false).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "monolog/monolog");
        assert_eq!(packages[0].version, "1.5.0");
    }

    #[test]
    fn test_load_installed_packages_no_dev() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        let mut installed = mozart_registry::installed::InstalledPackages::new();
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "1.5.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: BTreeMap::new(),
        });
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
            name: "phpunit/phpunit".to_string(),
            version: "10.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: BTreeMap::new(),
        });
        installed
            .dev_package_names
            .push("phpunit/phpunit".to_string());
        installed.write(&vendor_dir).unwrap();

        let packages = load_installed_packages(working_dir, true).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "monolog/monolog");
    }

    #[test]
    fn test_load_locked_packages() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages: vec![LockedPackage {
                name: "psr/log".to_string(),
                version: "3.0.0".to_string(),
                version_normalized: Some("3.0.0.0".to_string()),
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                provide: BTreeMap::new(),
                replace: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            }],
            packages_dev: None,
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        };

        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        let packages = load_locked_packages(working_dir, false).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "psr/log");
        assert_eq!(packages[0].version, "3.0.0");
    }

    #[test]
    fn test_load_locked_packages_no_dev() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages: vec![LockedPackage {
                name: "psr/log".to_string(),
                version: "3.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                provide: BTreeMap::new(),
                replace: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            }],
            packages_dev: Some(vec![LockedPackage {
                name: "phpunit/phpunit".to_string(),
                version: "10.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                provide: BTreeMap::new(),
                replace: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            }]),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        };

        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        let packages = load_locked_packages(working_dir, true).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "psr/log");

        let packages_all = load_locked_packages(working_dir, false).unwrap();
        assert_eq!(packages_all.len(), 2);
    }

    #[test]
    fn test_load_locked_packages_missing_lockfile() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let result = load_locked_packages(dir.path(), false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("composer.lock"));
    }

    #[test]
    fn test_package_info_abandoned() {
        let pkg = make_pkg_abandoned("old/pkg", "1.0.0", None);
        assert!(pkg.is_abandoned());
        assert!(pkg.replacement_package().is_none());

        let pkg_with_repl = make_pkg_abandoned("old/pkg", "1.0.0", Some("new/pkg"));
        assert!(pkg_with_repl.is_abandoned());
        assert_eq!(pkg_with_repl.replacement_package(), Some("new/pkg"));

        let active_pkg = make_pkg("active/pkg", "1.0.0", None);
        assert!(!active_pkg.is_abandoned());
    }

    #[test]
    fn test_invalid_format() {
        let format = "xml";
        assert!(AuditFormat::from_str(format).is_none());
    }

    #[test]
    fn test_valid_formats() {
        for fmt in &["table", "plain", "json", "summary"] {
            assert!(
                AuditFormat::from_str(fmt).is_some(),
                "format {fmt} should be valid"
            );
        }
    }

    #[test]
    fn test_invalid_abandoned_value() {
        assert!(AbandonedHandling::from_str("maybe").is_none());
    }

    #[test]
    fn test_valid_abandoned_values() {
        for value in &["ignore", "report", "fail"] {
            assert!(
                AbandonedHandling::from_str(value).is_some(),
                "abandoned value {value} should be valid"
            );
        }
    }
}
