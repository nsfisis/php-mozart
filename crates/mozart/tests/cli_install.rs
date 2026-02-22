mod common;

use predicates::str::contains;

#[test]
fn test_install_dry_run() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("install")
        .arg("--dry-run")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stderr(contains("Installing"));
}

#[test]
fn test_install_no_lock_file() {
    let project = common::copy_fixture_to_temp("minimal");
    common::mozart_cmd()
        .arg("install")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stderr(contains("No composer.lock file present"));
}

#[test]
fn test_install_no_composer_json() {
    let project = tempfile::TempDir::new().unwrap();
    common::mozart_cmd()
        .arg("install")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .failure();
}

#[test]
fn test_install_dry_run_shows_package_names() {
    let project = common::copy_fixture_to_temp("with_lock");
    common::mozart_cmd()
        .arg("install")
        .arg("--dry-run")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stderr(contains("psr/log"));
}
