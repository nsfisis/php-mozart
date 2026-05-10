use clap::Args;
use indexmap::IndexSet;
use mozart_core::console::IoInterface;
use mozart_core::installer::{
    InstalledRepoLite, MODE_BY_PACKAGE, MODE_BY_SUGGESTION, MODE_LIST, RootInfo,
    SuggestedPackagesReporter,
};
use mozart_core::platform::is_platform_package;
use std::path::Path;

#[derive(Args)]
pub struct SuggestsArgs {
    /// Package(s) to list suggestions for
    pub packages: Vec<String>,

    /// Group output by package
    #[arg(long)]
    pub by_package: bool,

    /// Group output by suggestion
    #[arg(long)]
    pub by_suggestion: bool,

    /// Show suggestions for all packages, not just root
    #[arg(short, long)]
    pub all: bool,

    /// Show only suggested package names in list format
    #[arg(long)]
    pub list: bool,

    /// Disables suggestions from require-dev packages
    #[arg(long)]
    pub no_dev: bool,
}

pub async fn execute(
    args: &SuggestsArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let io_guard = io.lock().unwrap();
    let console = &**io_guard;
    let working_dir = cli.working_dir()?;

    let lock_path = working_dir.join("composer.lock");
    let has_lock = lock_path.exists();

    let composer_json_path = working_dir.join("composer.json");
    let root = if composer_json_path.exists() {
        Some(mozart_core::package::read_from_file(&composer_json_path)?)
    } else {
        None
    };

    // Build the "installed repo" (names of everything currently present:
    // packages, provides, replaces, platform).
    let installed_repo = build_installed_repo(&working_dir, has_lock, args.no_dev, root.as_ref())?;

    let mut reporter = SuggestedPackagesReporter::new(console);

    let filter: IndexSet<String> = args.packages.iter().cloned().collect();

    // Iterate every package that contributes suggestions: locked/installed,
    // then root. Mirrors `$installedRepo->getPackages() + $composer->getPackage()`.
    if has_lock {
        let lock = mozart_core::repository::lockfile::LockFile::read_from_file(&lock_path)?;
        for pkg in lock.packages.iter() {
            if filter.is_empty() || filter.contains(&pkg.name) {
                reporter.add_suggestions_from_package(pkg);
            }
        }
        if !args.no_dev
            && let Some(ref pkgs_dev) = lock.packages_dev
        {
            for pkg in pkgs_dev {
                if filter.is_empty() || filter.contains(&pkg.name) {
                    reporter.add_suggestions_from_package(pkg);
                }
            }
        }
    } else {
        let vendor_dir = working_dir.join("vendor");
        let installed = mozart_core::repository::installed::InstalledPackages::read(&vendor_dir)?;

        if installed.packages.is_empty() {
            let installed_json = vendor_dir.join("composer/installed.json");
            if !installed_json.exists() {
                anyhow::bail!(
                    "No composer.lock and no installed.json found. \
                     Run `mozart install` first."
                );
            }
        }

        let dev_names: IndexSet<String> = installed
            .dev_package_names
            .iter()
            .map(|n| n.to_lowercase())
            .collect();

        for pkg in &installed.packages {
            if args.no_dev && dev_names.contains(&pkg.name.to_lowercase()) {
                continue;
            }
            if filter.is_empty() || filter.contains(&pkg.name) {
                reporter.add_suggestions_from_package(pkg);
            }
        }
    }

    if let Some(ref root) = root
        && (filter.is_empty() || filter.contains(&root.name))
    {
        reporter.add_suggestions_from_package(root);
    }

    // Resolve the output mode bitfield, mirroring SuggestsCommand::execute:
    // start with by-package; --by-suggestion replaces it; --by-package then
    // re-adds by-package; --list overrides everything.
    let mut mode: u32 = MODE_BY_PACKAGE;
    if args.by_suggestion {
        mode = MODE_BY_SUGGESTION;
    }
    if args.by_package {
        mode |= MODE_BY_PACKAGE;
    }
    if args.list {
        mode = MODE_LIST;
    }

    let only_dependents_of = if filter.is_empty() && !args.all {
        Some(build_root_info(root.as_ref()))
    } else {
        None
    };

    reporter.output(mode, Some(&installed_repo), only_dependents_of.as_ref());

    Ok(())
}

fn build_installed_repo(
    working_dir: &Path,
    has_lock: bool,
    no_dev: bool,
    root: Option<&mozart_core::package::RawPackageData>,
) -> anyhow::Result<InstalledRepoLite> {
    let mut repo = InstalledRepoLite::new();

    if has_lock {
        let lock_path = working_dir.join("composer.lock");
        let lock = mozart_core::repository::lockfile::LockFile::read_from_file(&lock_path)?;

        let mut all_packages: Vec<&mozart_core::repository::lockfile::LockedPackage> =
            lock.packages.iter().collect();
        if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
            all_packages.extend(pkgs_dev.iter());
        }

        for pkg in all_packages {
            repo.insert(&pkg.name);
            for name in pkg.provide.keys().chain(pkg.replace.keys()) {
                repo.insert(name);
            }
        }

        if let Some(obj) = lock.platform.as_object() {
            for key in obj.keys() {
                if is_platform_package(key) {
                    repo.insert(key);
                }
            }
        }
        if let Some(obj) = lock.platform_dev.as_object() {
            for key in obj.keys() {
                if is_platform_package(key) {
                    repo.insert(key);
                }
            }
        }
    } else {
        let vendor_dir = working_dir.join("vendor");
        let installed = mozart_core::repository::installed::InstalledPackages::read(&vendor_dir)?;

        let dev_names: IndexSet<String> = installed
            .dev_package_names
            .iter()
            .map(|n| n.to_lowercase())
            .collect();

        for pkg in &installed.packages {
            if no_dev && dev_names.contains(&pkg.name.to_lowercase()) {
                continue;
            }
            repo.insert(&pkg.name);
            for key in &["provide", "replace"] {
                if let Some(val) = pkg.extra_fields.get(*key)
                    && let Some(obj) = val.as_object()
                {
                    for name in obj.keys() {
                        repo.insert(name);
                    }
                }
            }
        }

        if let Some(root) = root {
            for name in root.require.keys().chain(root.require_dev.keys()) {
                if is_platform_package(name) {
                    repo.insert(name);
                }
            }
        }
    }

    Ok(repo)
}

fn build_root_info(root: Option<&mozart_core::package::RawPackageData>) -> RootInfo {
    let Some(root) = root else {
        return RootInfo::default();
    };
    let mut direct_deps: IndexSet<String> = IndexSet::new();
    for name in root.require.keys().chain(root.require_dev.keys()) {
        direct_deps.insert(name.to_lowercase());
    }
    RootInfo {
        name: root.name.clone(),
        direct_deps,
    }
}
