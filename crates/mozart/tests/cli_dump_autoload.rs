mod common;

use predicates::str::contains;

#[test]
fn test_dump_autoload_dry_run() {
    let project = common::copy_fixture_to_temp("minimal");
    common::mozart_cmd()
        .arg("dump-autoload")
        .arg("--dry-run")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success()
        .stderr(contains("Dry run"));
}

#[test]
fn test_dump_autoload_does_not_write_in_dry_run() {
    let project = common::copy_fixture_to_temp("minimal");
    let vendor_dir = project.path().join("vendor");
    common::mozart_cmd()
        .arg("dump-autoload")
        .arg("--dry-run")
        .arg("--working-dir")
        .arg(project.path())
        .assert()
        .success();
    // In dry-run mode, no vendor directory should have been created
    assert!(
        !vendor_dir.exists(),
        "vendor/ should not be created in dry-run mode"
    );
}
