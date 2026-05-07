use std::path::{Path, PathBuf};

use crate::{
    ArchiveFormat, collect_archivable_files, create_archive, generate_archive_filename,
    parse_composer_excludes, parse_gitattributes, parse_gitignore_pattern, self_exclusion_patterns,
};

/// A package to be archived.
///
/// Mirrors the role of Composer's `CompletePackageInterface` as input to
/// `ArchiveManager::archive()`. The `Root` variant points at an already-checked-out
/// source tree; the `Remote` variant carries dist metadata that the manager will
/// download and extract to a temporary directory.
pub enum ArchivePackage {
    Root {
        name: String,
        version: Option<String>,
        source_dir: PathBuf,
    },
    Remote {
        name: String,
        version: String,
        dist_url: String,
        dist_type: String,
        dist_shasum: Option<String>,
        dist_reference: Option<String>,
        source_reference: Option<String>,
    },
}

impl ArchivePackage {
    fn name(&self) -> &str {
        match self {
            Self::Root { name, .. } | Self::Remote { name, .. } => name,
        }
    }

    fn version(&self) -> Option<&str> {
        match self {
            Self::Root { version, .. } => version.as_deref(),
            Self::Remote { version, .. } => Some(version),
        }
    }

    fn dist_reference(&self) -> Option<&str> {
        match self {
            Self::Root { .. } => None,
            Self::Remote { dist_reference, .. } => dist_reference.as_deref(),
        }
    }

    fn dist_type(&self) -> Option<&str> {
        match self {
            Self::Root { .. } => None,
            Self::Remote { dist_type, .. } => Some(dist_type),
        }
    }

    fn source_reference(&self) -> Option<&str> {
        match self {
            Self::Root { .. } => None,
            Self::Remote {
                source_reference, ..
            } => source_reference.as_deref(),
        }
    }
}

/// Holds an extracted source directory plus, for remote packages, a tempdir
/// that must outlive `source_dir`. Drop removes the tempdir.
struct AcquiredSource {
    source_dir: PathBuf,
    archive_name: Option<String>,
    archive_excludes: Vec<String>,
    _temp_dir: Option<PathBuf>,
}

