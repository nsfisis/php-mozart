use anyhow::Context as _;
use regex::Regex;
use sha1::{Digest, Sha1};
use std::fs;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

// ─── Exclude filters ─────────────────────────────────────────────────────────

/// A compiled exclude pattern derived from a gitignore-style rule.
pub struct ExcludePattern {
    regex: Regex,
    /// If true, matching files are *re-included* (negation rule).
    negate: bool,
}

/// Convert a glob pattern string to a regex string.
///
/// Mapping:
/// - `**`  → `.*`   (matches any path segment sequence)
/// - `*`   → `[^/]*` (matches within a single path segment)
/// - `?`   → `[^/]`  (matches a single non-separator char)
/// - `[…]` → `[…]`   (character class, passed through)
/// - all other characters are regex-escaped
fn glob_to_regex(glob: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                result.push_str(".*");
                i += 2;
            }
            '*' => {
                result.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                result.push_str("[^/]");
                i += 1;
            }
            '[' => {
                // Pass character classes through as-is until the closing `]`
                result.push('[');
                i += 1;
                while i < chars.len() && chars[i] != ']' {
                    result.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    result.push(']');
                    i += 1;
                }
            }
            c => {
                // Regex-escape special characters
                if r"\.+^$|{}()?".contains(c) {
                    result.push('\\');
                }
                result.push(c);
                i += 1;
            }
        }
    }
    result
}

/// Convert a single gitignore-style rule into an `ExcludePattern`.
///
/// Returns `None` if the rule is empty or a comment.
pub fn parse_gitignore_pattern(rule: &str) -> Option<ExcludePattern> {
    let rule = rule.trim();
    if rule.is_empty() || rule.starts_with('#') {
        return None;
    }

    // Leading `!` negates the pattern
    let (negate, rule) = if let Some(rest) = rule.strip_prefix('!') {
        (true, rest)
    } else {
        (false, rule)
    };

    // Strip trailing `/` before globbing
    let rule = rule.trim_end_matches('/');
    if rule.is_empty() {
        return None;
    }

    // Determine anchor prefix:
    // - leading `/` → anchored at root: `^/<glob_regex>`
    // - no `/` inside pattern → matches anywhere: `/<glob_regex>`
    // - `/` somewhere in middle → anchored at root: `^/<glob_regex>`
    let (prefix, glob) = if let Some(without_leading_slash) = rule.strip_prefix('/') {
        // Root-anchored
        ("^/", without_leading_slash)
    } else if rule.contains('/') {
        // Slash in middle: treat as root-anchored
        ("^/", rule)
    } else {
        // No slash: matches anywhere
        ("/", rule)
    };

    let glob_regex = glob_to_regex(glob);
    // The final regex: `<prefix><glob_regex>(/|$)`
    // This matches the path component exactly (followed by a `/` or end-of-string).
    let pattern = format!("{prefix}{glob_regex}(/|$)");
    let regex = Regex::new(&pattern).ok()?;

    Some(ExcludePattern { regex, negate })
}

/// Apply a chain of exclude patterns to a relative path (as a `/`-prefixed string).
///
/// Patterns are applied in order; later patterns override earlier ones.
/// Returns `true` if the file is excluded by the final matching pattern
/// (or by `initially_excluded` if no pattern matches).
fn apply_filters(
    path_with_slash: &str,
    patterns: &[ExcludePattern],
    initially_excluded: bool,
) -> bool {
    let mut excluded = initially_excluded;
    for pat in patterns {
        if pat.regex.is_match(path_with_slash) {
            // A negate pattern re-includes; a normal pattern excludes
            excluded = !pat.negate;
        }
    }
    excluded
}

// ─── GitExcludeFilter ─────────────────────────────────────────────────────────

/// Parse `.gitattributes` from the source directory.
///
/// Returns exclude patterns for lines containing `export-ignore` or
/// `-export-ignore`.
pub fn parse_gitattributes(source_dir: &Path) -> Vec<ExcludePattern> {
    let path = source_dir.join(".gitattributes");
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut patterns = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let file_pattern = parts[0];
        // Check each attribute token for export-ignore / -export-ignore
        for attr in &parts[1..] {
            if *attr == "export-ignore" {
                if let Some(p) = parse_gitignore_pattern(file_pattern) {
                    patterns.push(p);
                }
            } else if *attr == "-export-ignore" {
                // Negation: re-include files that would otherwise be excluded
                let negated = format!("!{}", file_pattern);
                if let Some(p) = parse_gitignore_pattern(&negated) {
                    patterns.push(p);
                }
            }
        }
    }
    patterns
}

