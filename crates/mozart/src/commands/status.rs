use clap::Args;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct StatusArgs {
    /// Show a list of files for each modified package (implied by -v)
    #[arg(short, long)]
    pub verbose: bool,
}

// ─── Data structures ────────────────────────────────────────────────────────

/// Information extracted from a package's dist field.
struct DistInfo {
    dist_type: String,
    url: String,
    shasum: Option<String>,
}

/// The kind of change detected for a file.
#[derive(Debug, PartialEq)]
enum ChangeKind {
    /// File was modified (exists in both, different hash).
    Modified,
    /// File was added in the installed copy (not in original archive).
    Added,
    /// File was removed from the installed copy (in original archive, not installed).
    Removed,
}

/// A single file change within a package.
struct FileChange {
    kind: ChangeKind,
    path: String,
}

/// Changes detected for one package.
struct PackageStatus {
    name: String,
    changes: Vec<FileChange>,
}

// ─── Main entry point ────────────────────────────────────────────────────────

pub fn execute(
    args: &StatusArgs,
    cli: &super::Cli,
    _console: &crate::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let vendor_dir = working_dir.join("vendor");
    let installed = crate::installed::InstalledPackages::read(&vendor_dir)?;

    if installed.packages.is_empty() {
        println!("No packages installed.");
        return Ok(());
    }

    let cache_config = crate::cache::build_cache_config(cli);
    let files_cache = crate::cache::Cache::files(&cache_config);

    let show_files = args.verbose || cli.verbose > 0;

    let mut modified_packages: Vec<PackageStatus> = Vec::new();

    for pkg in &installed.packages {
        let dist = match extract_dist_info(pkg) {
            Some(d) => d,
            None => {
                if cli.verbose > 1 {
                    eprintln!("  Skipping {} — no dist info available", pkg.name);
                }
                continue;
            }
        };

        // Resolve install path
        let install_path = resolve_install_path(pkg, &vendor_dir);
        if !install_path.exists() {
            if cli.verbose > 0 {
                eprintln!(
                    "  Skipping {} — install path does not exist: {}",
                    pkg.name,
                    install_path.display()
                );
            }
            continue;
        }

        if cli.verbose > 0 {
            eprintln!("  Checking {} ...", pkg.name);
        }

        // Download original archive to a temp dir
        let tmp_dir = make_temp_dir(&pkg.name)?;
        let downloaded = crate::downloader::download_dist(
            &dist.url,
            dist.shasum.as_deref(),
            None,
            Some(&files_cache),
        );

        let bytes = match downloaded {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  Warning: could not download dist for {}: {}", pkg.name, e);
                let _ = std::fs::remove_dir_all(&tmp_dir);
                continue;
            }
        };

        // Extract archive to temp dir
        let extract_result = match dist.dist_type.as_str() {
            "zip" => crate::downloader::extract_zip(&bytes, &tmp_dir),
            "tar" | "tar.gz" | "tgz" => crate::downloader::extract_tar_gz(&bytes, &tmp_dir),
            other => {
                eprintln!(
                    "  Warning: unsupported dist type '{}' for {}",
                    other, pkg.name
                );
                let _ = std::fs::remove_dir_all(&tmp_dir);
                continue;
            }
        };

        if let Err(e) = extract_result {
            eprintln!("  Warning: could not extract dist for {}: {}", pkg.name, e);
            let _ = std::fs::remove_dir_all(&tmp_dir);
            continue;
        }

        // Hash both directories
        let original_hashes = hash_directory(&tmp_dir)?;
        let installed_hashes = hash_directory(&install_path)?;
        let _ = std::fs::remove_dir_all(&tmp_dir);

        // Compute diff
        let changes = compute_diff(&original_hashes, &installed_hashes);

        if !changes.is_empty() {
            modified_packages.push(PackageStatus {
                name: pkg.name.clone(),
                changes,
            });
        }
    }

    if modified_packages.is_empty() {
        println!("No local changes");
        return Ok(());
    }

    println!("You have changes in the following dependencies:\n");

    for pkg_status in &modified_packages {
        // Show package name with indicator
        println!("vendor/{} (M)", pkg_status.name);

        if show_files {
            let mut sorted_changes: Vec<&FileChange> = pkg_status.changes.iter().collect();
            sorted_changes.sort_by_key(|c| c.path.as_str());

            for change in sorted_changes {
                let prefix = match change.kind {
                    ChangeKind::Modified => 'M',
                    ChangeKind::Added => '+',
                    ChangeKind::Removed => '-',
                };
                println!("    {} {}", prefix, change.path);
            }
        }
    }

    // Exit with code 1 if modifications found
    std::process::exit(1);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Extract dist info from an installed package entry.
