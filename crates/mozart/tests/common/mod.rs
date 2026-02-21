use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Returns a `Command` configured to run the `mozart` binary.
pub fn mozart_cmd() -> assert_cmd::Command {
    assert_cmd::cargo::cargo_bin_cmd!("mozart")
}

/// Returns the absolute path to `tests/fixtures/<name>`.
pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// A temporary project directory that is cleaned up when dropped.
pub struct TestProject {
    #[allow(dead_code)]
    pub dir: TempDir,
}

impl TestProject {
    /// Returns the path to the temp directory root.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Create a temporary project with just a `composer.json`.
#[allow(dead_code)]
pub fn setup_temp_project(composer_json: &str) -> TestProject {
    let dir = TempDir::new().expect("Failed to create temp dir");
    fs::write(dir.path().join("composer.json"), composer_json)
        .expect("Failed to write composer.json");
    TestProject { dir }
}

/// Create a temporary project with both `composer.json` and `composer.lock`.
#[allow(dead_code)]
pub fn setup_temp_project_with_lock(composer_json: &str, composer_lock: &str) -> TestProject {
    let dir = TempDir::new().expect("Failed to create temp dir");
    fs::write(dir.path().join("composer.json"), composer_json)
        .expect("Failed to write composer.json");
    fs::write(dir.path().join("composer.lock"), composer_lock)
        .expect("Failed to write composer.lock");
    TestProject { dir }
}

/// Copy an entire fixture directory to a new temp directory.
#[allow(dead_code)]
pub fn copy_fixture_to_temp(fixture_name: &str) -> TestProject {
    let src = fixture_dir(fixture_name);
    let dir = TempDir::new().expect("Failed to create temp dir");
    copy_dir_recursive(&src, dir.path()).expect("Failed to copy fixture");
    TestProject { dir }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir_all(&dst_path)?;
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}