// ─── ComposerExcludeFilter ────────────────────────────────────────────────────

/// Convert `composer.json` `archive.exclude` rules into exclude patterns.
pub fn parse_composer_excludes(excludes: &[String]) -> Vec<ExcludePattern> {
    excludes
        .iter()
        .filter_map(|rule| parse_gitignore_pattern(rule))
        .collect()
}

// ─── VCS directory names ──────────────────────────────────────────────────────

const VCS_DIRS: &[&str] = &[".git", ".svn", ".hg", "CVS", ".bzr"];

// ─── File collection ──────────────────────────────────────────────────────────

/// Collect all archivable files from the source directory.
///
/// Returns paths relative to `source_dir`, sorted for deterministic output.
/// Applies `exclude_patterns` to filter files. VCS directories are always
/// skipped. Symlinks pointing outside `source_dir` are excluded.
pub fn collect_archivable_files(
    source_dir: &Path,
    exclude_patterns: &[ExcludePattern],
) -> anyhow::Result<Vec<PathBuf>> {
    let source_dir = source_dir
        .canonicalize()
        .unwrap_or_else(|_| source_dir.to_path_buf());
    let mut files = Vec::new();
    collect_recursive(&source_dir, &source_dir, exclude_patterns, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_recursive(
    source_dir: &Path,
    current_dir: &Path,
    exclude_patterns: &[ExcludePattern],
    out: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    let entries = fs::read_dir(current_dir)
        .with_context(|| format!("Failed to read directory: {}", current_dir.display()))?;

    let mut items: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    // Sort for determinism
    items.sort_by_key(|e| e.file_name());

    for entry in items {
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Skip VCS directories
        if VCS_DIRS.contains(&name_str.as_ref()) {
            continue;
        }

        // Compute the relative path (forward-slash, prefixed with `/` for filter matching)
        let relative = path
            .strip_prefix(source_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let path_with_slash = format!("/{}", relative);

        // Check if this entry is excluded
        if apply_filters(&path_with_slash, exclude_patterns, false) {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if metadata.is_symlink() {
            // Resolve the symlink; skip if it points outside source_dir
            if let Ok(resolved) = fs::canonicalize(&path) {
                if !resolved.starts_with(source_dir) {
                    continue;
                }
                out.push(PathBuf::from(&relative));
            }
            // If canonicalize fails, skip the symlink
        } else if metadata.is_dir() {
            // Collect children recursively
            let mut children = Vec::new();
            collect_recursive(source_dir, &path, exclude_patterns, &mut children)?;
            if children.is_empty() {
                // Include empty directory
                out.push(PathBuf::from(&relative));
            } else {
                out.extend(children);
            }
        } else {
            out.push(PathBuf::from(&relative));
        }
    }

    Ok(())
}

// ─── Archive formats ──────────────────────────────────────────────────────────

/// Supported archive formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
}

impl ArchiveFormat {
    /// Parse a format string (case-insensitive). Returns `None` for unsupported formats.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "zip" => Some(Self::Zip),
            "tar" => Some(Self::Tar),
            "tar.gz" | "tgz" => Some(Self::TarGz),
            "tar.bz2" => Some(Self::TarBz2),
            _ => None,
        }
    }

    /// File extension for this format.
    pub fn extension(&self) -> &str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarBz2 => "tar.bz2",
        }
    }
}

// ─── Archive creation ─────────────────────────────────────────────────────────