fn extract_dist_info(pkg: &crate::installed::InstalledPackageEntry) -> Option<DistInfo> {
    // Try the strongly-typed `dist` field first
    let dist_val = pkg.dist.as_ref().or_else(|| pkg.extra_fields.get("dist"))?;

    let dist_type = dist_val.get("type").and_then(|v| v.as_str())?.to_string();
    let url = dist_val.get("url").and_then(|v| v.as_str())?.to_string();
    let shasum = dist_val
        .get("shasum")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if url.is_empty() {
        return None;
    }

    Some(DistInfo {
        dist_type,
        url,
        shasum,
    })
}

/// Resolve the on-disk install path for a package.
///
/// Prefers the `install-path` field from installed.json when available,
/// since it is a path relative to `vendor/composer/`. Falls back to
/// `vendor/<package-name>`.
fn resolve_install_path(
    pkg: &crate::installed::InstalledPackageEntry,
    vendor_dir: &Path,
) -> PathBuf {
    if let Some(ref rel) = pkg.install_path {
        // install-path is relative to vendor/composer/
        let base = vendor_dir.join("composer");
        let resolved = base.join(rel);
        // Normalize out ".." segments using canonicalize-like logic
        let resolved_str = resolved.to_string_lossy().into_owned();
        let mut components: Vec<&str> = Vec::new();
        for part in resolved_str.split('/') {
            match part {
                ".." => {
                    components.pop();
                }
                "." | "" => {}
                p => components.push(p),
            }
        }
        PathBuf::from("/".to_string() + &components.join("/"))
    } else {
        vendor_dir.join(&pkg.name)
    }
}

/// Create a unique temporary directory for extracting a package archive.
///
/// The directory is placed under the system temp dir and named using the
/// package name (with `/` replaced) and a timestamp-derived suffix so that
/// concurrent runs are unlikely to collide.  The caller is responsible for
/// removing the directory when done.
fn make_temp_dir(package_name: &str) -> anyhow::Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let safe_name = package_name.replace('/', "_");
    let dir = std::env::temp_dir().join(format!("mozart_status_{}_{}", safe_name, nanos));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Recursively hash all files in a directory.
///
/// Returns a map from relative path string to SHA-1 hex digest.
fn hash_directory(dir: &Path) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    hash_dir_recursive(dir, dir, &mut map)?;
    Ok(map)
}

fn hash_dir_recursive(
    root: &Path,
    current: &Path,
    map: &mut HashMap<String, String>,
) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;

        if metadata.is_dir() {
            hash_dir_recursive(root, &path, map)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();

            let contents = std::fs::read(&path)?;
            let mut hasher = Sha1::new();
            hasher.update(&contents);
            let hex = format!("{:x}", hasher.finalize());

            map.insert(relative, hex);
        }
        // Symlinks are skipped
    }

    Ok(())
}

