use mozart_test_harness::{parse_test_file, run_test};
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composer/tests/Composer/Test/Fixtures/installer")
}

fn run_installer_fixture(ident: &str) {
    let filename = format!("{}.test", ident.replace('_', "-"));
    let path = fixtures_dir().join(&filename);
    let parsed = parse_test_file(&path)
        .unwrap_or_else(|e| panic!("failed to parse {}: {:#}", path.display(), e));
    let mozart_bin: &Path = assert_cmd::cargo::cargo_bin!("mozart");
    let result = run_test(&parsed, mozart_bin)
        .unwrap_or_else(|e| panic!("failed to run {}: {:#}", path.display(), e));

    // Composer's `.test` format uses EXPECT-EXCEPTION to assert that the run
    // throws an exception. PHP propagates uncaught exceptions as a non-zero
    // exit; we don't yet match the exception class, but we do require Mozart
    // to exit non-zero when the fixture expects an exception (and no explicit
    // EXPECT-EXIT-CODE has been pinned).
    if let Some(code) = parsed.expect_exit_code {
        assert_eq!(
            result.exit_code,
            code,
            "exit code mismatch for {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            path.display(),
            result.stdout,
            result.stderr,
        );
    } else if parsed.expect_exception.is_some() {
        assert_ne!(
            result.exit_code,
            0,
            "expected non-zero exit (EXPECT-EXCEPTION) for {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            path.display(),
            result.stdout,
            result.stderr,
        );
    } else {
        assert_eq!(
            result.exit_code,
            0,
            "exit code mismatch for {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            path.display(),
            result.stdout,
            result.stderr,
        );
    }
}

macro_rules! installer_fixture {
    ($name:ident) => {
        #[test]
        fn $name() {
            run_installer_fixture(stringify!($name));
        }
    };
    ($name:ident, ignore = $reason:literal) => {
        #[test]
        #[ignore = $reason]
        fn $name() {
            run_installer_fixture(stringify!($name));
        }
    };
}

