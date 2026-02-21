mod common;

use predicates::str::contains;

#[test]
fn test_licenses_locked() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("licenses")
        .arg("--locked")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stdout(contains("MIT"));
}

#[test]
fn test_licenses_format_json() {
    let project = common::copy_fixture_to_temp("with_lock");
    let output = common::mozart_cmd()
        .arg("licenses")
        .arg("--format=json")
        .arg("--locked")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);
    serde_json::from_str::<serde_json::Value>(&stdout)
        .expect("licenses --format=json output should be valid JSON");
}

#[test]
fn test_licenses_format_summary() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("licenses")
        .arg("--format=summary")
        .arg("--locked")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stdout(contains("MIT"));
}
