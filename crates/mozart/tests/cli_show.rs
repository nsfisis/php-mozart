mod common;

use predicates::str::contains;

#[test]
fn test_show_locked_packages() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("show")
        .arg("--locked")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stdout(contains("psr/log"));
}

#[test]
fn test_show_self() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("show")
        .arg("--self-info")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stdout(contains("test/locked-project"));
}

#[test]
fn test_show_name_only() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("show")
        .arg("--name-only")
        .arg("--locked")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stdout(contains("psr/log"));
}

#[test]
fn test_show_format_json() {
    let project = common::copy_fixture_to_temp("with_lock");
    let output = common::mozart_cmd()
        .arg("show")
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
    // Output should be parseable JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("show --format=json output should be valid JSON");
    assert!(parsed.is_array() || parsed.is_object());
}