impl Drop for AcquiredSource {
    fn drop(&mut self) {
        if let Some(ref dir) = self._temp_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Read `archive.name` and `archive.exclude` from a composer.json file.
fn read_archive_config(composer_json_path: &Path) -> anyhow::Result<(Option<String>, Vec<String>)> {
    let content = std::fs::read_to_string(composer_json_path)?;
    let value: serde_json::Value = serde_json::from_str(&content)?;

    let name = value
        .get("archive")
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    let excludes = value
        .get("archive")
        .and_then(|a| a.get("exclude"))
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    Ok((name, excludes))
}

/// Manages the creation of package archives.
///
/// Mirrors Composer's `Composer\Package\Archiver\ArchiveManager`.
pub struct ArchiveManager;

impl Default for ArchiveManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveManager {
    pub fn new() -> Self {
        ArchiveManager
    }

    /// Build the parts that make up a package archive's filename.
    fn package_filename_parts(package: &ArchivePackage, archive_name: Option<&str>) -> String {
        generate_archive_filename(
            package.name(),
            archive_name,
            package.version(),
            package.dist_reference(),
            package.dist_type(),
            package.source_reference(),
        )
    }

    /// Generate the archive filename (without extension) for a package, using
    /// any `archive.name` override from the package's source composer.json.
    pub fn package_filename(package: &ArchivePackage) -> String {
        let archive_name = match package {
            ArchivePackage::Root { source_dir, .. } => {
                read_archive_config(&source_dir.join("composer.json"))
                    .ok()
                    .and_then(|(n, _)| n)
            }
            ArchivePackage::Remote { .. } => None,
        };
        Self::package_filename_parts(package, archive_name.as_deref())
    }

    /// Join filename parts with `-`, mirroring Composer's
    /// `getPackageFilenameFromParts`.
    pub fn package_filename_from_parts(parts: &[&str]) -> String {
        parts.join("-")
    }

    /// Create an archive of the given package.
    ///
    /// For a `Remote` package, the dist is downloaded into a tempdir and
    /// extracted before archiving; the tempdir is removed afterward. For
    /// `Root`, the package's `source_dir` is archived in place.
    ///
    /// Returns the absolute path to the created archive.
    pub async fn archive(
        &self,
        package: &ArchivePackage,
        format: &str,
        target_dir: &Path,
        file_name: Option<&str>,
        ignore_filters: bool,
        files_cache: &mozart_registry::cache::Cache,
    ) -> anyhow::Result<PathBuf> {
        let archive_format = ArchiveFormat::parse(format).ok_or_else(|| {
            anyhow::anyhow!(
                "Unsupported archive format \"{}\". Supported formats: tar, tar.gz, tar.bz2, zip",
                format
            )
        })?;

        let source = acquire_source(package, files_cache).await?;

        let filename_base = if let Some(file_name) = file_name {
            file_name.to_string()
        } else {
            Self::package_filename_parts(package, source.archive_name.as_deref())
        };

        // Self-exclusion: prevent the archive from including itself
        let has_extra_parts = file_name.is_none()
            && (package.version().is_some()
                || package.dist_reference().is_some()
                || package.source_reference().is_some());
        let self_exclusion_strs = self_exclusion_patterns(&filename_base, has_extra_parts);

        let mut all_patterns = Vec::new();
        for rule in &self_exclusion_strs {
            if let Some(p) = parse_gitignore_pattern(rule) {
                all_patterns.push(p);
            }
        }

        if !ignore_filters {
            let git_patterns = parse_gitattributes(&source.source_dir);
            all_patterns.extend(git_patterns);

            let composer_patterns = parse_composer_excludes(&source.archive_excludes);
            all_patterns.extend(composer_patterns);
        }

        let files = collect_archivable_files(&source.source_dir, &all_patterns)?;

        std::fs::create_dir_all(target_dir)?;
        let target_dir = target_dir
            .canonicalize()
            .unwrap_or_else(|_| target_dir.to_path_buf());
        let target = target_dir.join(format!("{}.{}", filename_base, archive_format.extension()));
        create_archive(&source.source_dir, &files, &target, &archive_format)?;

        Ok(target)
    }
}

/// Acquire the source tree of a package — either by reusing the root
/// directory or by downloading and extracting the dist into a tempdir.
/// Also reads `archive.name` / `archive.exclude` from the package's
/// composer.json.
async fn acquire_source(
    package: &ArchivePackage,
    files_cache: &mozart_registry::cache::Cache,
) -> anyhow::Result<AcquiredSource> {
    match package {
        ArchivePackage::Root { source_dir, .. } => {
            let composer_json_path = source_dir.join("composer.json");
            let (archive_name, archive_excludes) = if composer_json_path.exists() {
                read_archive_config(&composer_json_path).unwrap_or((None, vec![]))
            } else {
                (None, vec![])
            };
            Ok(AcquiredSource {
                source_dir: source_dir.clone(),
                archive_name,
                archive_excludes,
                _temp_dir: None,
            })
        }
        ArchivePackage::Remote {
            dist_url,
            dist_type,
            dist_shasum,
            ..
        } => {
            let temp_base = std::env::temp_dir();
            let unique = format!(
                "mozart-archive-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let temp_dir = temp_base.join(&unique);
            std::fs::create_dir_all(&temp_dir)?;

            let bytes = mozart_registry::downloader::download_dist(
                dist_url,
                dist_shasum.as_deref(),
                None,
                files_cache,
            )
            .await?;

            match dist_type.as_str() {
                "zip" => mozart_registry::downloader::extract_zip(&bytes, &temp_dir)?,
                "tar" | "tar.gz" | "tgz" => {
                    mozart_registry::downloader::extract_tar_gz(&bytes, &temp_dir)?
                }
                other => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    anyhow::bail!("Unsupported dist type: {}", other);
                }
            }

            let extracted_composer = temp_dir.join("composer.json");
            let (archive_name, archive_excludes) = if extracted_composer.exists() {
                read_archive_config(&extracted_composer).unwrap_or((None, vec![]))
            } else {
                (None, vec![])
            };

            Ok(AcquiredSource {
                source_dir: temp_dir.clone(),
                archive_name,
                archive_excludes,
                _temp_dir: Some(temp_dir),
            })
        }
    }
}