/// Create an archive of the given files.
///
/// - `source_dir`: the root of the source tree
/// - `files`: relative paths (as returned by `collect_archivable_files`)
/// - `target`: full output path including extension
/// - `format`: the archive format to create
pub fn create_archive(
    source_dir: &Path,
    files: &[PathBuf],
    target: &Path,
    format: &ArchiveFormat,
) -> anyhow::Result<()> {
    match format {
        ArchiveFormat::Zip => create_zip(source_dir, files, target),
        ArchiveFormat::Tar => create_tar(source_dir, files, target),
        ArchiveFormat::TarGz => create_tar_gz(source_dir, files, target),
        ArchiveFormat::TarBz2 => create_tar_bz2(source_dir, files, target),
    }
}

fn create_zip(source_dir: &Path, files: &[PathBuf], target: &Path) -> anyhow::Result<()> {
    use zip::write::SimpleFileOptions;

    let file = fs::File::create(target)
        .with_context(|| format!("Failed to create archive: {}", target.display()))?;
    let mut writer = zip::ZipWriter::new(file);

    for rel in files {
        let abs = source_dir.join(rel);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        if abs.is_dir() {
            let opts = SimpleFileOptions::default();
            writer.add_directory(&rel_str, opts)?;
        } else {
            let metadata = fs::metadata(&abs)?;

            #[cfg(unix)]
            let opts = {
                use std::os::unix::fs::MetadataExt;
                let mode = metadata.mode();
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(mode)
            };

            #[cfg(not(unix))]
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

            let _ = metadata; // suppress unused warning on non-unix

            writer.start_file(&rel_str, opts)?;
            let content = fs::read(&abs)?;
            writer.write_all(&content)?;
        }
    }

    writer.finish()?;
    Ok(())
}

fn create_tar(source_dir: &Path, files: &[PathBuf], target: &Path) -> anyhow::Result<()> {
    let file = fs::File::create(target)
        .with_context(|| format!("Failed to create archive: {}", target.display()))?;
    let mut builder = tar::Builder::new(file);

    for rel in files {
        let abs = source_dir.join(rel);
        if abs.is_dir() {
            builder.append_dir(rel, &abs)?;
        } else {
            builder.append_path_with_name(&abs, rel)?;
        }
    }

    builder.finish()?;
    Ok(())
}

fn create_tar_gz(source_dir: &Path, files: &[PathBuf], target: &Path) -> anyhow::Result<()> {
    let file = fs::File::create(target)
        .with_context(|| format!("Failed to create archive: {}", target.display()))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    for rel in files {
        let abs = source_dir.join(rel);
        if abs.is_dir() {
            builder.append_dir(rel, &abs)?;
        } else {
            builder.append_path_with_name(&abs, rel)?;
        }
    }

    builder.into_inner()?.finish()?;
    Ok(())
}

fn create_tar_bz2(source_dir: &Path, files: &[PathBuf], target: &Path) -> anyhow::Result<()> {
    let file = fs::File::create(target)
        .with_context(|| format!("Failed to create archive: {}", target.display()))?;
    let encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    for rel in files {
        let abs = source_dir.join(rel);
        if abs.is_dir() {
            builder.append_dir(rel, &abs)?;
        } else {
            builder.append_path_with_name(&abs, rel)?;
        }
    }

    builder.into_inner()?.finish()?;
    Ok(())
}

// ─── Filename generation ──────────────────────────────────────────────────────

