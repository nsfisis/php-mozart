use crate::composer::Composer;
use clap::Args;
use indexmap::IndexMap;
use mozart_core::composer::LocalRepository;
use mozart_core::console::IoInterface;
use mozart_core::package::PackageInterface as _;
use mozart_core::{console_writeln, console_writeln_error};
use std::collections::BTreeMap;
use std::path::Path;

/// Exit code for stale lock file (matches Composer's BumpCommand::ERROR_LOCK_OUTDATED).
const ERROR_LOCK_OUTDATED: i32 = 2;

#[derive(Args)]
pub struct BumpArgs {
    /// Package(s) to bump
    pub packages: Vec<String>,

    /// Only bump packages in require-dev
    #[arg(short = 'D', long)]
    pub dev_only: bool,

    /// Only bump packages in require
    #[arg(short = 'R', long)]
    pub no_dev_only: bool,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn execute(
    args: &BumpArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let composer = Composer::require(io.clone(), &working_dir)?;

    let exit = do_bump(
        io,
        &composer,
        args.dev_only,
        args.no_dev_only,
        args.dry_run,
        &args.packages,
        "--dev-only",
    )
    .await?;

    if exit != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit));
    }
    Ok(())
}

/// Mirrors `Composer\Command\BumpCommand::doBump`. Returns the exit code
/// (0 / `ERROR_GENERIC` / `ERROR_LOCK_OUTDATED`).
///
/// `dev_only_flag_hint` is the option name shown in the `Alternatively you can use {hint}`
/// warning when the package has no `type` set. `bump` itself passes `--dev-only`;
/// `update --bump` will pass its own combined option name once that command is ported.
pub async fn do_bump(
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    composer: &Composer,
    dev_only: bool,
    no_dev_only: bool,
    dry_run: bool,
    packages_filter: &[String],
    dev_only_flag_hint: &str,
) -> anyhow::Result<i32> {
    let composer_json_path = composer.project_dir().join("composer.json");

    if !is_readable(&composer_json_path) {
        console_writeln_error!(
            io,
            "<error>{} is not readable.</error>",
            composer_json_path.display(),
        );
        return Ok(mozart_core::exit_code::GENERAL_ERROR);
    }

    let contents = match std::fs::read_to_string(&composer_json_path) {
        Ok(c) => c,
        Err(_) => {
            console_writeln_error!(
                io,
                "<error>{} is not readable.</error>",
                composer_json_path.display(),
            );
            return Ok(mozart_core::exit_code::GENERAL_ERROR);
        }
    };

    if !is_writable(&composer_json_path) {
        console_writeln_error!(
            io,
            "<error>{} is not writable.</error>",
            composer_json_path.display(),
        );
        return Ok(mozart_core::exit_code::GENERAL_ERROR);
    }

    // Mirrors Composer's `$hasLockfileDisabled = !$config->has('lock') || $config->get('lock')`.
    // The PHP variable is named "hasLockfileDisabled" but its value is *true* when the
    // lock is enabled (default) — i.e. the name is upstream-confusing. Mozart's
    // `Config::lock` is a `bool` (defaults to `true`), so the equivalent is just the field.
    let lock_enabled = composer.config().lock;
    let lock_path = composer.locker().lock_file_path();

    let locked_versions: IndexMap<String, (String, Option<String>)> = if !lock_enabled {
        // Composer always reaches for the locker here, even though `lock` is disabled.
        // Mirror that: if a lockfile exists on disk we use it; otherwise we fall back
        // to an empty map (`getLockedRepository` would throw in PHP — Mozart degrades
        // gracefully because `bump` has nothing to bump in that case anyway).
        if composer.locker().is_locked() {
            let lock = mozart_core::repository::lockfile::LockFile::read_from_file(lock_path)?;
            build_locked_versions_from_lock(&lock)
        } else {
            IndexMap::new()
        }
    } else if composer.locker().is_locked() {
        let lock = mozart_core::repository::lockfile::LockFile::read_from_file(lock_path)?;
        if !lock.is_fresh(&contents) {
            console_writeln_error!(
                io,
                "<error>The lock file is not up to date with the latest changes in composer.json. Run the appropriate `update` to fix that before you use the `bump` command.</error>",
            );
            return Ok(ERROR_LOCK_OUTDATED);
        }
        build_locked_versions_from_lock(&lock)
    } else {
        build_locked_versions_from_local(composer.repository_manager().local_repository())
    };

    let package_type = composer.package().package_type();
    if package_type != "project" && !dev_only {
        console_writeln_error!(
            io,
            "<warning>Warning: Bumping dependency constraints is not recommended for libraries as it will narrow down your dependencies and may cause problems for your users.</warning>",
        );
        if package_type == "library" {
            console_writeln_error!(
                io,
                "<warning>If your package is not a library, you can explicitly specify the \"type\" by using \"composer config type project\".</warning>",
            );
            console_writeln_error!(
                io,
                "<warning>Alternatively you can use {dev_only_flag_hint} to only bump dependencies within \"require-dev\".</warning>",
            );
        }
    }

    let mut tasks = Vec::new();
    if !dev_only {
        tasks.push(("require", composer.package().requires()));
    }
    if !no_dev_only {
        tasks.push(("require-dev", composer.package().dev_requires()));
    }

    let stripped_filter: Option<Vec<String>> = if packages_filter.is_empty() {
        None
    } else {
        let mut filtered: Vec<String> = packages_filter
            .iter()
            .map(|p| strip_inline_constraint(p).to_lowercase())
            .collect();
        filtered.sort();
        filtered.dedup();
        Some(filtered)
    };

    let mut updates: BTreeMap<&'static str, BTreeMap<String, String>> = BTreeMap::new();

    for (key, reqs) in &tasks {
        for (pkg_name, link) in reqs.iter() {
            let constraint = &link.constraint;
            if mozart_core::platform::is_platform_package(pkg_name) {
                continue;
            }
            if let Some(ref filter) = stripped_filter
                && !filter
                    .iter()
                    .any(|pat| mozart_core::matches_wildcard(pkg_name, pat))
            {
                continue;
            }
            let Some((pretty_version, version_normalized)) =
                locked_versions.get(&pkg_name.to_lowercase())
            else {
                continue;
            };
            let Some(new_constraint) = mozart_core::version_bumper::bump_requirement(
                constraint,
                pretty_version,
                version_normalized.as_deref(),
            ) else {
                continue;
            };
            if &new_constraint == constraint {
                continue;
            }
            updates
                .entry(*key)
                .or_default()
                .insert(pkg_name.clone(), new_constraint);
        }
    }

    if !dry_run && !update_file_cleanly(&composer_json_path, &updates)? {
        let mut composer_definition: mozart_core::package::RawPackageData =
            serde_json::from_str(&std::fs::read_to_string(&composer_json_path)?)?;
        for (key, packages) in &updates {
            for (package, version) in packages {
                match *key {
                    "require" => {
                        composer_definition
                            .require
                            .insert(package.clone(), version.clone());
                    }
                    "require-dev" => {
                        composer_definition
                            .require_dev
                            .insert(package.clone(), version.clone());
                    }
                    _ => unreachable!(),
                }
            }
        }
        mozart_core::package::write_to_file(&composer_definition, &composer_json_path)?;
    }

    let change_count: usize = updates.values().map(|m| m.len()).sum();
    if change_count > 0 {
        if dry_run {
            console_writeln!(
                io,
                "<info>{} would be updated with:</info>",
                composer_json_path.display(),
            );
            for (require_type, packages) in &updates {
                for (package, version) in packages {
                    console_writeln!(io, "<info> - {require_type}.{package}: {version}</info>");
                }
            }
        } else {
            console_writeln!(
                io,
                "<info>{} has been updated ({change_count} changes).</info>",
                composer_json_path.display(),
            );
        }
    } else {
        console_writeln!(
            io,
            "<info>No requirements to update in {}.</info>",
            composer_json_path.display(),
        );
    }

    if !dry_run && composer.locker().is_locked() && composer.config().lock && change_count > 0 {
        update_lock_hash(lock_path, &composer_json_path)?;
    }

    if dry_run && change_count > 0 {
        return Ok(mozart_core::exit_code::GENERAL_ERROR);
    }

    Ok(0)
}

