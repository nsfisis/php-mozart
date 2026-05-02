//! Installation execution abstraction.
//!
//! Mirrors `Composer\Installer\InstallationManager`: the per-operation
//! side-effect surface (download, extract, remove from vendor/) lives behind
//! a trait so test code can substitute a recording-only implementation
//! (Composer's `InstallationManagerMock`) without going anywhere near the
//! filesystem or the network.
//!
//! The orchestration loop (computing operations from lock vs installed,
//! emitting console messages, writing `installed.json`, generating the
//! autoloader) stays in the caller. The executor is purely the verb —
//! "install this package" / "uninstall this package" — so test traces match
//! Composer's `(string) $operation` byte-for-byte without the executor
//! having to also reproduce console formatting.

use std::path::PathBuf;

use crate::lockfile::{LockAlias, LockedPackage};

pub mod filesystem;
pub mod trace_recorder;

pub use filesystem::FilesystemExecutor;
pub use trace_recorder::TraceRecorderExecutor;

/// One install or update operation handed to [`InstallerExecutor::install_package`].
#[derive(Debug, Clone, Copy)]
pub enum PackageOperation<'a> {
    /// First-time install. The whole package directory is created from
    /// `package.dist`/`package.source`.
    Install { package: &'a LockedPackage },
    /// Replace an existing install with a new version. `from_version` is the
    /// pretty version that was installed before.
    Update {
        from_version: &'a str,
        package: &'a LockedPackage,
    },
    /// Mark an alias of a real package as installed. No filesystem effects —
    /// only the trace recorder needs this. Mirrors Composer's
    /// `MarkAliasInstalledOperation`.
    MarkAliasInstalled {
        /// The alias entry from `composer.lock`'s `aliases[]` block. Carries
        /// pretty + normalized alias version and the target's pretty version.
        alias: &'a LockAlias,
        /// The target package the alias points at — used to source the
        /// reference suffix for the trace line.
        target: &'a LockedPackage,
    },
}

impl<'a> PackageOperation<'a> {
    pub fn package(&self) -> Option<&'a LockedPackage> {
        match self {
            PackageOperation::Install { package } | PackageOperation::Update { package, .. } => {
                Some(package)
            }
            PackageOperation::MarkAliasInstalled { .. } => None,
        }
    }
}

/// Mirror Composer's `BasePackage::getFullPrettyVersion()` for a `LockedPackage`.
///
/// For dev-stability versions backed by a git/hg source, append the reference
/// (truncated to 7 chars when it looks like a 40-char sha1). Otherwise return
/// the pretty version unchanged.
pub fn format_full_pretty_version(pkg: &LockedPackage) -> String {
    format_full_pretty_with_pretty(&pkg.version, pkg)
}

/// Same as [`format_full_pretty_version`] but lets the caller supply an
/// alternate pretty version (used by `MarkAliasInstalled` so the alias's
/// `3.2.x-dev` text is rendered with the *target's* reference).
pub fn format_full_pretty_with_pretty(pretty_version: &str, pkg: &LockedPackage) -> String {
    let is_dev = mozart_semver::Version::parse(&pkg.version)
        .map(|v| matches!(v.pre_release.as_deref(), Some("dev")) || v.is_dev_branch)
        .unwrap_or(false);
    if !is_dev {
        return pretty_version.to_string();
    }
    let source_ref = pkg.source.as_ref().and_then(|s| s.reference.as_deref());
    let dist_ref = pkg.dist.as_ref().and_then(|d| d.reference.as_deref());
    let source_type = pkg.source.as_ref().map(|s| s.source_type.as_str());
    // Composer falls back to dist reference only when no source type is set
    // (or the package isn't git/hg — in which case the dev display is skipped
    // entirely above).
    let reference = source_ref.or(match source_type {
        Some("git") | Some("hg") => None,
        _ => dist_ref,
    });
    let Some(reference) = reference else {
        return pretty_version.to_string();
    };
    if matches!(source_type, Some("git") | Some("hg")) && reference.len() == 40 {
        format!("{} {}", pretty_version, &reference[..7])
    } else if matches!(source_type, Some("svn")) {
        // svn references are revision numbers, never truncated
        format!("{} {}", pretty_version, reference)
    } else if reference.len() == 40 {
        // dist-ref fallback (no git/hg source) — Composer truncates here too
        format!("{} {}", pretty_version, &reference[..7])
    } else {
        format!("{} {}", pretty_version, reference)
    }
}

/// Per-call configuration shared across executor methods. Owned by the
/// caller (typically `install_from_lock`) so the executor sees a consistent
/// view across an entire install/update run.
#[derive(Debug, Clone)]
pub struct ExecuteContext {
    pub vendor_dir: PathBuf,
    /// Suppress download progress bars.
    pub no_progress: bool,
    /// Prefer cloning from VCS source over downloading dist archives.
    pub prefer_source: bool,
}

/// Side-effect surface for install/update/uninstall operations.
///
/// Implementations are stateful — `&mut self` lets a recorder accumulate
/// trace lines and lets the filesystem implementation hold long-lived
/// handles (caches, progress bars). All methods return `anyhow::Result` so
/// callers can short-circuit on the first failure, mirroring Composer's
/// fail-fast `InstallationManager::execute`.
#[async_trait::async_trait]
pub trait InstallerExecutor: Send + Sync {
    /// Perform side effects for one install or update operation.
    async fn install_package(
        &mut self,
        op: PackageOperation<'_>,
        ctx: &ExecuteContext,
    ) -> anyhow::Result<()>;

    /// Perform side effects for one uninstall.
    ///
    /// `version` is the previously-installed version (from installed.json),
    /// passed so the trace recorder can format Composer's
    /// `Uninstalling pkg/name (version)` line. The filesystem implementation
    /// ignores it — `name` alone is enough to locate the vendor directory.
    fn uninstall_package(
        &mut self,
        name: &str,
        version: &str,
        ctx: &ExecuteContext,
    ) -> anyhow::Result<()>;

    /// Hook called once after every uninstall has run. Default no-op.
    /// Composer cleans up empty namespace directories here; the recorder
    /// has no work to do.
    fn cleanup_after_uninstalls(&mut self, _ctx: &ExecuteContext) -> anyhow::Result<()> {
        Ok(())
    }
}
