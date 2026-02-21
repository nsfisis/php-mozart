mod common;

#[test]
fn test_init_creates_composer_json() {
    let dir = tempfile::TempDir::new().unwrap();
    common::mozart_cmd()
        .arg("init")
        .arg("--name")
        .arg("test/new-project")
        .arg("--no-interaction")
        .arg("--working-dir")
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        dir.path().join("composer.json").exists(),
        "composer.json should have been created"
    );

    let content =
        std::fs::read_to_string(dir.path().join("composer.json")).expect("Failed to read");
    let json: serde_json::Value = serde_json::from_str(&content).expect("Should be valid JSON");
    assert_eq!(json["name"], serde_json::json!("test/new-project"));
}
