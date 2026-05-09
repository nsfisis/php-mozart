use crate::composer::Composer;
use clap::Args;
use mozart_archiver::{ArchiveManager, ArchivePackage};
use mozart_core::console_writeln;
use mozart_core::factory::create_config;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

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

pub async fn execute(
    args: &ArchiveArgs,
    cli: &super::Cli,
    io: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

    let composer = Composer::try_load(&working_dir)?;
    let config = if let Some(composer) = &composer {
        Cow::Borrowed(composer.config())
    } else {
        Cow::Owned(create_config()?)
    };

    let format = args.format.as_deref().unwrap_or(&config.archive_format);
    let dir = args.dir.as_deref().unwrap_or(&config.archive_dir);

    archive(
        io,
        args.package.as_deref(),
        args.version.as_deref(),
        format,
        dir,
        args.file.as_deref(),
        args.ignore_filters,
        &working_dir,
        cli.no_cache,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn archive(
    io: &mozart_core::console::Console,
    package_name: Option<&str>,
    version: Option<&str>,
    format: &str,
    dest: &str,
    file_name: Option<&str>,
    ignore_filters: bool,
    working_dir: &Path,
    no_cache: bool,
) -> anyhow::Result<()> {
    let cache_config = mozart_registry::cache::build_cache_config(no_cache);
    let repo_cache = mozart_registry::cache::Cache::repo(&cache_config);
    let files_cache = mozart_registry::cache::Cache::files(&cache_config);

    let archive_manager = ArchiveManager::new();

    let package = if let Some(package_name) = package_name {
        select_package(io, package_name, version, &repo_cache).await?
    } else {
        load_root_package(working_dir)?
    };

    let dest_dir = if Path::new(dest).is_absolute() {
        PathBuf::from(dest)
    } else {
        working_dir.join(dest)
    };

    io.info(&format!("Creating the archive into \"{}\".", dest));
    let package_path = archive_manager
        .archive(
            &package,
            format,
            &dest_dir,
            file_name,
            ignore_filters,
            &files_cache,
        )
        .await?;

    let absolute = package_path.display().to_string();
    let short_path = package_path
        .strip_prefix(working_dir)
        .ok()
        .map(|rel| rel.display().to_string())
        .filter(|rel| rel.len() < absolute.len())
        .unwrap_or(absolute);
    console_writeln!(io, "Created: {}", short_path);

    Ok(())
}

fn load_root_package(working_dir: &Path) -> anyhow::Result<ArchivePackage> {
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }
    let root = mozart_core::package::read_from_file(&composer_json_path)?;
    let version = root
        .extra_fields
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(ArchivePackage::Root {
        name: root.name.clone(),
        version,
        source_dir: working_dir.to_path_buf(),
    })
}

async fn select_package(
    io: &mozart_core::console::Console,
    package_name: &str,
    version: Option<&str>,
    repo_cache: &mozart_registry::cache::Cache,
) -> anyhow::Result<ArchivePackage> {
    use mozart_core::package::Stability;
    use mozart_registry::version::find_best_candidate;

    io.info("Searching for the specified package.");

    // Strip @stability suffix from the version constraint (e.g. "^1.0@beta" → "^1.0", Stability::Beta)
    let (version, min_stability) = if let Some(raw) = version {
        if let Some(at_pos) = raw.find('@') {
            let ver_part = raw[..at_pos].trim().to_string();
            let stab_part = raw[at_pos + 1..].trim();
            (Some(ver_part), Stability::parse(stab_part))
        } else {
            (Some(raw.to_string()), Stability::Stable)
        }
    } else {
        (None, Stability::Stable)
    };
    let version = version.as_deref();

    let packages =
        mozart_registry::packagist::fetch_package_versions(package_name, repo_cache).await?;
    if packages.is_empty() {
        anyhow::bail!("No versions found for package \"{}\"", package_name);
    }

    let package = if let Some(version) = version {
        let matches: Vec<_> = packages
            .iter()
            .filter(|v| v.version == version || v.version_normalized.starts_with(version))
            .collect();
        if matches.is_empty() {
            anyhow::bail!(
                "Could not find version \"{}\" for package \"{}\"",
                version,
                package_name
            );
        }
        let package = matches[0];
        if matches.len() > 1 {
            io.info(&format!(
                "Found multiple matches, selected {} {}.",
                package_name, package.version
            ));
        } else {
            io.info(&format!(
                "Found an exact match {} {}.",
                package_name, package.version
            ));
        }
        package
    } else {
        let package = find_best_candidate(&packages, min_stability)
            .or_else(|| find_best_candidate(&packages, Stability::Dev))
            .ok_or_else(|| {
                anyhow::anyhow!("No suitable version found for package \"{}\"", package_name)
            })?;
        io.info(&format!(
            "Found an exact match {} {}.",
            package_name, package.version
        ));
        package
    };

    let dist = package.dist.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Package \"{}\" version \"{}\" has no dist available",
            package_name,
            package.version
        )
    })?;

    Ok(ArchivePackage::Remote {
        name: package_name.to_string(),
        version: package.version.clone(),
        dist_url: dist.url.clone(),
        dist_type: dist.dist_type.clone(),
        dist_shasum: dist.shasum.clone(),
        dist_reference: dist.reference.clone(),
        source_reference: package.source.as_ref().and_then(|s| s.reference.clone()),
    })
}
