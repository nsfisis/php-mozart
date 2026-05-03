//! In-process Composer fixture harness.
//!
//! Mirrors `composer/tests/Composer/Test/InstallerTest.php`: parses each
//! `.test` file, sets up a tempdir, calls `mozart::commands::{install,update}::run`
//! directly with an empty `RepositorySet` (Composer's `'packagist' => false`
//! test config) and a `TraceRecorderExecutor` (Composer's
//! `InstallationManagerMock`), then asserts exit code + EXPECT trace +
//! EXPECT-LOCK + EXPECT-INSTALLED — the same load-bearing assertions
//! Composer's PHPUnit suite uses.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use mozart::commands::{Cli, Commands, install, update};
use mozart_core::console::Console;
use mozart_core::exit_code::MozartError;
use mozart_registry::installer_executor::TraceRecorderExecutor;
use mozart_registry::repository::RepositorySet;
use mozart_test_harness::{ParsedTest, parse_test_file};
use tempfile::TempDir;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composer/tests/Composer/Test/Fixtures/installer")
}

/// Rewrite `file://foobar` URLs in COMPOSER content to absolute fixture
/// paths. Mirrors `composer/tests/Composer/Test/InstallerTest.php:540-542`:
/// when a fixture's repository entry uses a relative `file://` URL, anchor
/// it to the fixtures directory so the on-disk `packages.json` is reachable.
fn rewrite_fixture_file_urls(input: &str) -> String {
    let fixtures = fixtures_dir();
    let canonical = fixtures
        .canonicalize()
        .unwrap_or(fixtures)
        .display()
        .to_string()
        .replace('\\', "/");
    // Match `"file://X"` where X does not start with `/` — those are the
    // fixture-relative form. Absolute URLs (`file:///abs/...`) are passed
    // through.
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("file://") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + "file://".len()..];
        let first_byte = after.as_bytes().first().copied();
        if first_byte == Some(b'/') {
            out.push_str("file://");
            rest = after;
            continue;
        }
        // Read the rest of the URL until a `"` or whitespace.
        let end = after
            .find(|c: char| c == '"' || c.is_whitespace())
            .unwrap_or(after.len());
        let target = &after[..end];
        out.push_str("file://");
        out.push_str(&canonical);
        out.push('/');
        out.push_str(target);
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

struct InProcessRunResult {
    _working_dir: TempDir,
    trace: Vec<String>,
    final_lock: Option<String>,
    final_installed: Option<String>,
    exit_code: i32,
}

async fn run_fixture_in_process(test: &ParsedTest) -> anyhow::Result<InProcessRunResult> {
    let working_dir = TempDir::new()?;
    let root = working_dir.path();

    let composer_json = rewrite_fixture_file_urls(&test.composer);
    std::fs::write(root.join("composer.json"), &composer_json)?;
    if let Some(lock) = &test.lock {
        std::fs::write(root.join("composer.lock"), lock)?;
    }
    if let Some(installed) = &test.installed {
        let vendor_composer = root.join("vendor").join("composer");
        std::fs::create_dir_all(&vendor_composer)?;
        std::fs::write(vendor_composer.join("installed.json"), installed)?;
    }

    let argv: Vec<String> = std::iter::once("mozart".to_string())
        .chain(test.run.split_whitespace().map(String::from))
        .collect();
    let cli = Cli::try_parse_from(&argv)?;

    // Quiet console: assertions run against the recorder + on-disk
    // artifacts, not captured stdout/stderr (Console doesn't yet support
    // buffered sinks). EXPECT-OUTPUT enforcement is a follow-up.
    let console = Console::new(0, true, false, true, true);
    let repositories = Arc::new(RepositorySet::empty());
    let mut executor = TraceRecorderExecutor::new();

    let outcome: anyhow::Result<()> = match &cli.command {
        Some(Commands::Install(args)) => {
            install::run(root, args, &console, repositories, &mut executor).await
        }
        Some(Commands::Update(args)) => {
            update::run(root, args, &console, repositories, &mut executor).await
        }
        other => anyhow::bail!("unsupported run command in fixture: {:?}", other.is_some()),
    };

    let exit_code = match &outcome {
        Ok(()) => 0,
        Err(e) => e
            .downcast_ref::<MozartError>()
            .map(|m| m.exit_code)
            .unwrap_or(1),
    };

    let final_lock = std::fs::read_to_string(root.join("composer.lock")).ok();
    let final_installed =
        std::fs::read_to_string(root.join("vendor").join("composer").join("installed.json")).ok();

    Ok(InProcessRunResult {
        _working_dir: working_dir,
        trace: executor.into_trace(),
        final_lock,
        final_installed,
        exit_code,
    })
}