/// Mirrors `BumpCommand::updateFileCleanly`. Returns `Ok(true)` on a clean,
/// formatting-preserving write; `Ok(false)` when the caller must fall back
/// to a full structured rewrite of `composer.json`.
///
/// Mozart does not yet have a `JsonManipulator` port, so this always returns
/// `Ok(false)` and the caller falls back. See `docs/known-incompatibilities.md`.
fn update_file_cleanly(
    _path: &Path,
    _updates: &BTreeMap<&'static str, BTreeMap<String, String>>,
) -> anyhow::Result<bool> {
    Ok(false)
}

/// Recompute the lock file's `content-hash` to match `composer_json_path`.
/// Mirrors `Locker::updateHash`, which `BumpCommand::doBump` calls after a
/// successful in-place edit so the lockfile stays "fresh" for the next install.
fn update_lock_hash(lock_path: &Path, composer_json_path: &Path) -> anyhow::Result<()> {
    let new_composer_json_content = std::fs::read_to_string(composer_json_path)?;
    let new_hash = mozart_core::repository::lockfile::LockFile::compute_content_hash(
        &new_composer_json_content,
    )?;
    let mut lock = mozart_core::repository::lockfile::LockFile::read_from_file(lock_path)?;
    lock.content_hash = new_hash;
    lock.write_to_file(lock_path)?;
    Ok(())
}