installer_fixture!(abandoned_listed);
installer_fixture!(
    alias_in_complex_constraints,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    alias_in_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(alias_in_lock2);
installer_fixture!(
    alias_on_unloadable_package,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    alias_solver_problems,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    alias_solver_problems2,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    alias_with_reference,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(aliased_priority);
installer_fixture!(aliased_priority_conflicting);
installer_fixture!(
    aliases_with_require_dev,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    broken_deps_do_not_replace,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    circular_dependency,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    circular_dependency2,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(circular_dependency_errors);
installer_fixture!(conflict_against_provided_by_dep_package_works);
installer_fixture!(conflict_against_provided_package_works);
installer_fixture!(conflict_against_replaced_by_dep_package_problem);
installer_fixture!(
    conflict_against_replaced_package_problem,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(conflict_between_dependents);
installer_fixture!(conflict_between_root_and_dependent);
installer_fixture!(conflict_downgrade);
installer_fixture!(conflict_downgrade_nested);
installer_fixture!(
    conflict_on_root_with_alias_prevents_update_if_not_required,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_with_alias_in_lock_does_prevents_install,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(conflict_with_alias_prevents_update);
installer_fixture!(
    conflict_with_alias_prevents_update_if_not_required,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_with_all_dependencies_option_dont_recommend_to_use_it,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(deduplicate_solver_problems);
installer_fixture!(disjunctive_multi_constraints);
installer_fixture!(full_update_minimal_changes);
installer_fixture!(github_issues_4319);
installer_fixture!(github_issues_4795);
installer_fixture!(github_issues_4795_2);
installer_fixture!(
    github_issues_7051,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(github_issues_8902);
installer_fixture!(
    github_issues_8903,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    github_issues_9012,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    github_issues_9290,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    hint_main_rename,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(install_aliased_alias);
installer_fixture!(
    install_branch_alias_composer_repo,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(install_dev);
installer_fixture!(install_dev_using_dist);
installer_fixture!(install_forces_reinstall_if_abandon_changes);
installer_fixture!(install_from_incomplete_lock);
installer_fixture!(
    install_from_incomplete_lock_with_ignore,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(install_from_lock_removes_package);
installer_fixture!(install_funding_notice);
installer_fixture!(install_funding_notice_env);
installer_fixture!(install_funding_notice_not_displayed_env);
installer_fixture!(install_ignore_platform_package_requirement_list);
installer_fixture!(install_ignore_platform_package_requirement_wildcard);
installer_fixture!(install_ignore_platform_package_requirements);
installer_fixture!(
    install_missing_alias_from_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_overridden_platform_packages,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(install_package_and_its_provider_skips_original);
installer_fixture!(install_prefers_repos_over_package_versions);
installer_fixture!(install_reference);
installer_fixture!(
    install_security_advisory_matching_dependency,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(install_self_from_root);
installer_fixture!(install_simple);
installer_fixture!(install_without_lock);
installer_fixture!(load_replaced_package_if_replacer_dropped);
installer_fixture!(outdated_lock_file_fails_install);
installer_fixture!(outdated_lock_file_with_new_platform_reqs_fails);
installer_fixture!(
    partial_update_always_updates_symlinked_path_repos,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_downgrades_non_allow_listed_unstable,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(partial_update_forces_dev_reference_from_lock_for_non_updated_packages);
installer_fixture!(partial_update_from_lock);
installer_fixture!(partial_update_from_lock_with_root_alias);
installer_fixture!(partial_update_installs_from_lock_even_missing);
installer_fixture!(partial_update_keeps_older_dep_if_still_required);
installer_fixture!(partial_update_keeps_older_dep_if_still_required_with_provide);
installer_fixture!(
    partial_update_loads_root_aliases_for_path_repos,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_security_advisory_matching_locked_dep,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_security_advisory_matching_locked_dep_with_dependencies,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(partial_update_with_dependencies_provide);
installer_fixture!(partial_update_with_dependencies_replace);
installer_fixture!(
    partial_update_with_deps_warns_root,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(partial_update_with_symlinked_path_repos);
installer_fixture!(partial_update_without_lock);
installer_fixture!(platform_ext_solver_problems);
installer_fixture!(plugins_are_installed_first);
installer_fixture!(prefer_lowest_branches);
installer_fixture!(problems_reduce_versions);
installer_fixture!(provider_can_coexist_with_other_version_of_provided);
installer_fixture!(
    provider_conflicts,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(provider_conflicts2);
installer_fixture!(provider_conflicts3);
installer_fixture!(
    provider_dev_require_can_satisfy_require,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(provider_gets_picked_together_with_other_version_of_provided);
installer_fixture!(
    provider_gets_picked_together_with_other_version_of_provided_conflict,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(provider_gets_picked_together_with_other_version_of_provided_indirect);
installer_fixture!(provider_packages_can_be_installed_if_selected);
installer_fixture!(
    provider_packages_can_be_installed_together_with_provided_if_both_installable,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_packages_can_not_be_installed_unless_selected,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(provider_satisfies_its_own_requirement);
installer_fixture!(remove_deletes_unused_deps);
installer_fixture!(remove_does_nothing_if_removal_requires_update_of_dep);
installer_fixture!(
    replace_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(replace_priorities);
installer_fixture!(replace_range_require_single_version);
installer_fixture!(replace_root_require);
installer_fixture!(replaced_packages_should_not_be_installed);
installer_fixture!(
    replaced_packages_should_not_be_installed_when_installing_from_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(replacer_satisfies_its_own_requirement);
installer_fixture!(
    repositories_priorities,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(repositories_priorities2);
installer_fixture!(repositories_priorities3);
installer_fixture!(
    repositories_priorities4,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    repositories_priorities5,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    root_alias_change_with_circular_dep,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(root_alias_gets_loaded_for_locked_pkgs);
installer_fixture!(root_requirements_do_not_affect_locked_versions);
installer_fixture!(
    solver_problem_with_hash_in_branch,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(solver_problems);
installer_fixture!(solver_problems_with_disabled_platform);
installer_fixture!(suggest_installed);
installer_fixture!(suggest_prod);
installer_fixture!(suggest_prod_nolock);
installer_fixture!(suggest_replaced);
installer_fixture!(suggest_uninstalled);
installer_fixture!(
    unbounded_conflict_does_not_match_default_branch_with_branch_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(unbounded_conflict_does_not_match_default_branch_with_numeric_branch);
installer_fixture!(
    unbounded_conflict_matches_default_branch,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_abandoned_package_required_but_blocked_via_audit_config,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(update_alias);
installer_fixture!(update_alias_lock);
installer_fixture!(update_alias_lock2);
installer_fixture!(update_all);
installer_fixture!(update_all_dry_run);
installer_fixture!(update_allow_list);
installer_fixture!(update_allow_list_locked_require);
installer_fixture!(update_allow_list_minimal_changes);
installer_fixture!(update_allow_list_patterns);
installer_fixture!(update_allow_list_patterns_with_all_dependencies);
installer_fixture!(update_allow_list_patterns_with_dependencies);
installer_fixture!(update_allow_list_patterns_with_root_dependencies);
installer_fixture!(update_allow_list_patterns_without_dependencies);
installer_fixture!(update_allow_list_reads_lock);
installer_fixture!(update_allow_list_removes_unused);
installer_fixture!(update_allow_list_require_new_replace);
installer_fixture!(update_allow_list_warns_non_existing_patterns);
installer_fixture!(update_allow_list_with_dependencies);
installer_fixture!(
    update_allow_list_with_dependencies_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(update_allow_list_with_dependencies_new_requirement);
installer_fixture!(update_allow_list_with_dependencies_require_new);
installer_fixture!(update_allow_list_with_dependencies_require_new_replace);
installer_fixture!(update_allow_list_with_dependencies_require_new_replace_mutual);
installer_fixture!(update_allow_list_with_dependency_conflict);
installer_fixture!(update_changes_url);
installer_fixture!(update_dev_ignores_providers);
installer_fixture!(update_dev_packages_updates_repo_url);
installer_fixture!(update_dev_to_new_ref_picks_up_changes);
installer_fixture!(
    update_downgrades_unstable_packages,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(update_ignore_platform_package_requirement_list);
installer_fixture!(update_ignore_platform_package_requirement_list_upper_bounds);
installer_fixture!(update_ignore_platform_package_requirement_wildcard);
installer_fixture!(update_ignore_platform_package_requirements);
installer_fixture!(update_installed_alias);
installer_fixture!(update_installed_alias_dry_run);
installer_fixture!(update_installed_reference);
installer_fixture!(update_installed_reference_dry_run);
installer_fixture!(update_mirrors_changes_url);
installer_fixture!(update_mirrors_fails_with_new_req);
installer_fixture!(update_no_dev_still_resolves_dev);
installer_fixture!(update_no_install);
installer_fixture!(update_package_present_in_lock_but_not_at_all_in_remote);
installer_fixture!(update_package_present_in_lock_but_not_in_remote);
installer_fixture!(update_package_present_in_lock_but_not_in_remote_due_to_min_stability);
installer_fixture!(
    update_package_present_in_lower_repo_prio_but_not_main_due_to_min_stability,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(update_picks_up_change_of_vcs_type);
installer_fixture!(update_prefer_lowest_stable);
installer_fixture!(update_reference);
installer_fixture!(update_reference_picks_latest);
installer_fixture!(update_removes_unused_locked_dep);
installer_fixture!(update_requiring_decision_reverts_and_learning_positive_literals);
installer_fixture!(
    update_security_advisory_matching_direct_dependency,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_security_advisory_matching_indirect_dependency,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(update_syncs_outdated);
installer_fixture!(update_to_empty_from_blank);
installer_fixture!(update_to_empty_from_locked);
installer_fixture!(update_with_all_dependencies);
installer_fixture!(update_without_lock);
installer_fixture!(updating_dev_from_lock_removes_old_deps);
installer_fixture!(updating_dev_updates_url_and_reference);