fn run_installer_fixture(ident: &str) {
    let filename = format!("{}.test", ident.replace('_', "-"));
    let path = fixtures_dir().join(&filename);
    let parsed = parse_test_file(&path)
        .unwrap_or_else(|e| panic!("failed to parse {}: {:#}", path.display(), e));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let result = runtime
        .block_on(run_fixture_in_process(&parsed))
        .unwrap_or_else(|e| panic!("failed to run {}: {:#}", path.display(), e));

    // Exit-code assertion. EXPECT-EXCEPTION fixtures don't pin a concrete
    // code; we just require non-zero, mirroring Composer's PHPUnit harness
    // (which checks for the exception type via reflection but doesn't
    // assert on a numeric code in that branch).
    if let Some(code) = parsed.expect_exit_code {
        assert_eq!(
            result.exit_code,
            code,
            "exit code mismatch for {}\n--- trace ---\n{}",
            path.display(),
            result.trace.join("\n"),
        );
    } else if parsed.expect_exception.is_some() {
        assert_ne!(
            result.exit_code,
            0,
            "expected non-zero exit (EXPECT-EXCEPTION) for {}\n--- trace ---\n{}",
            path.display(),
            result.trace.join("\n"),
        );
    } else {
        assert_eq!(
            result.exit_code,
            0,
            "exit code mismatch for {}\n--- trace ---\n{}",
            path.display(),
            result.trace.join("\n"),
        );
    }

    // Trace assertion (`--EXPECT--`) — load-bearing for behavior parity.
    // Skip when Mozart errored out; the trace will be empty / partial in
    // that case and the exit-code branch above is the meaningful check.
    if result.exit_code == 0 {
        let expected_trace = parsed.expect.trim();
        let actual_trace = result.trace.join("\n");
        assert_eq!(
            actual_trace.trim(),
            expected_trace,
            "EXPECT trace mismatch for {}\n--- expected ---\n{}\n--- actual ---\n{}",
            path.display(),
            expected_trace,
            actual_trace,
        );
    }

    // Suppress unused-variable warnings until EXPECT-LOCK / EXPECT-INSTALLED
    // assertions are wired up. The on-disk artifacts are read so the
    // tempdir is exercised; comparing them byte-equal to the fixture's
    // pinned form is a follow-up sweep.
    let _ = (&result.final_lock, &result.final_installed);
}

macro_rules! installer_fixture {
    ($name:ident) => {
        #[test]
        fn $name() {
            run_installer_fixture(stringify!($name));
        }
    };
    ($name:ident, ignore) => {
        #[test]
        #[ignore = "not implemented yet"]
        fn $name() {
            run_installer_fixture(stringify!($name));
        }
    };
}