/// Compare two hash maps (original vs installed) and return a list of changes.
fn compute_diff(
    original: &HashMap<String, String>,
    installed: &HashMap<String, String>,
) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();

    // Files in original: check for modifications and removals
    for (path, orig_hash) in original {
        match installed.get(path) {
            Some(inst_hash) if inst_hash != orig_hash => {
                changes.push(FileChange {
                    kind: ChangeKind::Modified,
                    path: path.clone(),
                });
            }
            Some(_) => {} // unchanged
            None => {
                changes.push(FileChange {
                    kind: ChangeKind::Removed,
                    path: path.clone(),
                });
            }
        }
    }

    // Files in installed but not in original: added
    for path in installed.keys() {
        if !original.contains_key(path) {
            changes.push(FileChange {
                kind: ChangeKind::Added,
                path: path.clone(),
            });
        }
    }

    changes
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ── hash_directory ────────────────────────────────────────────────────────

    #[test]
    fn test_hash_directory() {
        let dir = tempdir().unwrap();

        fs::write(dir.path().join("file.txt"), b"hello").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/nested.txt"), b"world").unwrap();

        let hashes = hash_directory(dir.path()).unwrap();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains_key("file.txt"));
        assert!(hashes.contains_key("sub/nested.txt"));

        // Same content → same hash
        let dir2 = tempdir().unwrap();
        fs::write(dir2.path().join("file.txt"), b"hello").unwrap();
        fs::create_dir(dir2.path().join("sub")).unwrap();
        fs::write(dir2.path().join("sub/nested.txt"), b"world").unwrap();

        let hashes2 = hash_directory(dir2.path()).unwrap();
        assert_eq!(hashes["file.txt"], hashes2["file.txt"]);
        assert_eq!(hashes["sub/nested.txt"], hashes2["sub/nested.txt"]);

        // Different content → different hash
        let dir3 = tempdir().unwrap();
        fs::write(dir3.path().join("file.txt"), b"different").unwrap();
        let hashes3 = hash_directory(dir3.path()).unwrap();
        assert_ne!(hashes["file.txt"], hashes3["file.txt"]);
    }

    // ── compute_diff_no_changes ───────────────────────────────────────────────

    #[test]
    fn test_compute_diff_no_changes() {
        let mut map: HashMap<String, String> = HashMap::new();
        map.insert("src/Foo.php".to_string(), "abc123".to_string());
        map.insert("src/Bar.php".to_string(), "def456".to_string());

        let changes = compute_diff(&map, &map);
        assert!(changes.is_empty());
    }

    // ── compute_diff_modified ─────────────────────────────────────────────────

    #[test]
    fn test_compute_diff_modified() {
        let mut original: HashMap<String, String> = HashMap::new();
        original.insert("src/Foo.php".to_string(), "abc123".to_string());

        let mut installed: HashMap<String, String> = HashMap::new();
        installed.insert("src/Foo.php".to_string(), "xyz999".to_string());

        let changes = compute_diff(&original, &installed);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Modified);
        assert_eq!(changes[0].path, "src/Foo.php");
    }

    // ── compute_diff_added ────────────────────────────────────────────────────

    #[test]
    fn test_compute_diff_added() {
        let original: HashMap<String, String> = HashMap::new();

        let mut installed: HashMap<String, String> = HashMap::new();
        installed.insert("src/NewFile.php".to_string(), "aabbcc".to_string());

        let changes = compute_diff(&original, &installed);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Added);
        assert_eq!(changes[0].path, "src/NewFile.php");
    }

    // ── compute_diff_removed ──────────────────────────────────────────────────

    #[test]
    fn test_compute_diff_removed() {
        let mut original: HashMap<String, String> = HashMap::new();
        original.insert("src/OldFile.php".to_string(), "112233".to_string());

        let installed: HashMap<String, String> = HashMap::new();

        let changes = compute_diff(&original, &installed);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Removed);
        assert_eq!(changes[0].path, "src/OldFile.php");
    }

    // ── compute_diff_mixed ────────────────────────────────────────────────────

    #[test]
    fn test_compute_diff_mixed() {
        let mut original: HashMap<String, String> = HashMap::new();
        original.insert("src/Unchanged.php".to_string(), "same".to_string());
        original.insert("src/Modified.php".to_string(), "old".to_string());
        original.insert("src/Removed.php".to_string(), "gone".to_string());

        let mut installed: HashMap<String, String> = HashMap::new();
        installed.insert("src/Unchanged.php".to_string(), "same".to_string());
        installed.insert("src/Modified.php".to_string(), "new".to_string());
        installed.insert("src/Added.php".to_string(), "extra".to_string());

        let changes = compute_diff(&original, &installed);
        assert_eq!(changes.len(), 3);

        let modified: Vec<_> = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Modified)
            .collect();
        let added: Vec<_> = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Added)
            .collect();
        let removed: Vec<_> = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Removed)
            .collect();

        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].path, "src/Modified.php");

        assert_eq!(added.len(), 1);
        assert_eq!(added[0].path, "src/Added.php");

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].path, "src/Removed.php");
    }

    // ── extract_dist_info ─────────────────────────────────────────────────────

    #[test]
    fn test_extract_dist_info_from_dist_field() {
        use std::collections::BTreeMap;

        let pkg = crate::installed::InstalledPackageEntry {
            name: "vendor/pkg".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: Some(serde_json::json!({
                "type": "zip",
                "url": "https://example.com/pkg.zip",
                "reference": "abc123",
                "shasum": "deadbeef"
            })),
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        };

        let info = extract_dist_info(&pkg).unwrap();
        assert_eq!(info.dist_type, "zip");
        assert_eq!(info.url, "https://example.com/pkg.zip");
        assert_eq!(info.shasum.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn test_extract_dist_info_no_url() {
        use std::collections::BTreeMap;

        let pkg = crate::installed::InstalledPackageEntry {
            name: "vendor/pkg".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: Some(serde_json::json!({
                "type": "zip",
                "url": "",
                "shasum": ""
            })),
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        };

        assert!(extract_dist_info(&pkg).is_none());
    }

    #[test]
    fn test_extract_dist_info_absent() {
        use std::collections::BTreeMap;

        let pkg = crate::installed::InstalledPackageEntry {
            name: "vendor/pkg".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        };

        assert!(extract_dist_info(&pkg).is_none());
    }

    // ── resolve_install_path ──────────────────────────────────────────────────

    #[test]
    fn test_resolve_install_path_default() {
        use std::collections::BTreeMap;

        let pkg = crate::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        };

        let vendor = PathBuf::from("/project/vendor");
        let path = resolve_install_path(&pkg, &vendor);
        assert_eq!(path, PathBuf::from("/project/vendor/monolog/monolog"));
    }

    #[test]
    fn test_resolve_install_path_with_install_path() {
        use std::collections::BTreeMap;

        let pkg = crate::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: Some("../monolog/monolog".to_string()),
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        };

        let vendor = PathBuf::from("/project/vendor");
        let path = resolve_install_path(&pkg, &vendor);
        assert_eq!(path, PathBuf::from("/project/vendor/monolog/monolog"));
    }
}
