//! Production [`InstallerExecutor`] that touches the real filesystem.
//!
//! This is the verb behind `mozart install` / `mozart update` — it pulls
//! dist archives via [`crate::downloader`], clones VCS sources via
//! [`crate::vcs`], and removes vendor directories. Test code substitutes a
//! recording-only executor instead (added in a later step).

use crate::downloader::{GitDownloader, HgDownloader, SvnDownloader};
use crate::repository::cache::Cache;
use crate::repository::downloader;
use crate::repository::installer_executor::{ExecuteContext, InstallerExecutor, PackageOperation};
use std::path::Path;

pub struct FilesystemExecutor {
    files_cache: Cache,
}

impl FilesystemExecutor {
    pub fn new(files_cache: Cache) -> Self {
        Self { files_cache }
    }
}

#[async_trait::async_trait]
impl InstallerExecutor for FilesystemExecutor {
    async fn install_package(
        &mut self,
        op: PackageOperation<'_>,
        ctx: &ExecuteContext,
    ) -> anyhow::Result<()> {
        // Marking an alias as installed/uninstalled has no filesystem side
        // effects — the target package's files are already in vendor/.
        // Mirrors Composer's `MarkAlias{,Un}installedOperation` which the
        // installation manager only uses to update the in-memory installed
        // repository.
        let Some(pkg) = op.package() else {
            return Ok(());
        };

        // Try source install if --prefer-source and source info is available.
        if ctx.prefer_source
            && let Some(source) = &pkg.source
        {
            return install_from_source(
                &source.source_type,
                &source.url,
                source.reference.as_deref().unwrap_or("HEAD"),
                &ctx.vendor_dir,
                &pkg.name,
            );
        }

        // A package with neither dist nor source has no install action.
        // This covers Composer's `type: metapackage` (modeled explicitly as
        // "no installer") and inline `type: package` definitions used in
        // test fixtures that intentionally omit download metadata. Mozart
        // records the operation and the installed.json entry but performs
        // no filesystem work, mirroring Composer's MetapackageInstaller.
        if pkg.dist.is_none() && pkg.source.is_none() {
            return Ok(());
        }

        let dist = pkg.dist.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Package {} has no dist information. Use --prefer-source to install from VCS.",
                pkg.name,
            )
        })?;

        let mut progress = downloader::DownloadProgress::new(
            !ctx.no_progress,
            format!("{} ({})", pkg.name, pkg.version),
        );

        downloader::install_package(
            &dist.url,
            &dist.dist_type,
            dist.shasum.as_deref(),
            &ctx.vendor_dir,
            &pkg.name,
            Some(&mut progress),
            &self.files_cache,
        )
        .await?;

        progress.finish();
        Ok(())
    }

    fn uninstall_package(
        &mut self,
        name: &str,
        _version: &str,
        ctx: &ExecuteContext,
    ) -> anyhow::Result<()> {
        let pkg_dir = ctx.vendor_dir.join(name);
        if pkg_dir.exists() {
            std::fs::remove_dir_all(&pkg_dir)?;
        }
        Ok(())
    }

    fn cleanup_after_uninstalls(&mut self, ctx: &ExecuteContext) -> anyhow::Result<()> {
        cleanup_empty_vendor_dirs(&ctx.vendor_dir)
    }
}

/// Remove empty vendor namespace directories left behind after package
/// removals. Skips the `composer/` and `bin/` directories. Mirrors the
/// post-uninstall cleanup Composer does in `LibraryInstaller::removeCode`.
fn cleanup_empty_vendor_dirs(vendor_dir: &Path) -> anyhow::Result<()> {
    if let Ok(entries) = std::fs::read_dir(vendor_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "composer" || name == "bin" {
                    continue;
                }
                if std::fs::read_dir(&path)?.next().is_none() {
                    std::fs::remove_dir(&path)?;
                }
            }
        }
    }
    Ok(())
}

/// Install a package from VCS source (git/svn/hg). Lifted from the previous
/// `commands/install.rs::install_from_source`. Mirrors the per-driver
/// dispatch in `Composer\Downloader\VcsDownloader::install`.
fn install_from_source(
    source_type: &str,
    url: &str,
    reference: &str,
    vendor_dir: &Path,
    package_name: &str,
) -> anyhow::Result<()> {
    use crate::downloader::VcsDownloader as _;

    let target = vendor_dir.join(package_name);
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }

    match source_type {
        "git" => {
            let process = crate::vcs::process::ProcessExecutor::new();
            let downloader = GitDownloader::new(process, vendor_dir.join(".cache").join("git"));
            downloader.download(url, reference, &target)?;
            downloader.install(url, reference, &target)?;
        }
        "svn" => {
            let process = crate::vcs::process::ProcessExecutor::new();
            let downloader = SvnDownloader::new(process);
            downloader.install(url, reference, &target)?;
        }
        "hg" => {
            let process = crate::vcs::process::ProcessExecutor::new();
            let downloader = HgDownloader::new(process);
            downloader.install(url, reference, &target)?;
        }
        _ => {
            anyhow::bail!("Unsupported source type for VCS install: {}", source_type);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_executor() -> FilesystemExecutor {
        FilesystemExecutor::new(Cache::new(std::env::temp_dir().join("__no_cache"), false))
    }

    #[test]
    fn cleanup_after_uninstalls_removes_empty_namespace_dirs() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");
        std::fs::create_dir_all(&vendor_dir).unwrap();

        let empty_ns = vendor_dir.join("old-vendor");
        std::fs::create_dir_all(&empty_ns).unwrap();

        let nonempty_ns = vendor_dir.join("psr");
        std::fs::create_dir_all(nonempty_ns.join("log")).unwrap();

        std::fs::create_dir_all(vendor_dir.join("composer")).unwrap();

        let mut exec = make_executor();
        exec.cleanup_after_uninstalls(&ExecuteContext {
            vendor_dir: vendor_dir.clone(),
            no_progress: true,
            prefer_source: false,
        })
        .unwrap();

        assert!(!empty_ns.exists());
        assert!(vendor_dir.join("psr").exists());
        assert!(vendor_dir.join("composer").exists());
    }

    #[test]
    fn cleanup_after_uninstalls_preserves_bin_dir() {
        let dir = tempdir().unwrap();
        let vendor_dir = dir.path().join("vendor");
        std::fs::create_dir_all(&vendor_dir).unwrap();

        let bin_dir = vendor_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let mut exec = make_executor();
        exec.cleanup_after_uninstalls(&ExecuteContext {
            vendor_dir: vendor_dir.clone(),
            no_progress: true,
            prefer_source: false,
        })
        .unwrap();

        assert!(bin_dir.exists());
    }
}