fn is_readable(path: &Path) -> bool {
    std::fs::File::open(path).is_ok()
}

fn is_writable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => !m.permissions().readonly(),
        Err(_) => false,
    }
}

/// Build a map of lowercase package names to (pretty_version, version_normalized)
/// from a parsed `composer.lock`.
fn build_locked_versions_from_lock(
    lock: &mozart_core::repository::lockfile::LockFile,
) -> IndexMap<String, (String, Option<String>)> {
    let mut map: IndexMap<String, (String, Option<String>)> = IndexMap::new();
    let all_packages = lock
        .packages
        .iter()
        .chain(lock.packages_dev.as_deref().unwrap_or(&[]));
    for pkg in all_packages {
        map.insert(
            pkg.name.to_lowercase(),
            (pkg.version.clone(), pkg.version_normalized.clone()),
        );
    }
    map
}

/// Build a map of lowercase package names to (pretty_version, None) from
/// the local repository (`vendor/composer/installed.json`). Used as the
/// fallback when no `composer.lock` is present, mirroring Composer's
/// `getRepositoryManager()->getLocalRepository()` branch.
fn build_locked_versions_from_local(
    repo: &LocalRepository,
) -> IndexMap<String, (String, Option<String>)> {
    let mut map: IndexMap<String, (String, Option<String>)> = IndexMap::new();
    for pkg in repo.get_canonical_packages() {
        map.insert(
            pkg.pretty_name().to_lowercase(),
            (pkg.pretty_version().to_string(), None),
        );
    }
    map
}

/// Strip an inline constraint suffix from a package filter argument.
///
/// Composer allows arguments like `vendor/pkg:^2.0`, `vendor/pkg=2.0`, or
/// `vendor/pkg ^2.0`. This function strips everything from the first `:`,
/// `=`, or ` ` character onward, returning just the package name portion.
/// Mirrors `Preg::replace('{[:= ].+}', '', $constraint)`.
fn strip_inline_constraint(arg: &str) -> &str {
    arg.find([':', '=', ' '])
        .map(|pos| &arg[..pos])
        .unwrap_or(arg)
}
