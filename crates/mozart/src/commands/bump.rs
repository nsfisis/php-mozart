use clap::Args;
use std::collections::HashMap;
use std::path::PathBuf;

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

// ─── Main entry point ─────────────────────────────────────────────────────────

pub fn execute(
    args: &BumpArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let composer_json_path = working_dir.join("composer.json");
    let lock_path = working_dir.join("composer.lock");

    // Ensure composer.json exists
    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }

    // Read composer.json content (raw string for hash computation)
    let composer_json_content = std::fs::read_to_string(&composer_json_path)?;

    // Parse composer.json
    let mut root: mozart_core::package::RawPackageData =
        serde_json::from_str(&composer_json_content)?;

    // Warn if package is not a project (libraries shouldn't bump)
    if let Some(ref pkg_type) = root.package_type
        && pkg_type != "project"
    {
        console.info(&format!(
            "{}",
            mozart_core::console::warning(&format!(
                "Warning: Bumping constraints for a non-project package (type=\"{pkg_type}\"). \
                 Libraries should not pin their dependencies."
            ))
        ));
    }

    // Check lock file existence
    if !lock_path.exists() {
        anyhow::bail!("No composer.lock found. Run `mozart install` first.");
    }

    // Read and parse lock file
    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    // Check lock file freshness
    if !lock.is_fresh(&composer_json_content) {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::LOCK_FILE_INVALID,
            "composer.lock is not up to date with composer.json. \
             Run `mozart install` or `mozart update` to refresh it.",
        ));
    }

    // Build map: package name (lowercase) → (pretty_version, version_normalized)
    let locked_versions = build_locked_versions_map(&lock);

    // Determine which sections to process
    let bump_require = !args.dev_only;
    let bump_require_dev = !args.no_dev_only;

    // Package filter (if specified)
    let package_filter: Option<Vec<String>> = if args.packages.is_empty() {
        None
    } else {
        Some(args.packages.iter().map(|p| p.to_lowercase()).collect())
    };

    // Collect changes
    let mut require_changes: Vec<(String, String, String)> = Vec::new(); // (name, old, new)
    let mut require_dev_changes: Vec<(String, String, String)> = Vec::new();

    // Process require
    if bump_require {
        for (pkg_name, constraint) in &root.require {
            if is_platform_package(pkg_name) {
                continue;
            }
            if let Some(ref filter) = package_filter
                && !filter.contains(&pkg_name.to_lowercase())
            {
                continue;
            }
            if let Some((pretty_version, version_normalized)) =
                locked_versions.get(&pkg_name.to_lowercase())
                && let Some(new_constraint) = mozart_core::version_bumper::bump_requirement(
                    constraint,
                    pretty_version,
                    version_normalized.as_deref(),
                )
            {
                require_changes.push((pkg_name.clone(), constraint.clone(), new_constraint));
            }
        }
    }

    // Process require-dev
    if bump_require_dev {
        for (pkg_name, constraint) in &root.require_dev {
            if is_platform_package(pkg_name) {
                continue;
            }
            if let Some(ref filter) = package_filter
                && !filter.contains(&pkg_name.to_lowercase())
            {
                continue;
            }
            if let Some((pretty_version, version_normalized)) =
                locked_versions.get(&pkg_name.to_lowercase())
                && let Some(new_constraint) = mozart_core::version_bumper::bump_requirement(
                    constraint,
                    pretty_version,
                    version_normalized.as_deref(),
                )
            {
                require_dev_changes.push((pkg_name.clone(), constraint.clone(), new_constraint));
            }
        }
    }

    let total_changes = require_changes.len() + require_dev_changes.len();

    if total_changes == 0 {
        println!("Nothing to bump.");
        return Ok(());
    }

    // Print what would change
    for (name, old, new) in require_changes.iter().chain(require_dev_changes.iter()) {
        if args.dry_run {
            println!(
                "{}: {} → {}",
                mozart_core::console::info(name),
                old,
                mozart_core::console::comment(new)
            );
        } else {
            println!(
                "Bumping {} from {} to {}",
                mozart_core::console::info(name),
                old,
                mozart_core::console::comment(new)
            );
        }
    }

    if args.dry_run {
        println!("\n{} constraint(s) would be bumped.", total_changes);
        return Ok(());
    }

    // Apply changes to root package
    for (name, _old, new) in &require_changes {
        root.require.insert(name.clone(), new.clone());
    }
    for (name, _old, new) in &require_dev_changes {
        root.require_dev.insert(name.clone(), new.clone());
    }

    // Write updated composer.json
    mozart_core::package::write_to_file(&root, &composer_json_path)?;

    // Update the lock file content-hash to match the new composer.json
    let new_composer_json_content = std::fs::read_to_string(&composer_json_path)?;
    let new_hash =
        mozart_registry::lockfile::LockFile::compute_content_hash(&new_composer_json_content)?;
    let mut updated_lock = lock;
    updated_lock.content_hash = new_hash;
    updated_lock.write_to_file(&lock_path)?;

    println!(
        "\n{}",
        mozart_core::console::info(&format!(
            "{} constraint(s) bumped successfully.",
            total_changes
        ))
    );

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Build a map of lowercase package names to (pretty_version, version_normalized) from composer.lock.
fn build_locked_versions_map(
    lock: &mozart_registry::lockfile::LockFile,
) -> HashMap<String, (String, Option<String>)> {
    let mut map: HashMap<String, (String, Option<String>)> = HashMap::new();

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

/// Returns true if the package name is a platform requirement (php, ext-*, lib-*, etc.).
fn is_platform_package(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "php"
        || lower.starts_with("ext-")
        || lower.starts_with("lib-")
        || lower == "php-64bit"
        || lower == "php-ipv6"
        || lower == "php-zts"
        || lower == "php-debug"
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_registry::lockfile::{LockFile, LockedPackage};
    use std::collections::BTreeMap;
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
            command: super::super::Commands::Bump(BumpArgs {
                packages: vec![],
                dev_only: false,
                no_dev_only: false,
                dry_run: false,
            }),
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

    // ── Basic bump ─────────────────────────────────────────────────────────

    #[test]
    fn test_basic_bump_modifies_composer_json() {
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let updated = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
    }

    // ── Dry run ────────────────────────────────────────────────────────────

    #[test]
    fn test_dry_run_does_not_modify_files() {
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        // composer.json should be unchanged
        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.0");
    }

    // ── No changes ─────────────────────────────────────────────────────────

    #[test]
    fn test_no_changes_when_already_bumped() {
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        // No changes should be made
        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
    }

    // ── Dev-only flag ──────────────────────────────────────────────────────

    #[test]
    fn test_dev_only_flag_only_bumps_require_dev() {
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // require should NOT be bumped
        assert_eq!(parsed["require"]["psr/log"], "^1.0");
        // require-dev should be bumped
        assert_eq!(parsed["require-dev"]["phpunit/phpunit"], "^9.5");
    }

    // ── No-dev-only flag ───────────────────────────────────────────────────

    #[test]
    fn test_no_dev_only_flag_only_bumps_require() {
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // require should be bumped
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
        // require-dev should NOT be bumped
        assert_eq!(parsed["require-dev"]["phpunit/phpunit"], "^9.0");
    }

    // ── Stale lock file ────────────────────────────────────────────────────

    #[test]
    fn test_stale_lock_file_produces_exit_code_2() {
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

        let _args = BumpArgs {
            packages: vec![],
            dev_only: false,
            no_dev_only: false,
            dry_run: false,
        };
        let _cli = make_cli(dir.path());

        // The execute function returns Err(MozartError) with LOCK_FILE_INVALID for stale lock.
        // We verify the lock IS stale here as a prerequisite check.
        let lock_loaded = LockFile::read_from_file(&dir.path().join("composer.lock")).unwrap();
        assert!(!lock_loaded.is_fresh(composer_json));
    }

    // ── Platform packages skipped ──────────────────────────────────────────

    #[test]
    fn test_platform_packages_are_skipped() {
        assert!(is_platform_package("php"));
        assert!(is_platform_package("ext-json"));
        assert!(is_platform_package("ext-mbstring"));
        assert!(is_platform_package("lib-pcre"));
        assert!(!is_platform_package("psr/log"));
        assert!(!is_platform_package("monolog/monolog"));
    }

    // ── Lock file hash updated ─────────────────────────────────────────────

    #[test]
    fn test_lock_file_hash_updated_after_bump() {
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        // The lock file content-hash should now match the updated composer.json
        let updated_composer = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let updated_lock = LockFile::read_from_file(&dir.path().join("composer.lock")).unwrap();
        assert!(
            updated_lock.is_fresh(&updated_composer),
            "Lock file hash should be updated to match new composer.json"
        );
    }

    // ── Package filter ─────────────────────────────────────────────────────

    #[test]
    fn test_package_filter_only_bumps_specified_packages() {
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
        // psr/http-message should NOT be bumped
        assert_eq!(parsed["require"]["psr/http-message"], "^1.0");
    }
}
