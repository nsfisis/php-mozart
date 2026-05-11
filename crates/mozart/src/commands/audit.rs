use crate::composer::Composer;
use clap::Args;
use indexmap::IndexMap;
use mozart_core::advisory::{AbandonedHandling, AuditConfig, AuditFormat};
use mozart_core::console::IoInterface;
use mozart_core::repository::advisory::{AuditOptions, Auditor, PackageInfo};
use mozart_core::repository::cache::{Cache, build_cache_config};
use mozart_core::repository::repository::RepositorySet;
use std::path::Path;

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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

    // Load Composer state (reads composer.json + config)
    let composer = Composer::require(io.clone(), &working_dir)?;

    // Parse audit config from composer.json's config.audit section
    let audit_config = AuditConfig::from_config(composer.config(), true, AuditFormat::Table)?;

    // Resolve format: CLI arg > config default (table)
    let format = match args.format.as_deref() {
        Some(f) => match f.parse::<AuditFormat>() {
            Ok(fmt) => fmt,
            Err(_) => anyhow::bail!(
                "Invalid format \"{f}\". Supported formats: table, plain, json, summary"
            ),
        },
        None => audit_config.audit_format,
    };

    // Resolve --abandoned: CLI > config
    let abandoned = match args.abandoned.as_deref() {
        Some(s) => match s.parse::<AbandonedHandling>() {
            Ok(h) => h,
            Err(_) => anyhow::bail!(
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
        io.lock().unwrap().info("No packages - skipping audit.");
        return Ok(());
    }

    // Build repository set
    let repo_cache = Cache::repo(&build_cache_config(cli.no_cache));
    let repo_set = RepositorySet::with_packagist(repo_cache);

    // Run audit
    let exit_code = Auditor::new()
        .audit(
            io.clone(),
            &repo_set,
            &packages,
            &AuditOptions {
                format,
                warning_only: false,
                ignore_list: &audit_config.ignore_list_for_audit,
                abandoned,
                ignored_severities: &ignore_severities,
                ignore_unreachable,
                ignore_abandoned: &audit_config.ignore_abandoned_for_audit,
            },
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
    let installed = mozart_core::repository::installed::InstalledPackages::read(&vendor_dir)?;

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

    let lock = mozart_core::repository::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut all_packages: Vec<&mozart_core::repository::lockfile::LockedPackage> =
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
