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
    let expected = parsed.expect_exit_code.unwrap_or(0);
    assert_eq!(
        result.exit_code,
        expected,
        "exit code mismatch for {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        path.display(),
        result.stdout,
        result.stderr,
    );
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

installer_fixture!(
    abandoned_listed,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    alias_in_complex_constraints,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    alias_in_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    alias_in_lock2,
    ignore = "mozart binary cannot yet run this fixture"
);
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
installer_fixture!(
    aliased_priority,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    aliased_priority_conflicting,
    ignore = "mozart binary cannot yet run this fixture"
);
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
installer_fixture!(
    circular_dependency_errors,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_against_provided_by_dep_package_works,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_against_provided_package_works,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_against_replaced_by_dep_package_problem,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_against_replaced_package_problem,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_between_dependents,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_between_root_and_dependent,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_downgrade,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_downgrade_nested,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_on_root_with_alias_prevents_update_if_not_required,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_with_alias_in_lock_does_prevents_install,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_with_alias_prevents_update,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_with_alias_prevents_update_if_not_required,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    conflict_with_all_dependencies_option_dont_recommend_to_use_it,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    deduplicate_solver_problems,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    disjunctive_multi_constraints,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    full_update_minimal_changes,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    github_issues_4319,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    github_issues_4795,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    github_issues_4795_2,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    github_issues_7051,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    github_issues_8902,
    ignore = "mozart binary cannot yet run this fixture"
);
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
installer_fixture!(
    install_aliased_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_branch_alias_composer_repo,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_dev,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_dev_using_dist,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_forces_reinstall_if_abandon_changes,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_from_incomplete_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_from_incomplete_lock_with_ignore,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_from_lock_removes_package,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_funding_notice,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_funding_notice_env,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_funding_notice_not_displayed_env,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_ignore_platform_package_requirement_list,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_ignore_platform_package_requirement_wildcard,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_ignore_platform_package_requirements,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_missing_alias_from_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_overridden_platform_packages,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_package_and_its_provider_skips_original,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_prefers_repos_over_package_versions,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_reference,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_security_advisory_matching_dependency,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_self_from_root,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_simple,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    install_without_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    load_replaced_package_if_replacer_dropped,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(outdated_lock_file_fails_install);
installer_fixture!(
    outdated_lock_file_with_new_platform_reqs_fails,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_always_updates_symlinked_path_repos,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_downgrades_non_allow_listed_unstable,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_forces_dev_reference_from_lock_for_non_updated_packages,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_from_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_from_lock_with_root_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_installs_from_lock_even_missing,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_keeps_older_dep_if_still_required,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_keeps_older_dep_if_still_required_with_provide,
    ignore = "mozart binary cannot yet run this fixture"
);
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
installer_fixture!(
    partial_update_with_dependencies_provide,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_with_dependencies_replace,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_with_deps_warns_root,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_with_symlinked_path_repos,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    partial_update_without_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    platform_ext_solver_problems,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    plugins_are_installed_first,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    prefer_lowest_branches,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    problems_reduce_versions,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_can_coexist_with_other_version_of_provided,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_conflicts,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_conflicts2,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_conflicts3,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_dev_require_can_satisfy_require,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_gets_picked_together_with_other_version_of_provided,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_gets_picked_together_with_other_version_of_provided_conflict,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_gets_picked_together_with_other_version_of_provided_indirect,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_packages_can_be_installed_if_selected,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_packages_can_be_installed_together_with_provided_if_both_installable,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_packages_can_not_be_installed_unless_selected,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    provider_satisfies_its_own_requirement,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    remove_deletes_unused_deps,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    remove_does_nothing_if_removal_requires_update_of_dep,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    replace_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    replace_priorities,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    replace_range_require_single_version,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    replace_root_require,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    replaced_packages_should_not_be_installed,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    replaced_packages_should_not_be_installed_when_installing_from_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    replacer_satisfies_its_own_requirement,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    repositories_priorities,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    repositories_priorities2,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    repositories_priorities3,
    ignore = "mozart binary cannot yet run this fixture"
);
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
installer_fixture!(
    root_alias_gets_loaded_for_locked_pkgs,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    root_requirements_do_not_affect_locked_versions,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    solver_problem_with_hash_in_branch,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    solver_problems,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    solver_problems_with_disabled_platform,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    suggest_installed,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    suggest_prod,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    suggest_prod_nolock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    suggest_replaced,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    suggest_uninstalled,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    unbounded_conflict_does_not_match_default_branch_with_branch_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    unbounded_conflict_does_not_match_default_branch_with_numeric_branch,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    unbounded_conflict_matches_default_branch,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_abandoned_package_required_but_blocked_via_audit_config,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_alias_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_alias_lock2,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_all,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_all_dry_run,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_locked_require,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_minimal_changes,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_patterns,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_patterns_with_all_dependencies,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_patterns_with_dependencies,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_patterns_with_root_dependencies,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_patterns_without_dependencies,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_reads_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_removes_unused,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_require_new_replace,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_warns_non_existing_patterns,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_with_dependencies,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_with_dependencies_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_with_dependencies_new_requirement,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_with_dependencies_require_new,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_with_dependencies_require_new_replace,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_with_dependencies_require_new_replace_mutual,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_allow_list_with_dependency_conflict,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_changes_url,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_dev_ignores_providers,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_dev_packages_updates_repo_url,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_dev_to_new_ref_picks_up_changes,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_downgrades_unstable_packages,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_ignore_platform_package_requirement_list,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_ignore_platform_package_requirement_list_upper_bounds,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_ignore_platform_package_requirement_wildcard,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_ignore_platform_package_requirements,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_installed_alias,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_installed_alias_dry_run,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_installed_reference,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_installed_reference_dry_run,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_mirrors_changes_url,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_mirrors_fails_with_new_req,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_no_dev_still_resolves_dev,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_no_install,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_package_present_in_lock_but_not_at_all_in_remote,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_package_present_in_lock_but_not_in_remote,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_package_present_in_lock_but_not_in_remote_due_to_min_stability,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_package_present_in_lower_repo_prio_but_not_main_due_to_min_stability,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_picks_up_change_of_vcs_type,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_prefer_lowest_stable,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_reference,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_reference_picks_latest,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_removes_unused_locked_dep,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_requiring_decision_reverts_and_learning_positive_literals,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_security_advisory_matching_direct_dependency,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_security_advisory_matching_indirect_dependency,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_syncs_outdated,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(update_to_empty_from_blank);
installer_fixture!(update_to_empty_from_locked);
installer_fixture!(
    update_with_all_dependencies,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    update_without_lock,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    updating_dev_from_lock_removes_old_deps,
    ignore = "mozart binary cannot yet run this fixture"
);
installer_fixture!(
    updating_dev_updates_url_and_reference,
    ignore = "mozart binary cannot yet run this fixture"
);
