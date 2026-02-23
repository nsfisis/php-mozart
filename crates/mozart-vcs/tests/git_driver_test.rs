use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use mozart_vcs::downloader::VcsDownloader;
use mozart_vcs::downloader::git::GitDownloader;
use mozart_vcs::driver::git::GitDriver;
use mozart_vcs::driver::{DriverConfig, VcsDriver};
use mozart_vcs::process::ProcessExecutor;
use mozart_vcs::util::git::GitUtil;

fn has_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn create_test_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let output = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Command failed: {:?}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run(&["git", "init", "-b", "main"]);
    run(&["git", "config", "user.email", "test@test.com"]);
    run(&["git", "config", "user.name", "Test"]);

    // Create composer.json
    std::fs::write(
        dir.join("composer.json"),
        r#"{"name": "test/package", "description": "Test package"}"#,
    )
    .unwrap();

    run(&["git", "add", "."]);
    run(&["git", "commit", "-m", "Initial commit"]);

    // Create a tag
    run(&["git", "tag", "v1.0.0"]);

    // Create another commit on main
    std::fs::write(dir.join("README.md"), "# Test").unwrap();
    run(&["git", "add", "."]);
    run(&["git", "commit", "-m", "Add readme"]);

    // Create a second tag
    run(&["git", "tag", "v1.1.0"]);

    // Create a feature branch
    run(&["git", "checkout", "-b", "feature/test"]);
    std::fs::write(dir.join("feature.txt"), "feature").unwrap();
    run(&["git", "add", "."]);
    run(&["git", "commit", "-m", "Feature commit"]);
    run(&["git", "checkout", "main"]);
}

#[test]
fn test_git_driver_local_repo() {
    if !has_git() {
        eprintln!("Skipping test: git not available");
        return;
    }

    let repo_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    create_test_repo(repo_dir.path());

    let config = DriverConfig {
        cache_dir: cache_dir.path().to_path_buf(),
        ..DriverConfig::default()
    };

    let mut driver = GitDriver::new(repo_dir.path().to_str().unwrap(), config);

    driver.initialize().unwrap();
    assert_eq!(driver.root_identifier(), "main");

    // Check tags
    let tags = driver.tags().unwrap().clone();
    assert!(
        tags.contains_key("v1.0.0"),
        "Missing tag v1.0.0: {:?}",
        tags
    );
    assert!(
        tags.contains_key("v1.1.0"),
        "Missing tag v1.1.0: {:?}",
        tags
    );

    // Check branches
    let branches = driver.branches().unwrap().clone();
    assert!(
        branches.contains_key("main"),
        "Missing branch main: {:?}",
        branches
    );
    assert!(
        branches.contains_key("feature/test"),
        "Missing branch feature/test: {:?}",
        branches,
    );

    // Read composer.json
    let tag_hash = &tags["v1.0.0"];
    let info = driver.composer_information(tag_hash).unwrap();
    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(info["name"].as_str(), Some("test/package"));

    // Read file content
    let content = driver.file_content("composer.json", tag_hash).unwrap();
    assert!(content.is_some());
    assert!(content.unwrap().contains("test/package"));

    // Change date
    let date = driver.change_date(tag_hash).unwrap();
    assert!(date.is_some());

    // Source reference
    let source = driver.source(tag_hash);
    assert_eq!(source.source_type, "git");

    driver.cleanup().unwrap();
}

#[test]
fn test_git_downloader() {
    if !has_git() {
        eprintln!("Skipping test: git not available");
        return;
    }

    let repo_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    let install_dir = TempDir::new().unwrap();
    create_test_repo(repo_dir.path());

    let process = ProcessExecutor::new();
    let git_util = GitUtil::new(process, cache_dir.path().join("git"));
    let downloader = GitDownloader::new(git_util);

    let url = repo_dir.path().to_str().unwrap();
    let target = install_dir.path().join("test-package");

    // Download (sync mirror)
    downloader.download(url, "v1.0.0", &target).unwrap();

    // Install
    downloader.install(url, "v1.0.0", &target).unwrap();
    assert!(target.join("composer.json").exists());

    // Check no local changes
    let changes = downloader.local_changes(&target).unwrap();
    assert!(changes.is_none(), "Expected no changes, got: {:?}", changes);

    // Make a local change and detect it
    std::fs::write(target.join("local_change.txt"), "change").unwrap();
    let changes = downloader.local_changes(&target).unwrap();
    assert!(changes.is_some());
    assert!(changes.unwrap().contains("local_change.txt"));

    // Commit logs
    let logs = downloader.commit_logs("v1.0.0", "v1.1.0", &target).unwrap();
    assert!(logs.contains("Add readme"));

    // Remove
    downloader.remove(&target).unwrap();
    assert!(!target.exists());
}

#[test]
fn test_detect_driver() {
    use mozart_vcs::driver::{DriverType, detect_driver};

    let config = DriverConfig::default();

    assert_eq!(
        detect_driver("https://github.com/owner/repo", None, &config),
        Some(DriverType::GitHub),
    );
    assert_eq!(
        detect_driver("git@github.com:owner/repo.git", None, &config),
        Some(DriverType::GitHub),
    );
    assert_eq!(
        detect_driver("https://gitlab.com/owner/repo", None, &config),
        Some(DriverType::GitLab),
    );
    assert_eq!(
        detect_driver("https://bitbucket.org/owner/repo", None, &config),
        Some(DriverType::Bitbucket),
    );
    assert_eq!(
        detect_driver("https://codeberg.org/owner/repo", None, &config),
        Some(DriverType::Forgejo),
    );
    assert_eq!(
        detect_driver("git://example.com/repo.git", None, &config),
        Some(DriverType::Git),
    );
    assert_eq!(
        detect_driver("svn://example.com/repo", None, &config),
        Some(DriverType::Svn),
    );

    // Forced type
    assert_eq!(
        detect_driver("https://example.com/repo", Some("git"), &config),
        Some(DriverType::Git),
    );
}

#[test]
fn test_vcs_repository_scan() {
    if !has_git() {
        eprintln!("Skipping test: git not available");
        return;
    }

    let repo_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    create_test_repo(repo_dir.path());

    let config = DriverConfig {
        cache_dir: cache_dir.path().to_path_buf(),
        ..DriverConfig::default()
    };

    let repo = mozart_vcs::repository::VcsRepository::new(
        repo_dir.path().to_str().unwrap().to_string(),
        None,
        config,
    );

    let versions = repo.scan().unwrap();
    assert!(!versions.is_empty(), "No versions found");

    // Should find tag versions
    let tag_versions: Vec<_> = versions
        .iter()
        .filter(|v| !v.version.starts_with("dev-"))
        .collect();
    assert!(!tag_versions.is_empty(), "No tag versions found");

    // Should find branch versions
    let dev_versions: Vec<_> = versions
        .iter()
        .filter(|v| v.version.starts_with("dev-"))
        .collect();
    assert!(!dev_versions.is_empty(), "No dev versions found");

    // Check default branch flag
    let default_versions: Vec<_> = versions.iter().filter(|v| v.is_default_branch).collect();
    assert_eq!(
        default_versions.len(),
        1,
        "Expected exactly one default branch version"
    );
}
