use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct ArchiveArgs {
    /// The package name
    pub package: Option<String>,

    /// A version constraint
    pub version: Option<String>,

    /// Format of the resulting archive (tar, tar.gz, tar.bz2, zip)
    #[arg(short, long, value_parser = ["tar", "tar.gz", "tar.bz2", "zip"])]
    pub format: Option<String>,

    /// Write the archive to this directory
    #[arg(long)]
    pub dir: Option<String>,

    /// Write the archive with the given file name
    #[arg(long)]
    pub file: Option<String>,

    /// Ignore filters when saving archive
    #[arg(long)]
    pub ignore_filters: bool,
}

// ─── Archive config helpers ───────────────────────────────────────────────────

/// Read `archive.name` and `archive.exclude` from a composer.json file.
fn read_archive_config(
    composer_json_path: &std::path::Path,
) -> anyhow::Result<(Option<String>, Vec<String>)> {
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

// ─── Metadata for a resolved package ─────────────────────────────────────────

struct PackageMeta {
    source_dir: PathBuf,
    package_name: String,
    archive_name: Option<String>,
    archive_excludes: Vec<String>,
    version: Option<String>,
    dist_reference: Option<String>,
    dist_type: Option<String>,
    source_reference: Option<String>,
    /// Holds an optional temp directory that must outlive `source_dir`.
    _temp_dir: Option<PathBuf>,
}

impl Drop for PackageMeta {
    fn drop(&mut self) {
        // Clean up temporary directory used for remote packages
        if let Some(ref dir) = self._temp_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ─── Main entry point ─────────────────────────────────────────────────────────

pub async fn execute(
    args: &ArchiveArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    use mozart_archiver::{
        ArchiveFormat, collect_archivable_files, create_archive, generate_archive_filename,
        parse_composer_excludes, parse_gitattributes, parse_gitignore_pattern,
        self_exclusion_patterns,
    };

    // 1. Determine working directory
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // 2. Load config for format/dir defaults from composer.json's "config" section
    let composer_json_path = working_dir.join("composer.json");
    let (config_archive_format, config_archive_dir) = if composer_json_path.exists() {
        let content = std::fs::read_to_string(&composer_json_path)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        let fmt = value
            .get("config")
            .and_then(|c| c.get("archive-format"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let dir = value
            .get("config")
            .and_then(|c| c.get("archive-dir"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        (fmt, dir)
    } else {
        (None, None)
    };

    // 3. Determine format: args -> config -> default "tar"
    let format_str = args
        .format
        .as_deref()
        .or(config_archive_format.as_deref())
        .unwrap_or("tar");
    let format = ArchiveFormat::parse(format_str).ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported archive format \"{}\". Supported formats: tar, tar.gz, tar.bz2, zip",
            format_str
        )
    })?;

    // 4. Determine output directory: args -> config -> default "."
    let output_dir_str = args
        .dir
        .as_deref()
        .or(config_archive_dir.as_deref())
        .unwrap_or(".");
    let output_dir = if std::path::Path::new(output_dir_str).is_absolute() {
        PathBuf::from(output_dir_str)
    } else {
        working_dir.join(output_dir_str)
    };
    std::fs::create_dir_all(&output_dir)?;

    // 5. Determine source directory and package metadata
    let meta: PackageMeta = if let Some(ref pkg_name) = args.package {
        // Remote package mode
        console.info("Searching for the specified package.");
        resolve_remote_package(pkg_name, args.version.as_deref(), console).await?
    } else {
        // Root package mode
        if !composer_json_path.exists() {
            anyhow::bail!("No composer.json found in {}", working_dir.display());
        }
        let root = mozart_core::package::read_from_file(&composer_json_path)?;
        let (archive_name, archive_excludes) = read_archive_config(&composer_json_path)?;
        let version = root
            .extra_fields
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        PackageMeta {
            source_dir: working_dir.clone(),
            package_name: root.name.clone(),
            archive_name,
            archive_excludes,
            version,
            dist_reference: None,
            dist_type: None,
            source_reference: None,
            _temp_dir: None,
        }
    };

    // 6. Generate output filename
    let filename_base = if let Some(ref f) = args.file {
        f.clone()
    } else {
        generate_archive_filename(
            &meta.package_name,
            meta.archive_name.as_deref(),
            meta.version.as_deref(),
            meta.dist_reference.as_deref(),
            meta.dist_type.as_deref(),
            meta.source_reference.as_deref(),
        )
    };

    // 7. Build exclude patterns
    // Self-exclusion: prevent the archive from including itself
    let has_extra_parts = args.file.is_none()
        && (meta.version.is_some()
            || meta.dist_reference.is_some()
            || meta.source_reference.is_some());
    let self_exclusion_strs = self_exclusion_patterns(&filename_base, has_extra_parts);

    let mut all_patterns = Vec::new();

    // Self-exclusion always applies
    for rule in &self_exclusion_strs {
        if let Some(p) = parse_gitignore_pattern(rule) {
            all_patterns.push(p);
        }
    }

    if !args.ignore_filters {
        // Parse .gitattributes export-ignore rules
        let git_patterns = parse_gitattributes(&meta.source_dir);
        all_patterns.extend(git_patterns);

        // Parse composer.json archive.exclude rules
        let composer_patterns = parse_composer_excludes(&meta.archive_excludes);
        all_patterns.extend(composer_patterns);
    }

    // 8. Collect files
    let files = collect_archivable_files(&meta.source_dir, &all_patterns)?;

    // 9. Create archive
    let target_path = output_dir.join(format!("{}.{}", filename_base, format.extension()));
    console.info(&format!(
        "Creating the archive into \"{}\".",
        output_dir.display()
    ));
    create_archive(&meta.source_dir, &files, &target_path, &format)?;

    // Print relative path if possible
    let display_path = if let Ok(rel) = target_path.strip_prefix(&working_dir) {
        rel.display().to_string()
    } else {
        target_path.display().to_string()
    };
    eprint!("Created: ");
    println!("{}", display_path);

    Ok(())
}

// ─── Remote package resolution ────────────────────────────────────────────────

async fn resolve_remote_package(
    package_name: &str,
    version_constraint: Option<&str>,
    console: &mozart_core::console::Console,
) -> anyhow::Result<PackageMeta> {
    use mozart_core::package::Stability;
    use mozart_registry::version::find_best_candidate;

    // Parse @stability suffix from version constraint (e.g. "^1.0@beta" → "^1.0", Stability::Beta)
    let (constraint_stripped, stability) = if let Some(raw) = version_constraint {
        if let Some(at_pos) = raw.find('@') {
            let ver_part = raw[..at_pos].trim();
            let stab_part = raw[at_pos + 1..].trim();
            let stab = Stability::parse(stab_part);
            (Some(ver_part.to_string()), stab)
        } else {
            (Some(raw.to_string()), Stability::Stable)
        }
    } else {
        (None, Stability::Stable)
    };
    let version_constraint = constraint_stripped.as_deref();

    // Fetch versions from Packagist
    let versions = mozart_registry::packagist::fetch_package_versions(package_name, None).await?;
    if versions.is_empty() {
        anyhow::bail!("No versions found for package \"{}\"", package_name);
    }

    // Apply version constraint filtering if given
    let candidate = if let Some(constraint) = version_constraint {
        let matches: Vec<_> = versions
            .iter()
            .filter(|v| v.version == constraint || v.version_normalized.starts_with(constraint))
            .collect();
        if matches.is_empty() {
            anyhow::bail!(
                "Could not find version \"{}\" for package \"{}\"",
                constraint,
                package_name
            );
        }
        let best = matches[0];
        if matches.len() > 1 {
            console.info(&format!(
                "Found multiple matches, selected {} {}.",
                package_name, best.version
            ));
        } else {
            console.info(&format!(
                "Found an exact match {} {}.",
                package_name, best.version
            ));
        }
        best
    } else {
        let best = find_best_candidate(&versions, stability)
            .or_else(|| find_best_candidate(&versions, Stability::Dev))
            .ok_or_else(|| {
                anyhow::anyhow!("No suitable version found for package \"{}\"", package_name)
            })?;
        console.info(&format!(
            "Found an exact match {} {}.",
            package_name, best.version
        ));
        best
    };

    let dist = candidate.dist.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Package \"{}\" version \"{}\" has no dist available",
            package_name,
            candidate.version
        )
    })?;

    // Create a temp directory using std (not tempfile crate, which is dev-only)
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

    let bytes =
        mozart_registry::downloader::download_dist(&dist.url, dist.shasum.as_deref(), None, None)
            .await?;

    match dist.dist_type.as_str() {
        "zip" => mozart_registry::downloader::extract_zip(&bytes, &temp_dir)?,
        "tar" | "tar.gz" | "tgz" => mozart_registry::downloader::extract_tar_gz(&bytes, &temp_dir)?,
        other => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            anyhow::bail!("Unsupported dist type: {}", other);
        }
    }

    // Try to read composer.json from the extracted source for archive.name / archive.exclude
    let extracted_composer = temp_dir.join("composer.json");
    let (archive_name, archive_excludes) = if extracted_composer.exists() {
        read_archive_config(&extracted_composer).unwrap_or((None, vec![]))
    } else {
        (None, vec![])
    };

    let version: Option<String> = Some(candidate.version.clone());
    let dist_reference: Option<String> = dist.reference.clone();
    let dist_type: Option<String> = Some(dist.dist_type.clone());
    let source_reference: Option<String> =
        candidate.source.as_ref().and_then(|s| s.reference.clone());

    Ok(PackageMeta {
        source_dir: temp_dir.clone(),
        package_name: package_name.to_string(),
        archive_name,
        archive_excludes,
        version,
        dist_reference,
        dist_type,
        source_reference,
        _temp_dir: Some(temp_dir),
    })
}
