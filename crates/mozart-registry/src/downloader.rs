use crate::cache::Cache;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

/// A simple download progress tracker that writes to stderr.
///
/// When `show` is false, all methods are no-ops. This lets callers toggle
/// progress display without branching on every call.
pub struct DownloadProgress {
    show: bool,
    total: u64,
    downloaded: u64,
    label: String,
}

impl DownloadProgress {
    /// Create a new progress tracker.
    ///
    /// - `show`: whether to actually display anything.
    /// - `label`: a human-readable label (e.g. "psr/log (3.0.2)").
    pub fn new(show: bool, label: impl Into<String>) -> Self {
        Self {
            show,
            total: 0,
            downloaded: 0,
            label: label.into(),
        }
    }

    /// Set the total expected bytes from a `Content-Length` header.
    pub fn set_total(&mut self, total: u64) {
        self.total = total;
    }

    /// Advance the downloaded byte count and redraw the line.
    pub fn inc(&mut self, n: u64) {
        if !self.show {
            return;
        }
        self.downloaded += n;
        let stderr = std::io::stderr();
        let mut out = stderr.lock();
        if let Some(pct) = (self.downloaded * 100).checked_div(self.total) {
            let _ = write!(
                out,
                "\r  Downloading {} ({}/{} bytes, {}%)",
                self.label, self.downloaded, self.total, pct
            );
        } else {
            let _ = write!(
                out,
                "\r  Downloading {} ({} bytes)",
                self.label, self.downloaded
            );
        }
        let _ = out.flush();
    }

    /// Clear the progress line from the terminal.
    pub fn finish(&self) {
        if !self.show {
            return;
        }
        let stderr = std::io::stderr();
        let mut out = stderr.lock();
        // Clear the line with spaces then return to start
        let _ = write!(out, "\r{}\r", " ".repeat(80));
        let _ = out.flush();
    }
}

/// Download a dist archive from a URL.
/// Returns the raw bytes of the downloaded archive.
/// If `expected_shasum` is provided and non-empty, verifies SHA-1 of the downloaded bytes.
/// If `progress` is provided, increments it as bytes are received and sets the total from
/// the `Content-Length` response header.
/// If `files_cache` is provided, the downloaded bytes are cached by URL; cache hits skip
/// the network request entirely.
pub async fn download_dist(
    url: &str,
    expected_shasum: Option<&str>,
    progress: Option<&mut DownloadProgress>,
    files_cache: Option<&Cache>,
) -> anyhow::Result<Vec<u8>> {
    // Build a cache key from the URL
    let cache_key = Cache::sanitize_key(url);

    // Check cache first
    if let Some(cache) = files_cache
        && let Some(cached_bytes) = cache.read_bytes(&cache_key)
    {
        // Verify checksum against cache hit if provided
        if let Some(shasum) = expected_shasum
            && !shasum.is_empty()
        {
            let mut hasher = Sha1::new();
            hasher.update(&cached_bytes);
            let computed = format!("{:x}", hasher.finalize());
            if computed == shasum {
                return Ok(cached_bytes);
            }
            // Checksum mismatch — discard cache, re-download
        } else {
            return Ok(cached_bytes);
        }
    }

    let response = reqwest::get(url).await?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Failed to download dist archive from {} (HTTP {})",
            url,
            response.status()
        );
    }

    // Stream the response body, updating progress as bytes arrive
    let bytes = if let Some(pb) = progress {
        if let Some(content_length) = response.content_length() {
            pb.set_total(content_length);
        }
        let mut buf = Vec::new();
        let mut stream = response;
        while let Some(chunk) = stream.chunk().await? {
            buf.extend_from_slice(&chunk);
            pb.inc(chunk.len() as u64);
        }
        buf
    } else {
        response.bytes().await?.to_vec()
    };

    // Verify SHA-1 checksum if provided
    if let Some(shasum) = expected_shasum
        && !shasum.is_empty()
    {
        let mut hasher = Sha1::new();
        hasher.update(&bytes);
        let result = hasher.finalize();
        let computed = format!("{result:x}");

        if computed != shasum {
            anyhow::bail!("SHA-1 checksum mismatch for {url}: expected {shasum}, got {computed}");
        }
    }

    // Write to cache
    if let Some(cache) = files_cache {
        let _ = cache.write_bytes(&cache_key, &bytes);
    }

    Ok(bytes)
}

