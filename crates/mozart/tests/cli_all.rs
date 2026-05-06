mod common;

macro_rules! test_help {
    ($func_name:ident, $command_name:literal) => {
        #[test]
        fn $func_name() {
            $crate::common::mozart_cmd()
                .arg($command_name)
                .arg("--help")
                .assert()
                .success();
        }
    };
}

test_help!(test_about_help, "about");
test_help!(test_archive_help, "archive");
test_help!(test_audit_help, "audit");
test_help!(test_browse_help, "browse");
test_help!(test_bump_help, "bump");
test_help!(test_check_platform_reqs_help, "check-platform-reqs");
test_help!(test_clear_cache_help, "clear-cache");
test_help!(test_completion_help, "completion");
test_help!(test_config_help, "config");
test_help!(test_create_project_help, "create-project");
test_help!(test_depends_help, "depends");
test_help!(test_diagnose_help, "diagnose");
test_help!(test_dump_autoload_help, "dump-autoload");
test_help!(test_exec_help, "exec");
test_help!(test_fund_help, "fund");
test_help!(test_global_help, "global");
test_help!(test_init_help, "init");
test_help!(test_install_help, "install");
test_help!(test_licenses_help, "licenses");
test_help!(test_outdated_help, "outdated");
test_help!(test_prohibits_help, "prohibits");
test_help!(test_reinstall_help, "reinstall");
test_help!(test_remove_help, "remove");
test_help!(test_repository_help, "repository");
test_help!(test_require_help, "require");
test_help!(test_run_script_help, "run-script");
test_help!(test_search_help, "search");
test_help!(test_self_update_help, "self-update");
test_help!(test_show_help, "show");
test_help!(test_status_help, "status");
test_help!(test_suggests_help, "suggests");
test_help!(test_update_help, "update");
test_help!(test_validate_help, "validate");