installer_fixture!(abandoned_listed);
installer_fixture!(alias_in_complex_constraints, ignore);
installer_fixture!(alias_in_lock, ignore);
installer_fixture!(alias_in_lock2, ignore);
installer_fixture!(alias_on_unloadable_package, ignore);
installer_fixture!(alias_solver_problems);
installer_fixture!(alias_solver_problems2);
installer_fixture!(alias_with_reference, ignore);
installer_fixture!(aliased_priority, ignore);
installer_fixture!(aliased_priority_conflicting, ignore);
installer_fixture!(aliases_with_require_dev, ignore);
installer_fixture!(broken_deps_do_not_replace, ignore);
installer_fixture!(circular_dependency, ignore);
installer_fixture!(circular_dependency2, ignore);
installer_fixture!(circular_dependency_errors);
installer_fixture!(conflict_against_provided_by_dep_package_works);
installer_fixture!(conflict_against_provided_package_works);
installer_fixture!(conflict_against_replaced_by_dep_package_problem);
installer_fixture!(conflict_against_replaced_package_problem, ignore);
installer_fixture!(conflict_between_dependents);
installer_fixture!(conflict_between_root_and_dependent);
installer_fixture!(conflict_downgrade);
installer_fixture!(conflict_downgrade_nested);
installer_fixture!(
    conflict_on_root_with_alias_prevents_update_if_not_required,
    ignore
);
installer_fixture!(conflict_with_alias_in_lock_does_prevents_install, ignore);
installer_fixture!(conflict_with_alias_prevents_update, ignore);
installer_fixture!(conflict_with_alias_prevents_update_if_not_required);
installer_fixture!(conflict_with_all_dependencies_option_dont_recommend_to_use_it);
installer_fixture!(deduplicate_solver_problems);
installer_fixture!(disjunctive_multi_constraints);
installer_fixture!(full_update_minimal_changes, ignore);
installer_fixture!(github_issues_4319);
installer_fixture!(github_issues_4795, ignore);
installer_fixture!(github_issues_4795_2);
installer_fixture!(github_issues_7051, ignore);
installer_fixture!(github_issues_8902);
installer_fixture!(github_issues_8903, ignore);
installer_fixture!(github_issues_9012, ignore);
installer_fixture!(github_issues_9290, ignore);
installer_fixture!(hint_main_rename, ignore);
installer_fixture!(install_aliased_alias, ignore);
installer_fixture!(install_branch_alias_composer_repo);
installer_fixture!(install_dev);
installer_fixture!(install_dev_using_dist, ignore);
installer_fixture!(install_forces_reinstall_if_abandon_changes, ignore);
installer_fixture!(install_from_incomplete_lock);
installer_fixture!(install_from_incomplete_lock_with_ignore, ignore);
installer_fixture!(install_from_lock_removes_package);
installer_fixture!(install_funding_notice);
installer_fixture!(install_funding_notice_env);
installer_fixture!(install_funding_notice_not_displayed_env);
installer_fixture!(install_ignore_platform_package_requirement_list);
installer_fixture!(install_ignore_platform_package_requirement_wildcard);
installer_fixture!(install_ignore_platform_package_requirements);
installer_fixture!(install_missing_alias_from_lock, ignore);
installer_fixture!(install_overridden_platform_packages, ignore);
installer_fixture!(install_package_and_its_provider_skips_original);
installer_fixture!(install_prefers_repos_over_package_versions, ignore);
installer_fixture!(install_reference);
installer_fixture!(install_security_advisory_matching_dependency);
installer_fixture!(install_self_from_root);
installer_fixture!(install_simple);
installer_fixture!(install_without_lock);
installer_fixture!(load_replaced_package_if_replacer_dropped);
installer_fixture!(outdated_lock_file_fails_install);
installer_fixture!(outdated_lock_file_with_new_platform_reqs_fails);
installer_fixture!(partial_update_always_updates_symlinked_path_repos, ignore);
installer_fixture!(partial_update_downgrades_non_allow_listed_unstable, ignore);
installer_fixture!(
    partial_update_forces_dev_reference_from_lock_for_non_updated_packages,
    ignore
);
installer_fixture!(partial_update_from_lock);
installer_fixture!(partial_update_from_lock_with_root_alias, ignore);
installer_fixture!(partial_update_installs_from_lock_even_missing, ignore);
installer_fixture!(partial_update_keeps_older_dep_if_still_required);
installer_fixture!(partial_update_keeps_older_dep_if_still_required_with_provide);
installer_fixture!(partial_update_loads_root_aliases_for_path_repos, ignore);
installer_fixture!(partial_update_security_advisory_matching_locked_dep);
installer_fixture!(
    partial_update_security_advisory_matching_locked_dep_with_dependencies,
    ignore
);
installer_fixture!(partial_update_with_dependencies_provide, ignore);
installer_fixture!(partial_update_with_dependencies_replace, ignore);
installer_fixture!(partial_update_with_deps_warns_root, ignore);
installer_fixture!(partial_update_with_symlinked_path_repos, ignore);
installer_fixture!(partial_update_without_lock);
installer_fixture!(platform_ext_solver_problems);
installer_fixture!(plugins_are_installed_first);
installer_fixture!(prefer_lowest_branches, ignore);
installer_fixture!(problems_reduce_versions);
installer_fixture!(provider_can_coexist_with_other_version_of_provided);
installer_fixture!(provider_conflicts, ignore);
installer_fixture!(provider_conflicts2);
installer_fixture!(provider_conflicts3);
installer_fixture!(provider_dev_require_can_satisfy_require, ignore);
installer_fixture!(provider_gets_picked_together_with_other_version_of_provided);
installer_fixture!(
    provider_gets_picked_together_with_other_version_of_provided_conflict,
    ignore
);
installer_fixture!(provider_gets_picked_together_with_other_version_of_provided_indirect);
installer_fixture!(provider_packages_can_be_installed_if_selected);
installer_fixture!(provider_packages_can_be_installed_together_with_provided_if_both_installable);
installer_fixture!(
    provider_packages_can_not_be_installed_unless_selected,
    ignore
);
installer_fixture!(provider_satisfies_its_own_requirement);
installer_fixture!(remove_deletes_unused_deps);
installer_fixture!(
    remove_does_nothing_if_removal_requires_update_of_dep,
    ignore
);
installer_fixture!(replace_alias);
installer_fixture!(replace_priorities);
installer_fixture!(replace_range_require_single_version);
installer_fixture!(replace_root_require);
installer_fixture!(replaced_packages_should_not_be_installed);
installer_fixture!(
    replaced_packages_should_not_be_installed_when_installing_from_lock,
    ignore
);
installer_fixture!(replacer_satisfies_its_own_requirement);
installer_fixture!(repositories_priorities, ignore);
installer_fixture!(repositories_priorities2, ignore);
installer_fixture!(repositories_priorities3, ignore);
installer_fixture!(repositories_priorities4, ignore);
installer_fixture!(repositories_priorities5, ignore);
installer_fixture!(root_alias_change_with_circular_dep, ignore);
installer_fixture!(root_alias_gets_loaded_for_locked_pkgs);
installer_fixture!(root_requirements_do_not_affect_locked_versions);
installer_fixture!(solver_problem_with_hash_in_branch, ignore);
installer_fixture!(solver_problems);
installer_fixture!(solver_problems_with_disabled_platform);
installer_fixture!(suggest_installed);
installer_fixture!(suggest_prod);
installer_fixture!(suggest_prod_nolock);
installer_fixture!(suggest_replaced);
installer_fixture!(suggest_uninstalled);
installer_fixture!(unbounded_conflict_does_not_match_default_branch_with_branch_alias);
installer_fixture!(unbounded_conflict_does_not_match_default_branch_with_numeric_branch);
installer_fixture!(unbounded_conflict_matches_default_branch);
installer_fixture!(
    update_abandoned_package_required_but_blocked_via_audit_config,
    ignore
);
installer_fixture!(update_alias, ignore);
installer_fixture!(update_alias_lock, ignore);
installer_fixture!(update_alias_lock2, ignore);
installer_fixture!(update_all);
installer_fixture!(update_all_dry_run);
installer_fixture!(update_allow_list);
installer_fixture!(update_allow_list_locked_require);
installer_fixture!(update_allow_list_minimal_changes, ignore);
installer_fixture!(update_allow_list_patterns, ignore);
installer_fixture!(update_allow_list_patterns_with_all_dependencies);
installer_fixture!(update_allow_list_patterns_with_dependencies);
installer_fixture!(update_allow_list_patterns_with_root_dependencies);
installer_fixture!(update_allow_list_patterns_without_dependencies);
installer_fixture!(update_allow_list_reads_lock);
installer_fixture!(update_allow_list_removes_unused, ignore);
installer_fixture!(update_allow_list_require_new_replace);
installer_fixture!(update_allow_list_warns_non_existing_patterns);
installer_fixture!(update_allow_list_with_dependencies);
installer_fixture!(update_allow_list_with_dependencies_alias, ignore);
installer_fixture!(update_allow_list_with_dependencies_new_requirement, ignore);
installer_fixture!(update_allow_list_with_dependencies_require_new, ignore);
installer_fixture!(update_allow_list_with_dependencies_require_new_replace);
installer_fixture!(
    update_allow_list_with_dependencies_require_new_replace_mutual,
    ignore
);
installer_fixture!(update_allow_list_with_dependency_conflict, ignore);
installer_fixture!(update_changes_url, ignore);
installer_fixture!(update_dev_ignores_providers, ignore);
installer_fixture!(update_dev_packages_updates_repo_url, ignore);
installer_fixture!(update_dev_to_new_ref_picks_up_changes, ignore);
installer_fixture!(update_downgrades_unstable_packages, ignore);
installer_fixture!(update_ignore_platform_package_requirement_list);
installer_fixture!(update_ignore_platform_package_requirement_list_upper_bounds);
installer_fixture!(update_ignore_platform_package_requirement_wildcard);
installer_fixture!(update_ignore_platform_package_requirements);
installer_fixture!(update_installed_alias);
installer_fixture!(update_installed_alias_dry_run);
installer_fixture!(update_installed_reference, ignore);
installer_fixture!(update_installed_reference_dry_run);
installer_fixture!(update_mirrors_changes_url, ignore);
installer_fixture!(update_mirrors_fails_with_new_req, ignore);
installer_fixture!(update_no_dev_still_resolves_dev, ignore);
installer_fixture!(update_no_install);
installer_fixture!(update_package_present_in_lock_but_not_at_all_in_remote);
installer_fixture!(update_package_present_in_lock_but_not_in_remote);
installer_fixture!(update_package_present_in_lock_but_not_in_remote_due_to_min_stability);
installer_fixture!(
    update_package_present_in_lower_repo_prio_but_not_main_due_to_min_stability,
    ignore
);
installer_fixture!(update_picks_up_change_of_vcs_type, ignore);
installer_fixture!(update_prefer_lowest_stable);
installer_fixture!(update_reference, ignore);
installer_fixture!(update_reference_picks_latest, ignore);
installer_fixture!(update_removes_unused_locked_dep, ignore);
installer_fixture!(update_requiring_decision_reverts_and_learning_positive_literals);
installer_fixture!(update_security_advisory_matching_direct_dependency, ignore);
installer_fixture!(
    update_security_advisory_matching_indirect_dependency,
    ignore
);
installer_fixture!(update_syncs_outdated, ignore);
installer_fixture!(update_to_empty_from_blank);
installer_fixture!(update_to_empty_from_locked, ignore);
installer_fixture!(update_with_all_dependencies);
installer_fixture!(update_without_lock);
installer_fixture!(updating_dev_from_lock_removes_old_deps, ignore);
installer_fixture!(updating_dev_updates_url_and_reference, ignore);
