mod common;

use predicates::str::contains;

#[test]
fn test_config_list() {
    let project = common::copy_fixture_to_temp("minimal");
    common::mozart_cmd()
        .arg("config")
        .arg("--list")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stdout(contains("vendor-dir"));
}

#[test]
fn test_config_single_key() {
    let project = common::copy_fixture_to_temp("minimal");
    common::mozart_cmd()
        .arg("config")
        .arg("vendor-dir")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stdout(contains("vendor"));
}

#[test]
fn test_config_no_key_fails() {
    let project = common::copy_fixture_to_temp("minimal");
    common::mozart_cmd()
        .arg("config")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .failure();
}
