mod common;

use predicates::str::contains;

#[test]
fn test_about_prints_version() {
    common::mozart_cmd()
        .arg("about")
        .assert()
        .success()
        .stdout(contains("Mozart"));
}
