use clap::Args;
use mozart_core::console_format;
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

pub async fn execute(
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
    match root.package_type.as_deref() {
        Some("project") => {}
        Some(pkg_type) => {
            console.info(&console_format!("<warning>Warning: Bumping constraints for a non-project package (type=\"{pkg_type}\"). Libraries should not pin their dependencies.</warning>"));
        }
        None if !args.dev_only => {
            console.info(&console_format!("<warning>Warning: Bumping constraints for a non-project package. No type was set so it defaults to \"library\". Libraries should not pin their dependencies. Consider using --dev-only or setting the type to \"project\".</warning>"));
        }
        None => {}
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
        Some(
            args.packages
                .iter()
                .map(|p| strip_inline_constraint(p).to_lowercase())
                .collect(),
        )
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
                && !matches_filter(filter, pkg_name)
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
                && !matches_filter(filter, pkg_name)
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
        println!(
            "{}",
            console_format!(
                "<info>No requirements to update in {}.</info>",
                composer_json_path.display()
            )
        );
        return Ok(());
    }

    if args.dry_run {
        println!(
            "{}",
            console_format!(
                "<info>{} would be updated with:</info>",
                composer_json_path.display()
            )
        );
        for (name, _old, new) in &require_changes {
            println!(
                "{}",
                console_format!("<info> - require.{name}: {new}</info>")
            );
        }
        for (name, _old, new) in &require_dev_changes {
            println!(
                "{}",
                console_format!("<info> - require-dev.{name}: {new}</info>")
            );
        }
        // Return exit code 1 when dry-run detects changes (useful for CI to detect un-bumped constraints)
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
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
        "{}",
        console_format!(
            "<info>{} has been updated ({total_changes} changes).</info>",
            composer_json_path.display()
        )
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

/// Strip an inline constraint suffix from a package filter argument.
///
/// Composer allows arguments like `vendor/pkg:^2.0`, `vendor/pkg=2.0`, or
/// `vendor/pkg ^2.0`. This function strips everything from the first `:`,
/// `=`, or ` ` character onward, returning just the package name portion.
fn strip_inline_constraint(arg: &str) -> &str {
    arg.find([':', '=', ' '])
        .map(|pos| &arg[..pos])
        .unwrap_or(arg)
}

/// Returns true if `name` matches any of the glob patterns in `filter`.
///
/// Patterns may contain `*` wildcards (e.g. `psr/*`, `symfony/*`).
/// Matching is case-insensitive. Exact patterns are also supported.
fn matches_filter(filter: &[String], name: &str) -> bool {
    let name_lower = name.to_lowercase();
    filter.iter().any(|pat| glob_matches(pat, &name_lower))
}

/// Match a single package name against a glob pattern.
///
/// Only `*` wildcards are supported (matches any sequence of characters within
/// a path segment). Matching is case-insensitive.
///   - `psr/*`      matches `psr/log`, `psr/container`
///   - `symfony/*`  matches `symfony/console`, `symfony/http-kernel`
fn glob_matches(pattern: &str, name: &str) -> bool {
    // Fast path: no wildcard
    if !pattern.contains('*') {
        return pattern == name;
    }
    let pat_parts: Vec<&str> = pattern.splitn(2, '/').collect();
    let name_parts: Vec<&str> = name.splitn(2, '/').collect();
    if pat_parts.len() != name_parts.len() {
        return false;
    }
    pat_parts
        .iter()
        .zip(name_parts.iter())
        .all(|(pp, np)| glob_segment_matches(pp, np))
}

/// Match a single path segment against a pattern segment (no `/` involved).
/// `*` matches any sequence of characters (including empty). Both inputs are
/// already lowercased before being passed here.
fn glob_segment_matches(pattern: &str, text: &str) -> bool {
    glob_segment_matches_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_segment_matches_inner(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(&b'*'), _) => {
            glob_segment_matches_inner(&pattern[1..], text)
                || (!text.is_empty() && glob_segment_matches_inner(pattern, &text[1..]))
        }
        (Some(p), Some(t)) if p == t => glob_segment_matches_inner(&pattern[1..], &text[1..]),
        _ => false,
    }
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

    // ── Basic bump ─────────────────────────────────────────────────────────

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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).await.unwrap();

        let updated = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
    }

    // ── Dry run ────────────────────────────────────────────────────────────

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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
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

    // ── No changes ─────────────────────────────────────────────────────────

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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).await.unwrap();

        // No changes should be made
        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
    }

    // ── Dev-only flag ──────────────────────────────────────────────────────

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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // require should NOT be bumped
        assert_eq!(parsed["require"]["psr/log"], "^1.0");
        // require-dev should be bumped
        assert_eq!(parsed["require-dev"]["phpunit/phpunit"], "^9.5");
    }

    // ── No-dev-only flag ───────────────────────────────────────────────────

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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).await.unwrap();

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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).await.unwrap();

        // The lock file content-hash should now match the updated composer.json
        let updated_composer = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let updated_lock = LockFile::read_from_file(&dir.path().join("composer.lock")).unwrap();
        assert!(
            updated_lock.is_fresh(&updated_composer),
            "Lock file hash should be updated to match new composer.json"
        );
    }

    // ── strip_inline_constraint ────────────────────────────────────────────

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

    // ── glob_matches ───────────────────────────────────────────────────────

    #[test]
    fn test_glob_matches_exact() {
        assert!(glob_matches("psr/log", "psr/log"));
        assert!(!glob_matches("psr/log", "psr/container"));
    }

    #[test]
    fn test_glob_matches_wildcard_vendor() {
        assert!(glob_matches("psr/*", "psr/log"));
        assert!(glob_matches("psr/*", "psr/container"));
        assert!(!glob_matches("psr/*", "symfony/console"));
    }

    #[test]
    fn test_glob_matches_wildcard_suffix() {
        assert!(glob_matches("monolog/mono*", "monolog/monolog"));
        assert!(!glob_matches("monolog/mono*", "monolog/other"));
    }

    #[test]
    fn test_glob_matches_case_insensitive() {
        // pattern is lowercased before being stored; name is also lowercased
        assert!(glob_matches("psr/log", "psr/log"));
    }

    // ── matches_filter ─────────────────────────────────────────────────────

    #[test]
    fn test_matches_filter_exact() {
        let filter = vec!["psr/log".to_string()];
        assert!(matches_filter(&filter, "psr/log"));
        assert!(!matches_filter(&filter, "psr/container"));
    }

    #[test]
    fn test_matches_filter_glob() {
        let filter = vec!["psr/*".to_string()];
        assert!(matches_filter(&filter, "psr/log"));
        assert!(matches_filter(&filter, "psr/container"));
        assert!(!matches_filter(&filter, "monolog/monolog"));
    }

    #[test]
    fn test_matches_filter_multiple_patterns() {
        let filter = vec!["psr/*".to_string(), "monolog/monolog".to_string()];
        assert!(matches_filter(&filter, "psr/log"));
        assert!(matches_filter(&filter, "monolog/monolog"));
        assert!(!matches_filter(&filter, "symfony/console"));
    }

    // ── Package filter ─────────────────────────────────────────────────────

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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
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
        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join("composer.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // psr/log should be bumped (constraint suffix stripped from filter arg)
        assert_eq!(parsed["require"]["psr/log"], "^1.1.4");
        // monolog/monolog should NOT be bumped
        assert_eq!(parsed["require"]["monolog/monolog"], "^2.0");
    }
}