/// Generate an archive filename (without extension) for a package.
///
/// Mirrors Composer's `ArchiveManager::getPackageFilenameParts()`.
pub fn generate_archive_filename(
    name: &str,
    archive_name: Option<&str>,
    version: Option<&str>,
    dist_reference: Option<&str>,
    dist_type: Option<&str>,
    source_reference: Option<&str>,
) -> String {
    // Base: archive_name if set, otherwise replace non-alphanumeric chars with `-`
    let base = if let Some(an) = archive_name {
        an.to_string()
    } else {
        let re = Regex::new(r"[^a-zA-Z0-9_\-]").unwrap();
        re.replace_all(name, "-").to_string()
    };

    let mut parts: Vec<String> = vec![base];

    // Determine if dist_reference is a 40-char hex (SHA-1 commit hash)
    let is_sha_dist_ref = dist_reference
        .map(|r| r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or(false);

    if is_sha_dist_ref {
        // Append dist_reference and dist_type
        if let Some(dr) = dist_reference {
            parts.push(dr.to_string());
        }
        if let Some(dt) = dist_type {
            parts.push(dt.to_string());
        }
    } else {
        // Append version (if any), then dist_reference (if any)
        if let Some(v) = version {
            parts.push(v.to_string());
        }
        if let Some(dr) = dist_reference {
            parts.push(dr.to_string());
        }
    }

    // Append first 6 chars of SHA-1 of source_reference (if any)
    if let Some(sr) = source_reference {
        let mut hasher = Sha1::new();
        hasher.update(sr.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        parts.push(hash[..6.min(hash.len())].to_string());
    }

    // Replace `/` with `-` in each part, then join
    parts
        .iter()
        .map(|p| p.replace('/', "-"))
        .collect::<Vec<_>>()
        .join("-")
}

// ─── Self-exclusion patterns ──────────────────────────────────────────────────

/// The set of archive extensions we support.
const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "tar", "tar.gz", "tar.bz2"];

/// Generate patterns to exclude previous archives of this package from the archive.
///
/// If `has_extra_parts` is true (version/ref was appended), the pattern is
/// `<base>-*.<ext>`. Otherwise it's `<base>.<ext>`.
pub fn self_exclusion_patterns(base_name: &str, has_extra_parts: bool) -> Vec<String> {
    ARCHIVE_EXTENSIONS
        .iter()
        .map(|ext| {
            if has_extra_parts {
                format!("/{}-*.{}", base_name, ext)
            } else {
                format!("/{}.{}", base_name, ext)
            }
        })
        .collect()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── glob_to_regex ─────────────────────────────────────────────────────────
    // Note: glob_to_regex produces a *fragment* for use inside a larger pattern.
    // We test it by embedding it in a full anchored regex.

    fn full_pattern(glob: &str) -> Regex {
        // Simulate the unanchored pattern: `/fragment(/|$)`
        Regex::new(&format!("/{glob_re}(/|$)", glob_re = glob_to_regex(glob))).unwrap()
    }

    #[test]
    fn test_glob_to_regex_star() {
        let re = full_pattern("*.txt");
        // Unanchored pattern: matches any .txt file at any depth
        assert!(re.is_match("/foo.txt"));
        // Also matches nested .txt files (unanchored `/` prefix)
        assert!(re.is_match("/a/b.txt"));
        // Does NOT match non-.txt files
        assert!(!re.is_match("/foo.php"));
    }

    #[test]
    fn test_glob_to_regex_double_star() {
        // Double star matches across path separators
        let frag = glob_to_regex("**/*.txt");
        let re = Regex::new(&format!("/{frag}(/|$)")).unwrap();
        assert!(re.is_match("/a/b/c.txt"));
    }

    #[test]
    fn test_glob_to_regex_question() {
        let frag = glob_to_regex("?.txt");
        let re = Regex::new(&format!("/{frag}(/|$)")).unwrap();
        assert!(re.is_match("/a.txt"));
        assert!(!re.is_match("/ab.txt"));
    }

    #[test]
    fn test_glob_to_regex_bracket() {
        let frag = glob_to_regex("[abc].txt");
        let re = Regex::new(&format!("/{frag}(/|$)")).unwrap();
        assert!(re.is_match("/a.txt"));
        assert!(re.is_match("/b.txt"));
        assert!(!re.is_match("/d.txt"));
    }

    // ── parse_gitignore_pattern ───────────────────────────────────────────────

    #[test]
    fn test_parse_gitignore_simple() {
        let pat = parse_gitignore_pattern("docs/").unwrap();
        assert!(!pat.negate);
        // "/docs" should match
        assert!(pat.regex.is_match("/docs"));
    }

    #[test]
    fn test_parse_gitignore_negated() {
        let pat = parse_gitignore_pattern("!important.txt").unwrap();
        assert!(pat.negate);
    }

    #[test]
    fn test_parse_gitignore_rooted() {
        let pat = parse_gitignore_pattern("/build").unwrap();
        assert!(!pat.negate);
        // Should match at root
        assert!(pat.regex.is_match("/build"));
        // Should NOT match in subdirectory (rooted pattern)
        assert!(!pat.regex.is_match("/src/build"));
    }

    #[test]
    fn test_parse_gitignore_unrooted() {
        let pat = parse_gitignore_pattern("*.log").unwrap();
        assert!(!pat.negate);
        // Should match anywhere
        assert!(pat.regex.is_match("/app.log"));
        assert!(pat.regex.is_match("/sub/dir/foo.log"));
    }

    // ── parse_gitattributes ───────────────────────────────────────────────────

    #[test]
    fn test_parse_gitattributes_export_ignore() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitattributes"), "tests/ export-ignore\n").unwrap();
        let patterns = parse_gitattributes(dir.path());
        assert_eq!(patterns.len(), 1);
        assert!(!patterns[0].negate);
        assert!(patterns[0].regex.is_match("/tests"));
    }

    #[test]
    fn test_parse_gitattributes_neg_export_ignore() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitattributes"), "tests/ -export-ignore\n").unwrap();
        let patterns = parse_gitattributes(dir.path());
        assert_eq!(patterns.len(), 1);
        assert!(patterns[0].negate);
    }

    #[test]
    fn test_parse_gitattributes_comment() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(".gitattributes"),
            "# comment\ntests/ export-ignore\n",
        )
        .unwrap();
        let patterns = parse_gitattributes(dir.path());
        assert_eq!(patterns.len(), 1);
    }

    #[test]
    fn test_parse_gitattributes_non_export() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitattributes"), "*.php text\n").unwrap();
        let patterns = parse_gitattributes(dir.path());
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_parse_gitattributes_missing_file() {
        let dir = tempdir().unwrap();
        let patterns = parse_gitattributes(dir.path());
        assert!(patterns.is_empty());
    }

    // ── collect_archivable_files ──────────────────────────────────────────────

    #[test]
    fn test_collect_files_basic() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.php"), b"<?php").unwrap();
        fs::write(dir.path().join("b.php"), b"<?php").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("c.php"), b"<?php").unwrap();

        let files = collect_archivable_files(dir.path(), &[]).unwrap();
        let strs: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(strs.contains(&"a.php".to_string()));
        assert!(strs.contains(&"b.php".to_string()));
        assert!(strs.contains(&"src/c.php".to_string()));
    }

    #[test]
    fn test_collect_files_excludes() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.php"), b"<?php").unwrap();
        fs::create_dir(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("tests").join("test.php"), b"<?php").unwrap();

        let patterns = vec![parse_gitignore_pattern("tests/").unwrap()];
        let files = collect_archivable_files(dir.path(), &patterns).unwrap();
        let strs: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(strs.contains(&"main.php".to_string()));
        assert!(!strs.iter().any(|s| s.starts_with("tests")));
    }

    #[test]
    fn test_collect_files_skips_vcs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.php"), b"<?php").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git").join("HEAD"),
            b"ref: refs/heads/main",
        )
        .unwrap();

        let files = collect_archivable_files(dir.path(), &[]).unwrap();
        let strs: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(strs.contains(&"main.php".to_string()));
        assert!(!strs.iter().any(|s| s.starts_with(".git")));
    }

    #[test]
    fn test_collect_files_empty_dir() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.php"), b"<?php").unwrap();
        fs::create_dir(dir.path().join("empty_dir")).unwrap();

        let files = collect_archivable_files(dir.path(), &[]).unwrap();
        let strs: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(strs.contains(&"main.php".to_string()));
        assert!(strs.contains(&"empty_dir".to_string()));
    }

    // ── create_archive ────────────────────────────────────────────────────────

    fn make_source_tree(dir: &Path) {
        fs::write(dir.join("main.php"), b"<?php echo 'hello';").unwrap();
        fs::create_dir(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("Foo.php"), b"<?php class Foo {}").unwrap();
    }

    #[test]
    fn test_create_zip_archive() {
        let src = tempdir().unwrap();
        make_source_tree(src.path());
        let out = tempdir().unwrap();
        let target = out.path().join("test.zip");

        let files = collect_archivable_files(src.path(), &[]).unwrap();
        create_archive(src.path(), &files, &target, &ArchiveFormat::Zip).unwrap();
        assert!(target.exists());

        // Verify contents
        let zip_data = fs::read(&target).unwrap();
        let cursor = std::io::Cursor::new(zip_data);
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"main.php".to_string()));
        assert!(names.contains(&"src/Foo.php".to_string()));
    }

    #[test]
    fn test_create_tar_archive() {
        let src = tempdir().unwrap();
        make_source_tree(src.path());
        let out = tempdir().unwrap();
        let target = out.path().join("test.tar");

        let files = collect_archivable_files(src.path(), &[]).unwrap();
        create_archive(src.path(), &files, &target, &ArchiveFormat::Tar).unwrap();
        assert!(target.exists());

        // Verify contents
        let tar_data = fs::read(&target).unwrap();
        let cursor = std::io::Cursor::new(tar_data);
        let mut archive = tar::Archive::new(cursor);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.php".to_string()));
        assert!(names.contains(&"src/Foo.php".to_string()));
    }

    #[test]
    fn test_create_tar_gz_archive() {
        let src = tempdir().unwrap();
        make_source_tree(src.path());
        let out = tempdir().unwrap();
        let target = out.path().join("test.tar.gz");

        let files = collect_archivable_files(src.path(), &[]).unwrap();
        create_archive(src.path(), &files, &target, &ArchiveFormat::TarGz).unwrap();
        assert!(target.exists());

        let gz_data = fs::read(&target).unwrap();
        let cursor = std::io::Cursor::new(gz_data);
        let decoder = flate2::read::GzDecoder::new(cursor);
        let mut archive = tar::Archive::new(decoder);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.php".to_string()));
    }

    #[test]
    fn test_create_tar_bz2_archive() {
        let src = tempdir().unwrap();
        make_source_tree(src.path());
        let out = tempdir().unwrap();
        let target = out.path().join("test.tar.bz2");

        let files = collect_archivable_files(src.path(), &[]).unwrap();
        create_archive(src.path(), &files, &target, &ArchiveFormat::TarBz2).unwrap();
        assert!(target.exists());

        let bz_data = fs::read(&target).unwrap();
        let cursor = std::io::Cursor::new(bz_data);
        let decoder = bzip2::read::BzDecoder::new(cursor);
        let mut archive = tar::Archive::new(decoder);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.php".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn test_zip_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let src = tempdir().unwrap();
        let script = src.path().join("run.sh");
        fs::write(&script, b"#!/bin/sh\necho hello").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let out = tempdir().unwrap();
        let target = out.path().join("test.zip");
        let files = collect_archivable_files(src.path(), &[]).unwrap();
        create_archive(src.path(), &files, &target, &ArchiveFormat::Zip).unwrap();

        let zip_data = fs::read(&target).unwrap();
        let cursor = std::io::Cursor::new(zip_data);
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        let entry = archive.by_name("run.sh").unwrap();
        let mode = entry.unix_mode().unwrap_or(0);
        // Lower 9 bits should be 0o755
        assert_eq!(mode & 0o777, 0o755);
    }

    // ── generate_archive_filename ─────────────────────────────────────────────

    #[test]
    fn test_filename_simple_package() {
        let name = generate_archive_filename("vendor/pkg", None, Some("1.2.3"), None, None, None);
        assert_eq!(name, "vendor-pkg-1.2.3");
    }

    #[test]
    fn test_filename_with_archive_name() {
        let name = generate_archive_filename(
            "vendor/pkg",
            Some("my-package"),
            Some("1.0.0"),
            None,
            None,
            None,
        );
        assert_eq!(name, "my-package-1.0.0");
    }

    #[test]
    fn test_filename_with_sha_dist_ref() {
        let sha = "a".repeat(40);
        let name = generate_archive_filename(
            "vendor/pkg",
            None,
            Some("1.0.0"),
            Some(&sha),
            Some("zip"),
            None,
        );
        // 40-char hex → append dist_ref and dist_type, not version
        assert_eq!(name, format!("vendor-pkg-{}-zip", sha));
    }

    #[test]
    fn test_filename_with_source_ref() {
        let name = generate_archive_filename(
            "vendor/pkg",
            None,
            Some("1.0.0"),
            None,
            None,
            Some("abc123"),
        );
        // Appends first 6 chars of SHA-1 of "abc123"
        let mut hasher = Sha1::new();
        hasher.update(b"abc123");
        let hash = format!("{:x}", hasher.finalize());
        let expected = format!("vendor-pkg-1.0.0-{}", &hash[..6]);
        assert_eq!(name, expected);
    }

    #[test]
    fn test_filename_slashes_replaced() {
        let name =
            generate_archive_filename("vendor/my-pkg", None, Some("1.0/beta"), None, None, None);
        assert_eq!(name, "vendor-my-pkg-1.0-beta");
    }

    // ── self_exclusion_patterns ───────────────────────────────────────────────

    #[test]
    fn test_self_exclusion_patterns_with_extra_parts() {
        let patterns = self_exclusion_patterns("vendor-pkg", true);
        assert!(patterns.contains(&"/vendor-pkg-*.zip".to_string()));
        assert!(patterns.contains(&"/vendor-pkg-*.tar".to_string()));
        assert!(patterns.contains(&"/vendor-pkg-*.tar.gz".to_string()));
        assert!(patterns.contains(&"/vendor-pkg-*.tar.bz2".to_string()));
    }

    #[test]
    fn test_self_exclusion_patterns_no_extra_parts() {
        let patterns = self_exclusion_patterns("vendor-pkg", false);
        assert!(patterns.contains(&"/vendor-pkg.zip".to_string()));
        assert!(patterns.contains(&"/vendor-pkg.tar".to_string()));
    }

    // ── Integration tests ─────────────────────────────────────────────────────

    #[test]
    fn test_archive_root_package_tar() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();
        std::fs::write(src.path().join("main.php"), b"<?php").unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("tar".to_string()),
            dir: Some(out.path().to_string_lossy().to_string()),
            file: Some("test-archive".to_string()),
            ignore_filters: false,
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("tar".to_string()),
                dir: Some(out.path().to_string_lossy().to_string()),
                file: Some("test-archive".to_string()),
                ignore_filters: false,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let archive_path = out.path().join("test-archive.tar");
        assert!(archive_path.exists(), "tar archive was not created");

        // Verify contents
        let tar_data = std::fs::read(&archive_path).unwrap();
        let cursor = std::io::Cursor::new(tar_data);
        let mut archive = tar::Archive::new(cursor);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.php".to_string()));
        assert!(names.contains(&"composer.json".to_string()));
    }

    #[test]
    fn test_archive_root_package_zip() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();
        std::fs::write(src.path().join("main.php"), b"<?php").unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("zip".to_string()),
            dir: Some(out.path().to_string_lossy().to_string()),
            file: Some("test-archive".to_string()),
            ignore_filters: false,
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("zip".to_string()),
                dir: Some(out.path().to_string_lossy().to_string()),
                file: Some("test-archive".to_string()),
                ignore_filters: false,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let archive_path = out.path().join("test-archive.zip");
        assert!(archive_path.exists(), "zip archive was not created");
    }

    #[test]
    fn test_archive_custom_dir() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let custom_out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("tar".to_string()),
            dir: Some(custom_out.path().to_string_lossy().to_string()),
            file: Some("custom".to_string()),
            ignore_filters: false,
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("tar".to_string()),
                dir: Some(custom_out.path().to_string_lossy().to_string()),
                file: Some("custom".to_string()),
                ignore_filters: false,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        assert!(custom_out.path().join("custom.tar").exists());
    }

    #[test]
    fn test_archive_custom_filename() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("tar".to_string()),
            dir: Some(out.path().to_string_lossy().to_string()),
            file: Some("my-custom-name".to_string()),
            ignore_filters: false,
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("tar".to_string()),
                dir: Some(out.path().to_string_lossy().to_string()),
                file: Some("my-custom-name".to_string()),
                ignore_filters: false,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        assert!(out.path().join("my-custom-name.tar").exists());
    }

    #[test]
    fn test_archive_gitattributes_filter() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();
        std::fs::write(src.path().join("main.php"), b"<?php").unwrap();
        std::fs::create_dir(src.path().join("tests")).unwrap();
        std::fs::write(src.path().join("tests").join("FooTest.php"), b"<?php").unwrap();
        std::fs::write(src.path().join(".gitattributes"), "tests/ export-ignore\n").unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("tar".to_string()),
            dir: Some(out.path().to_string_lossy().to_string()),
            file: Some("filtered".to_string()),
            ignore_filters: false,
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("tar".to_string()),
                dir: Some(out.path().to_string_lossy().to_string()),
                file: Some("filtered".to_string()),
                ignore_filters: false,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let tar_path = out.path().join("filtered.tar");
        assert!(tar_path.exists());

        let tar_data = std::fs::read(&tar_path).unwrap();
        let cursor = std::io::Cursor::new(tar_data);
        let mut archive = tar::Archive::new(cursor);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.php".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("tests")));
    }

    #[test]
    fn test_archive_composer_excludes() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}, "archive": {"exclude": ["/docs"]}}"#,
        )
        .unwrap();
        std::fs::write(src.path().join("main.php"), b"<?php").unwrap();
        std::fs::create_dir(src.path().join("docs")).unwrap();
        std::fs::write(src.path().join("docs").join("README.md"), b"# Docs").unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("tar".to_string()),
            dir: Some(out.path().to_string_lossy().to_string()),
            file: Some("with-excludes".to_string()),
            ignore_filters: false,
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("tar".to_string()),
                dir: Some(out.path().to_string_lossy().to_string()),
                file: Some("with-excludes".to_string()),
                ignore_filters: false,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let tar_path = out.path().join("with-excludes.tar");
        assert!(tar_path.exists());

        let tar_data = std::fs::read(&tar_path).unwrap();
        let cursor = std::io::Cursor::new(tar_data);
        let mut archive = tar::Archive::new(cursor);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.php".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("docs")));
    }

    #[test]
    fn test_archive_ignore_filters() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();
        std::fs::write(src.path().join("main.php"), b"<?php").unwrap();
        std::fs::create_dir(src.path().join("tests")).unwrap();
        std::fs::write(src.path().join("tests").join("FooTest.php"), b"<?php").unwrap();
        std::fs::write(src.path().join(".gitattributes"), "tests/ export-ignore\n").unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("tar".to_string()),
            dir: Some(out.path().to_string_lossy().to_string()),
            file: Some("unfiltered".to_string()),
            ignore_filters: true, // All filters ignored
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("tar".to_string()),
                dir: Some(out.path().to_string_lossy().to_string()),
                file: Some("unfiltered".to_string()),
                ignore_filters: true,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        execute(&args, &cli, &console).unwrap();

        let tar_path = out.path().join("unfiltered.tar");
        assert!(tar_path.exists());

        let tar_data = std::fs::read(&tar_path).unwrap();
        let cursor = std::io::Cursor::new(tar_data);
        let mut archive = tar::Archive::new(cursor);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().to_string()))
            .collect();
        // With --ignore-filters, tests/ should be included (VCS is still skipped)
        assert!(names.iter().any(|n| n.starts_with("tests")));
    }

    #[test]
    fn test_archive_invalid_format() {
        use crate::commands::Cli;
        use crate::commands::archive::{ArchiveArgs, execute};

        let src = tempdir().unwrap();
        let out = tempdir().unwrap();

        std::fs::write(
            src.path().join("composer.json"),
            r#"{"name": "test/project", "require": {}}"#,
        )
        .unwrap();

        let args = ArchiveArgs {
            package: None,
            version: None,
            format: Some("rar".to_string()),
            dir: Some(out.path().to_string_lossy().to_string()),
            file: None,
            ignore_filters: false,
        };

        let cli = Cli {
            command: crate::commands::Commands::Archive(ArchiveArgs {
                package: None,
                version: None,
                format: Some("rar".to_string()),
                dir: Some(out.path().to_string_lossy().to_string()),
                file: None,
                ignore_filters: false,
            }),
            verbose: 0,
            profile: false,
            no_plugins: false,
            no_scripts: false,
            working_dir: Some(src.path().to_string_lossy().to_string()),
            no_cache: false,
            no_interaction: false,
            quiet: false,
            ansi: false,
            no_ansi: false,
        };

        let console = mozart_core::console::Console {
            interactive: false,
            verbosity: mozart_core::console::Verbosity::Normal,
            decorated: false,
        };
        let result = execute(&args, &cli, &console);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("rar"));
    }
}
