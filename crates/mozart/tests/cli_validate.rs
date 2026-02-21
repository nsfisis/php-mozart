mod common;

#[test]
fn test_validate_valid_project() {
    let project = common::copy_fixture_to_temp("minimal");
    common::mozart_cmd()
        .arg("validate")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success();
}

#[test]
fn test_validate_valid_with_lock() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("validate")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success();
}

#[test]
fn test_validate_invalid_json() {
    let project = common::copy_fixture_to_temp("invalid_json");
    common::mozart_cmd()
        .arg("validate")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .code(2);
}

#[test]
fn test_validate_missing_composer_json() {
    let project = tempfile::TempDir::new().unwrap();
    common::mozart_cmd()
        .arg("validate")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .failure();
}

#[test]
fn test_validate_strict_with_warnings() {
    // A composer.json without a license generates a warning.
    // With --strict, warnings become a non-zero exit code.
    let project =
        common::setup_temp_project(r#"{"name": "test/no-license", "require": {"php": ">=8.1"}}"#);
    common::mozart_cmd()
        .arg("validate")
        .arg("--strict")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .code(1);
}
