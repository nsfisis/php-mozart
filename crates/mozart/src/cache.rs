//! Filesystem-backed cache system with TTL expiration and size-limited GC.
//!
//! Cache directory structure:
//! ```text
//! ~/.cache/mozart/          (or $COMPOSER_CACHE_DIR)
//!   files/                  dist archives (key: vendor~package~reference.ext)
//!   repo/                   API responses (key: provider-vendor~package.json)
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ─────────────────────────────────────────────────────────────────────────────
// CacheConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the Mozart cache system.
pub struct CacheConfig {
    /// Root cache directory (e.g. `~/.cache/mozart`).
    pub cache_dir: PathBuf,
    /// Directory for dist archives.
    pub cache_files_dir: PathBuf,
    /// Directory for API responses.
    pub cache_repo_dir: PathBuf,
    /// TTL in seconds for repo entries (default: 15,552,000 = 6 months).
    pub cache_ttl: u64,
    /// TTL in seconds for files entries (falls back to `cache_ttl`).
    pub cache_files_ttl: u64,
    /// Maximum size of the files cache in bytes (default: 300 MiB).
    pub cache_files_maxsize: u64,
    /// Whether the cache is read-only (no writes).
    pub read_only: bool,
    /// Whether caching is entirely disabled.
    pub no_cache: bool,
}

impl CacheConfig {
    /// Default TTL: 6 months in seconds.
    pub const DEFAULT_TTL: u64 = 15_552_000;
    /// Default max files cache size: 300 MiB.
    pub const DEFAULT_FILES_MAXSIZE: u64 = 300 * 1024 * 1024;
}

/// Build a `CacheConfig` from CLI flags and environment variables.
///
/// Respects `$COMPOSER_CACHE_DIR` for the base directory, and
/// `$COMPOSER_NO_CACHE` / `COMPOSER_CACHE_READ_ONLY` env vars.
pub fn build_cache_config(cli: &super::commands::Cli) -> CacheConfig {
    let no_cache = std::env::var("COMPOSER_NO_CACHE").is_ok() || cli.no_cache;

    let read_only = std::env::var("COMPOSER_CACHE_READ_ONLY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let cache_dir = if let Ok(dir) = std::env::var("COMPOSER_CACHE_DIR") {
        PathBuf::from(dir)
    } else {
        // Use XDG cache dir or fallback
        dirs_cache_dir().join("mozart")
    };

    let cache_files_dir = cache_dir.join("files");
    let cache_repo_dir = cache_dir.join("repo");

    CacheConfig {
        cache_files_dir,
        cache_repo_dir,
        cache_ttl: CacheConfig::DEFAULT_TTL,
        cache_files_ttl: CacheConfig::DEFAULT_TTL,
        cache_files_maxsize: CacheConfig::DEFAULT_FILES_MAXSIZE,
        cache_dir,
        read_only,
        no_cache,
    }
}

/// Return the platform cache directory (XDG_CACHE_HOME or ~/.cache).
fn dirs_cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(xdg);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    PathBuf::from("/tmp")
}

// ─────────────────────────────────────────────────────────────────────────────
// Cache
// ─────────────────────────────────────────────────────────────────────────────

/// A single cache bucket (a directory on disk).
#[derive(Clone)]
pub struct Cache {
    root: PathBuf,
    enabled: bool,
}

impl Cache {
    /// Create a new cache rooted at `root`.
    ///
    /// Creates the directory if it doesn't exist and caching is enabled.
    pub fn new(root: PathBuf, enabled: bool) -> Self {
        if enabled {
            let _ = fs::create_dir_all(&root);
        }
        Self { root, enabled }
    }

    /// Shorthand: create the repo cache from a `CacheConfig`.
    pub fn repo(config: &CacheConfig) -> Self {
        Self::new(config.cache_repo_dir.clone(), !config.no_cache)
    }

    /// Shorthand: create the files cache from a `CacheConfig`.
    pub fn files(config: &CacheConfig) -> Self {
        Self::new(config.cache_files_dir.clone(), !config.no_cache)
    }

    /// Whether caching is enabled for this bucket.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Sanitize a cache key for use as a filename.
    ///
    /// Replaces `/` with `~` and strips characters that are unsafe in
    /// filenames (anything except alphanumerics, `-`, `_`, `.`, `~`).
    pub fn sanitize_key(key: &str) -> String {
        key.replace('/', "~")
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
            .collect()
    }