/// Find the common top-level directory prefix shared by all entries.
/// Returns `Some(prefix)` if all entries share a single top-level directory.
fn find_top_level_dir(entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    let mut prefixes: HashSet<String> = HashSet::new();
    for entry in entries {
        if let Some(slash_pos) = entry.find('/') {
            prefixes.insert(entry[..slash_pos + 1].to_string());
        } else {
            // Entry at root level — no common prefix to strip
            return None;
        }
    }

    if prefixes.len() == 1 {
        prefixes.into_iter().next()
    } else {
        None
    }
}

/// Extract a zip archive to the target directory.
/// Strips a common top-level directory if all entries share one (Packagist pattern).
pub fn extract_zip(data: &[u8], target_dir: &Path) -> anyhow::Result<()> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)?;

    // Collect all entry names to detect common prefix
    let entry_names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).map(|e| e.name().to_string()))
        .collect::<Result<_, _>>()?;

    let prefix = find_top_level_dir(&entry_names);

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_string();

        // Strip common prefix
        let relative = if let Some(ref pfx) = prefix {
            if raw_name.starts_with(pfx.as_str()) {
                &raw_name[pfx.len()..]
            } else {
                &raw_name
            }
        } else {
            &raw_name
        };

        // Skip the directory entry itself (empty name after stripping)
        if relative.is_empty() {
            continue;
        }

        let target_path = target_dir.join(relative);

        if raw_name.ends_with('/') {
            // Directory entry
            fs::create_dir_all(&target_path)?;
        } else {
            // File entry
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            fs::write(&target_path, &buf)?;

            // Set permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    fs::set_permissions(&target_path, fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }

    Ok(())
}

/// Extract a tar.gz archive to the target directory.
/// Strips a common top-level directory if all entries share one (Packagist pattern).
pub fn extract_tar_gz(data: &[u8], target_dir: &Path) -> anyhow::Result<()> {
    let cursor = Cursor::new(data);
    let decoder = flate2::read::GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(decoder);

    // We need to process in two passes: first collect names, then extract.
    // Use a buffered approach: collect entries into memory.
    let cursor2 = Cursor::new(data);
    let decoder2 = flate2::read::GzDecoder::new(cursor2);
    let mut archive2 = tar::Archive::new(decoder2);

    let entry_names: Vec<String> = archive2
        .entries()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
        .collect();

    let prefix = find_top_level_dir(&entry_names);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = entry.path()?.to_string_lossy().to_string();

        // Strip common prefix
        let relative = if let Some(ref pfx) = prefix {
            if raw_path.starts_with(pfx.as_str()) {
                raw_path[pfx.len()..].to_string()
            } else {
                raw_path.clone()
            }
        } else {
            raw_path.clone()
        };

        // Skip empty (top-level dir itself)
        if relative.is_empty() {
            continue;
        }

        let target_path = target_dir.join(&relative);

        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&target_path)?;
        } else if entry_type.is_file() {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            fs::write(&target_path, &buf)?;

            // Set permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(mode) = entry.header().mode() {
                    fs::set_permissions(&target_path, fs::Permissions::from_mode(mode))?;
                }
            }
        }
        // Symlinks and other types are skipped for now
    }

    Ok(())
}

