//! Pool-builder fixture suite, ported from
//! `composer/tests/Composer/Test/DependencyResolver/PoolBuilderTest.php`.
//!
//! Composer drives this suite through a `@dataProvider`; each `.test` file
//! becomes one parameterized case. Mirrored here as one `#[test]` per
//! fixture so the count surfaces in `cargo test` output and individual
//! cases can be re-enabled as the runner is fleshed out.
//!
//! Every test is currently `#[ignore]` because the runner is a stub: the
//! orchestration that takes a `RepositorySet` + `Request` and produces a
//! populated `Pool` lives inline in `mozart_registry::resolver::resolve`,
//! not as an extracted entry point. Wiring those up — alias handling,
//! stability flags, fixed/locked packages, the optimizer pass — is the
//! follow-up work this scaffolding exists to track.

use std::path::{Path, PathBuf};

use mozart_test_harness::{ParsedPoolBuilderTest, parse_pool_builder_test_file};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composer/tests/Composer/Test/DependencyResolver/Fixtures/poolbuilder")
}

fn run_poolbuilder_fixture(ident: &str) {
    let filename = format!("{}.test", ident.replace('_', "-"));
    let path = fixtures_dir().join(&filename);
    let _parsed: ParsedPoolBuilderTest = parse_pool_builder_test_file(&path)
        .unwrap_or_else(|e| panic!("failed to parse {}: {:#}", path.display(), e));

    // Runner is intentionally not implemented yet — see module docs.
    // Removing `#[ignore]` from a case will surface this `unimplemented!`
    // and force the missing pool-builder entry point into existence.
    unimplemented!(
        "PoolBuilderTest runner not yet wired up; cannot execute {}",
        path.display()
    );
}

macro_rules! poolbuilder_fixture {
    ($name:ident) => {
        #[test]
        #[ignore]
        fn $name() {
            run_poolbuilder_fixture(stringify!($name));
        }
    };
}

poolbuilder_fixture!(alias_priority_conflicting);
poolbuilder_fixture!(alias_with_reference);
poolbuilder_fixture!(constraint_expansion_works_with_exact_versions);
poolbuilder_fixture!(filter_impossible_packages);
poolbuilder_fixture!(filter_impossible_packages_locked_replacer);
poolbuilder_fixture!(filter_impossible_packages_only_required);
poolbuilder_fixture!(filter_impossible_packages_only_required_provides);
poolbuilder_fixture!(filter_impossible_packages_only_required_replaces);
poolbuilder_fixture!(filter_impossible_packages_provides);
poolbuilder_fixture!(filter_impossible_packages_replaces);
poolbuilder_fixture!(fixed_packages_do_not_load_from_repos);
poolbuilder_fixture!(fixed_packages_replaced_do_not_load_from_repos);
poolbuilder_fixture!(load_replaced_package_if_replacer_dropped);
poolbuilder_fixture!(load_replaced_root_package_if_replacer_dropped);
poolbuilder_fixture!(multi_repo_replace);
poolbuilder_fixture!(multi_repo_replace_partial_update_all);
poolbuilder_fixture!(must_expand_root_reqs);
poolbuilder_fixture!(package_versions_are_not_loaded_if_not_required_expansion);
poolbuilder_fixture!(package_versions_are_not_loaded_if_not_required_recursive);
poolbuilder_fixture!(packages_that_do_not_exist);
poolbuilder_fixture!(partial_update);
poolbuilder_fixture!(partial_update_transitive_deps_no_root_unfix);
poolbuilder_fixture!(partial_update_transitive_deps_unfix);
poolbuilder_fixture!(partial_update_unfixes_path_repo_replacer_with_transitive_deps);
poolbuilder_fixture!(partial_update_unfixes_path_repos_always_but_not_their_transitive_deps);
poolbuilder_fixture!(partial_update_unfixing_locked_deps);
poolbuilder_fixture!(partial_update_unfixing_replacers);
poolbuilder_fixture!(partial_update_unfixing_with_replacers);
poolbuilder_fixture!(partial_update_unfixing_with_replacers_providers);
poolbuilder_fixture!(root_requirements_avoid_loading_further_versions);
poolbuilder_fixture!(stability_flags_take_over_minimum_stability_and_filter_packages);