    /// Return the full path for a cache entry.
    fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(Self::sanitize_key(key))
    }

    /// Read a cached string entry, or `None` if absent or cache disabled.
    pub fn read(&self, key: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        fs::read_to_string(self.path_for(key)).ok()
    }

    /// Write a string entry atomically (write to temp file, then rename).
    pub fn write(&self, key: &str, contents: &str) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.write_bytes(key, contents.as_bytes())
    }

    /// Read a cached binary entry, or `None` if absent or cache disabled.
    pub fn read_bytes(&self, key: &str) -> Option<Vec<u8>> {
        if !self.enabled {
            return None;
        }
        fs::read(self.path_for(key)).ok()
    }

    /// Write a binary entry atomically (write to temp file, then rename).
    pub fn write_bytes(&self, key: &str, data: &[u8]) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let dest = self.path_for(key);
        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write to a temp file next to the destination
        let tmp = dest.with_extension("tmp");
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &dest)?;
        Ok(())
    }

    /// Delete all cached entries in this bucket.
    pub fn clear(&self) -> anyhow::Result<()> {
        if !self.root.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(&path)?;
            } else if path.is_dir() {
                fs::remove_dir_all(&path)?;
            }
        }
        Ok(())
    }

    /// Run garbage collection on this cache bucket.
    ///
    /// 1. Deletes files with mtime older than `ttl_seconds`.
    /// 2. If total remaining size > `max_size_bytes`, deletes the oldest files
    ///    (by mtime) until the total is under the limit.
    pub fn gc(&self, ttl_seconds: u64, max_size_bytes: u64) -> anyhow::Result<()> {
        if !self.enabled || !self.root.exists() {
            return Ok(());
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Collect (path, mtime, size) for all files
        let mut files: Vec<(PathBuf, u64, u64)> = Vec::new();
        collect_files(&self.root, &mut files)?;

        // Phase 1: delete TTL-expired files
        let mut remaining: Vec<(PathBuf, u64, u64)> = Vec::new();
        for (path, mtime, size) in files {
            let age = now.saturating_sub(mtime);
            if age > ttl_seconds {
                let _ = fs::remove_file(&path);
            } else {
                remaining.push((path, mtime, size));
            }
        }

        // Phase 2: enforce size limit by deleting oldest first
        let total_size: u64 = remaining.iter().map(|(_, _, sz)| sz).sum();
        if total_size > max_size_bytes {
            // Sort by mtime ascending (oldest first)
            remaining.sort_by_key(|(_, mtime, _)| *mtime);
            let mut current_size = total_size;
            for (path, _, size) in &remaining {
                if current_size <= max_size_bytes {
                    break;
                }
                if fs::remove_file(path).is_ok() {
                    current_size = current_size.saturating_sub(*size);
                }
            }
        }

        Ok(())
    }

    /// Return the age in seconds of a cached entry based on its mtime,
    /// or `None` if the entry doesn't exist or mtime can't be read.
    pub fn age(&self, key: &str) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        let path = self.path_for(key);
        let metadata = fs::metadata(&path).ok()?;
        let mtime = metadata.modified().ok()?;
        let now = SystemTime::now();
        now.duration_since(mtime).ok().map(|d| d.as_secs())
    }
}