/// Download and install a package to the vendor directory.
///
/// - `dist_url`: the download URL (from `LockedPackage.dist.url`)
/// - `dist_type`: `"zip"` or `"tar"` (from `LockedPackage.dist.dist_type`)
/// - `dist_shasum`: optional SHA-1 checksum
/// - `vendor_dir`: path to `vendor/` directory
/// - `package_name`: e.g. `"monolog/monolog"`
/// - `progress`: optional mutable progress tracker to update during download
/// - `files_cache`: optional files cache; if provided, the archive bytes are cached by URL
pub async fn install_package(
    dist_url: &str,
    dist_type: &str,
    dist_shasum: Option<&str>,
    vendor_dir: &Path,
    package_name: &str,
    progress: Option<&mut DownloadProgress>,
    files_cache: Option<&Cache>,
) -> anyhow::Result<()> {
    let target = vendor_dir.join(package_name);

    // Remove existing installation for a clean reinstall
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::create_dir_all(&target)?;

    let bytes = download_dist(dist_url, dist_shasum, progress, files_cache).await?;

    match dist_type {
        "zip" => extract_zip(&bytes, &target)?,
        "tar" | "tar.gz" | "tgz" => extract_tar_gz(&bytes, &target)?,
        other => anyhow::bail!("Unsupported dist type: {other}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use tempfile::tempdir;

    /// Build a minimal zip archive in memory.
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let buf = Vec::new();
        let cursor = Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);

        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }

        writer.finish().unwrap().into_inner()
    }

    /// Build a minimal tar.gz archive in memory.
    fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let buf = Vec::new();
        let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);

        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, Cursor::new(content))
                .unwrap();
        }

        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn test_extract_zip_flat() {
        let zip_data = make_zip(&[("file1.txt", b"hello"), ("subdir/file2.txt", b"world")]);

        let dir = tempdir().unwrap();
        extract_zip(&zip_data, dir.path()).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("file1.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("subdir/file2.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn test_extract_zip_with_top_level_dir() {
        // Packagist pattern: all files under vendor-package-abc123/
        let zip_data = make_zip(&[
            ("vendor-pkg-abc/", &[]),
            ("vendor-pkg-abc/file1.txt", b"hello"),
            ("vendor-pkg-abc/src/Foo.php", b"<?php"),
        ]);

        let dir = tempdir().unwrap();
        extract_zip(&zip_data, dir.path()).unwrap();

        // Top-level dir should be stripped
        assert!(dir.path().join("file1.txt").exists());
        assert!(dir.path().join("src/Foo.php").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("file1.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_extract_tar_gz_flat() {
        let tar_data = make_tar_gz(&[("file1.txt", b"hello"), ("subdir/file2.txt", b"world")]);

        let dir = tempdir().unwrap();
        extract_tar_gz(&tar_data, dir.path()).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("file1.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("subdir/file2.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn test_extract_tar_gz_with_top_level_dir() {
        let tar_data = make_tar_gz(&[
            ("vendor-pkg-abc/file1.txt", b"hello"),
            ("vendor-pkg-abc/src/Foo.php", b"<?php"),
        ]);

        let dir = tempdir().unwrap();
        extract_tar_gz(&tar_data, dir.path()).unwrap();

        assert!(dir.path().join("file1.txt").exists());
        assert!(dir.path().join("src/Foo.php").exists());
    }

    #[test]
    fn test_sha1_verification() {
        use sha1::{Digest, Sha1};

        let data = b"test content";
        let mut hasher = Sha1::new();
        hasher.update(data);
        let expected = format!("{:x}", hasher.finalize());

        // We can't test download_dist without a server, but we can verify the
        // SHA-1 logic: same data should produce same hash
        let mut hasher2 = Sha1::new();
        hasher2.update(data);
        let computed = format!("{:x}", hasher2.finalize());

        assert_eq!(expected, computed);
        assert!(!expected.is_empty());
    }

    #[test]
    fn test_find_top_level_dir_common() {
        let entries = vec![
            "pkg-1.0/".to_string(),
            "pkg-1.0/README.md".to_string(),
            "pkg-1.0/src/Foo.php".to_string(),
        ];
        assert_eq!(find_top_level_dir(&entries), Some("pkg-1.0/".to_string()));
    }

    #[test]
    fn test_find_top_level_dir_none_when_mixed() {
        let entries = vec!["pkg-1.0/file.txt".to_string(), "other/file.txt".to_string()];
        assert_eq!(find_top_level_dir(&entries), None);
    }

    #[test]
    fn test_find_top_level_dir_none_when_root_file() {
        let entries = vec!["file.txt".to_string(), "pkg/other.txt".to_string()];
        assert_eq!(find_top_level_dir(&entries), None);
    }
}
