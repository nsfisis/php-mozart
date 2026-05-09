use clap::Args;
use indexmap::IndexMap;
use mozart_core::composer::{Composer, LocalRepository};
use mozart_core::console::Console;
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

pub async fn execute(args: &BumpArgs, cli: &super::Cli, console: &Console) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let composer = Composer::require(&working_dir)?;

    let exit = do_bump(
        console,
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
    io: &Console,
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
            let lock = mozart_registry::lockfile::LockFile::read_from_file(lock_path)?;
            build_locked_versions_from_lock(&lock)
        } else {
            IndexMap::new()
        }
    } else if composer.locker().is_locked() {
        let lock = mozart_registry::lockfile::LockFile::read_from_file(lock_path)?;
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

    let package_type = composer.package().package_type.as_deref();
    if package_type != Some("project") && !dev_only {
        console_writeln_error!(
            io,
            "<warning>Warning: Bumping dependency constraints is not recommended for libraries as it will narrow down your dependencies and may cause problems for your users.</warning>",
        );
        if package_type.is_none() {
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

    let mut tasks: Vec<(&'static str, &BTreeMap<String, String>)> = Vec::new();
    if !dev_only {
        tasks.push(("require", &composer.package().require));
    }
    if !no_dev_only {
        tasks.push(("require-dev", &composer.package().require_dev));
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
        for (pkg_name, constraint) in reqs.iter() {
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
    let new_hash =
        mozart_registry::lockfile::LockFile::compute_content_hash(&new_composer_json_content)?;
    let mut lock = mozart_registry::lockfile::LockFile::read_from_file(lock_path)?;
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
    lock: &mozart_registry::lockfile::LockFile,
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

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_registry::lockfile::{LockFile, LockedPackage};
    use tempfile::tempdir;

    fn minimal_lock(packages: Vec<LockedPackage>, packages_dev: Vec<LockedPackage>) -> LockFile {
        LockFile {
            readme: LockFile::default_readme(),
            content_hash: "placeholder".to_string(),
            packages,
            packages_dev: Some(packages_dev),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        }
    }

    fn make_locked_package(name: &str, version: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: Some(format!("{version}.0")),
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
        }
    }

    fn write_composer_json(dir: &std::path::Path, content: &str) {
        std::fs::write(dir.join("composer.json"), content).unwrap();
    }

    fn write_lock_with_hash(dir: &std::path::Path, mut lock: LockFile, composer_json: &str) {
        let hash = LockFile::compute_content_hash(composer_json).unwrap();
        lock.content_hash = hash;
        lock.write_to_file(&dir.join("composer.lock")).unwrap();
    }

    fn make_cli(working_dir: &std::path::Path) -> super::super::Cli {
        super::super::Cli {
            command: Some(super::super::Commands::Bump(BumpArgs {
                packages: vec![],
                dev_only: false,
                no_dev_only: false,
                dry_run: false,
            })),
            version: false,
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(working_dir.to_str().unwrap().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        }
    }

    fn quiet_console() -> Console {
        Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        }
    }

    #[tokio::test]
    async fn test_basic_bump_modifies_composer_json() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(vec![make_locked_package("psr/log", "1.1.4")], vec![]);
        write_lock_with_hash(dir.path(), lock, composer_json);

        let args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        let updated = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
    }

    #[tokio::test]
    async fn test_dry_run_does_not_modify_files() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(vec![make_locked_package("psr/log", "1.1.4")], vec![]);
        write_lock_with_hash(dir.path(), lock, composer_json);

        let args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: false,
            dry_run: true,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        let result = execute(&args, &cli, &console).await;

        // dry-run with changes returns exit code 1 (for CI usage)
        let err = result.unwrap_err();
        let mozart_err = err
            .downcast_ref::<mozart_core::exit_code::MozartError>()
            .expect("should be MozartError");
        assert_eq!(mozart_err.exit_code, mozart_core::exit_code::GENERAL_ERROR);

        // composer.json should be unchanged
        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.0");
    }

    #[tokio::test]
    async fn test_no_changes_when_already_bumped() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.1.4"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(vec![make_locked_package("psr/log", "1.1.4")], vec![]);
        write_lock_with_hash(dir.path(), lock, composer_json);

        let args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        // No changes should be made
        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
    }

    #[tokio::test]
    async fn test_dev_only_flag_only_bumps_require_dev() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0"
    },
    "require-dev": {
        "phpunit/phpunit": "^9.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(
            vec![make_locked_package("psr/log", "1.1.4")],
            vec![make_locked_package("phpunit/phpunit", "9.5.0")],
        );
        write_lock_with_hash(dir.path(), lock, composer_json);

        let args = BumpArgs {
            packages: vec![],
            dev_only: true,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // require should NOT be bumped
        assert_eq!(parsed["require"]["psr/log"], "^1.0");
        // require-dev should be bumped
        assert_eq!(parsed["require-dev"]["phpunit/phpunit"], "^9.5");
    }

    #[tokio::test]
    async fn test_no_dev_only_flag_only_bumps_require() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0"
    },
    "require-dev": {
        "phpunit/phpunit": "^9.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(
            vec![make_locked_package("psr/log", "1.1.4")],
            vec![make_locked_package("phpunit/phpunit", "9.5.0")],
        );
        write_lock_with_hash(dir.path(), lock, composer_json);

        let args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: true,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // require should be bumped
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
        // require-dev should NOT be bumped
        assert_eq!(parsed["require-dev"]["phpunit/phpunit"], "^9.0");
    }

    #[tokio::test]
    async fn test_stale_lock_file_produces_exit_code_2() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "require": {
        "psr/log": "^1.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        // Write lock with a wrong hash (stale)
        let mut lock = minimal_lock(vec![make_locked_package("psr/log", "1.1.4")], vec![]);
        lock.content_hash = "wrong_hash_here".to_string();
        lock.write_to_file(&dir.path().join("composer.lock"))
            .unwrap();

        let args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        let result = execute(&args, &cli, &console).await;

        // stale lock file should return exit code 2 (ERROR_LOCK_OUTDATED)
        let err = result.unwrap_err();
        let mozart_err = err
            .downcast_ref::<mozart_core::exit_code::MozartError>()
            .expect("should be MozartError");
        assert_eq!(mozart_err.exit_code, ERROR_LOCK_OUTDATED);
    }

    #[tokio::test]
    async fn test_lock_file_hash_updated_after_bump() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(vec![make_locked_package("psr/log", "1.1.4")], vec![]);
        write_lock_with_hash(dir.path(), lock, composer_json);

        let args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        // The lock file content-hash should now match the updated composer.json
        let updated_composer = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let updated_lock = LockFile::read_from_file(&dir.path().join("composer.lock")).unwrap();
        assert!(
            updated_lock.is_fresh(&updated_composer),
            "Lock file hash should be updated to match new composer.json"
        );
    }

    #[tokio::test]
    async fn test_no_lock_falls_back_to_local_repository() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        // No composer.lock — instead populate vendor/composer/installed.json.
        let installed_dir = dir.path().join("vendor/composer");
        std::fs::create_dir_all(&installed_dir).unwrap();
        let installed = serde_json::json!({
            "packages": [
                {
                    "name": "psr/log",
                    "version": "1.1.4",
                    "version_normalized": "1.1.4.0",
                }
            ],
            "dev": false,
        });
        std::fs::write(
            installed_dir.join("installed.json"),
            serde_json::to_string_pretty(&installed).unwrap(),
        )
        .unwrap();

        let args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
    }

    #[test]
    fn test_strip_inline_constraint_colon() {
        assert_eq!(strip_inline_constraint("vendor/pkg:^2.0"), "vendor/pkg");
    }

    #[test]
    fn test_strip_inline_constraint_equals() {
        assert_eq!(strip_inline_constraint("vendor/pkg=2.0.0"), "vendor/pkg");
    }

    #[test]
    fn test_strip_inline_constraint_space() {
        assert_eq!(strip_inline_constraint("vendor/pkg ^2.0"), "vendor/pkg");
    }

    #[test]
    fn test_strip_inline_constraint_no_suffix() {
        assert_eq!(strip_inline_constraint("vendor/pkg"), "vendor/pkg");
        assert_eq!(strip_inline_constraint("psr/log"), "psr/log");
    }

    #[tokio::test]
    async fn test_package_filter_only_bumps_specified_packages() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0",
        "psr/http-message": "^1.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(
            vec![
                make_locked_package("psr/log", "1.1.4"),
                make_locked_package("psr/http-message", "1.2.0"),
            ],
            vec![],
        );
        write_lock_with_hash(dir.path(), lock, composer_json);

        let args = BumpArgs {
            packages: vec!["psr/log".to_string()],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
        // psr/http-message should NOT be bumped
        assert_eq!(parsed["require"]["psr/http-message"], "^1.0");
    }

    #[tokio::test]
    async fn test_package_filter_glob_wildcard() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0",
        "psr/container": "^1.0",
        "monolog/monolog": "^2.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(
            vec![
                make_locked_package("psr/log", "1.1.4"),
                make_locked_package("psr/container", "1.1.1"),
                make_locked_package("monolog/monolog", "2.9.0"),
            ],
            vec![],
        );
        write_lock_with_hash(dir.path(), lock, composer_json);

        // Filter using a wildcard: only bump psr/* packages
        let args = BumpArgs {
            packages: vec!["psr/*".to_string()],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Both psr/* packages should be bumped
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
        assert_eq!(parsed["require"]["psr/container"], "^1.1.1");
        // monolog/monolog should NOT be bumped
        assert_eq!(parsed["require"]["monolog/monolog"], "^2.0");
    }

    #[tokio::test]
    async fn test_package_filter_with_inline_constraint() {
        let dir = tempdir().unwrap();
        let composer_json = r#"{
    "name": "test/project",
    "type": "project",
    "require": {
        "psr/log": "^1.0",
        "monolog/monolog": "^2.0"
    }
}"#;
        write_composer_json(dir.path(), composer_json);

        let lock = minimal_lock(
            vec![
                make_locked_package("psr/log", "1.1.4"),
                make_locked_package("monolog/monolog", "2.9.0"),
            ],
            vec![],
        );
        write_lock_with_hash(dir.path(), lock, composer_json);

        // Specify filter with an inline constraint suffix (Composer-style: "psr/log:^1.0")
        let args = BumpArgs {
            packages: vec!["psr/log:^1.0".to_string()],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let cli = make_cli(dir.path());
        let console = quiet_console();
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // psr/log should be bumped (constraint suffix stripped from filter arg)
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
        // monolog/monolog should NOT be bumped
        assert_eq!(parsed["require"]["monolog/monolog"], "^2.0");
    }
}