/// Recursively collect all files under `dir` as `(path, mtime_secs, size_bytes)`.
fn collect_files(dir: &Path, out: &mut Vec<(PathBuf, u64, u64)>) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_files(&path, out)?;
        } else if metadata.is_file() {
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let size = metadata.len();
            out.push((path, mtime, size));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Probabilistic GC trigger
// ─────────────────────────────────────────────────────────────────────────────

/// Return `true` with a probability of 1 in 50 (based on system time nanos).
///
/// Used to decide whether to run GC after an install/update operation.
pub fn gc_is_necessary() -> bool {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    nanos.is_multiple_of(50)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    // ──────────── sanitize_key ────────────

    #[test]
    fn test_sanitize_key_replaces_slash() {
        assert_eq!(Cache::sanitize_key("vendor/package"), "vendor~package");
    }

    #[test]
    fn test_sanitize_key_strips_unsafe_chars() {
        // Colons and spaces should be stripped
        assert_eq!(Cache::sanitize_key("foo:bar baz"), "foobarbaz");
    }

    #[test]
    fn test_sanitize_key_preserves_safe_chars() {
        let key = "provider-vendor~package.json";
        assert_eq!(Cache::sanitize_key(key), key);
    }

    #[test]
    fn test_sanitize_key_full_example() {
        assert_eq!(
            Cache::sanitize_key("provider-monolog/monolog.json"),
            "provider-monolog~monolog.json"
        );
    }

    // ──────────── read/write roundtrip (string) ────────────

    #[test]
    fn test_write_read_roundtrip_string() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), true);

        cache.write("test-key", "hello world").unwrap();
        let result = cache.read("test-key");
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    // ──────────── read/write roundtrip (bytes) ────────────

    #[test]
    fn test_write_read_roundtrip_bytes() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), true);

        let data = vec![0u8, 1, 2, 3, 255];
        cache.write_bytes("bin-key", &data).unwrap();
        let result = cache.read_bytes("bin-key");
        assert_eq!(result, Some(data));
    }

    // ──────────── clear removes all entries ────────────

    #[test]
    fn test_clear_removes_all_entries() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), true);

        cache.write("key1", "value1").unwrap();
        cache.write("key2", "value2").unwrap();
        assert!(cache.read("key1").is_some());
        assert!(cache.read("key2").is_some());

        cache.clear().unwrap();

        assert!(cache.read("key1").is_none());
        assert!(cache.read("key2").is_none());
    }

    // ──────────── disabled cache returns None ────────────

    #[test]
    fn test_disabled_cache_returns_none() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), false);

        // Write should silently succeed (no-op)
        cache.write("key", "value").unwrap();

        // Read should return None even if we wrote
        assert!(cache.read("key").is_none());
        assert!(cache.read_bytes("key").is_none());
    }

    // ──────────── GC with TTL expiration ────────────

    #[test]
    fn test_gc_ttl_expiration() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), true);

        // Write a file, then manually set its mtime to the past
        cache.write("old-key", "old content").unwrap();
        let old_path = dir.path().join(Cache::sanitize_key("old-key"));

        // Write a fresh file
        cache.write("new-key", "new content").unwrap();

        // Set the old file's mtime to 2 hours ago
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(
            &old_path,
            filetime::FileTime::from_system_time(two_hours_ago),
        )
        .unwrap();

        // GC with TTL of 1 hour (3600 seconds)
        cache.gc(3600, u64::MAX).unwrap();

        // Old file should be deleted, new file should remain
        assert!(
            cache.read("old-key").is_none(),
            "expired file should be deleted"
        );
        assert!(cache.read("new-key").is_some(), "fresh file should remain");
    }

    // ──────────── GC with size limit ────────────

    #[test]
    fn test_gc_size_limit() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), true);

        // Write two files; the first one should be older
        cache.write("old-file", "aaaaaaaaaa").unwrap(); // 10 bytes
        let old_path = dir.path().join(Cache::sanitize_key("old-file"));

        // Add a small delay before writing second file via mtime manipulation
        cache.write("new-file", "bbbbbbbbbb").unwrap(); // 10 bytes

        // Set old-file's mtime to 1 second ago so it's older
        let one_second_ago = SystemTime::now() - Duration::from_secs(1);
        filetime::set_file_mtime(
            &old_path,
            filetime::FileTime::from_system_time(one_second_ago),
        )
        .unwrap();

        // GC with a max size of 12 bytes (can only fit one 10-byte file)
        // TTL is very long so no TTL expiration
        cache.gc(u64::MAX / 2, 12).unwrap();

        // The older file should be removed to get under the size limit
        assert!(
            cache.read("old-file").is_none() || cache.read("new-file").is_none(),
            "at least one file should be removed to enforce size limit"
        );
    }

    // ──────────── age ────────────

    #[test]
    fn test_age_existing_entry() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), true);

        cache.write("fresh-key", "content").unwrap();
        let age = cache.age("fresh-key");

        // Should be very recent (< 5 seconds)
        assert!(age.is_some());
        assert!(age.unwrap() < 5);
    }

    #[test]
    fn test_age_missing_entry() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), true);
        assert!(cache.age("nonexistent-key").is_none());
    }

    #[test]
    fn test_age_disabled_cache() {
        let dir = tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), false);
        assert!(cache.age("any-key").is_none());
    }
}
