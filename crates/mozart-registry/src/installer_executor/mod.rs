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

use crate::lockfile::LockedPackage;

pub mod filesystem;

pub use filesystem::FilesystemExecutor;

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
}

impl<'a> PackageOperation<'a> {
    pub fn package(&self) -> &'a LockedPackage {
        match self {
            PackageOperation::Install { package }
            | PackageOperation::Update { package, .. } => package,
        }
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
    fn uninstall_package(&mut self, name: &str, ctx: &ExecuteContext) -> anyhow::Result<()>;

    /// Hook called once after every uninstall has run. Default no-op.
    /// Composer cleans up empty namespace directories here; the recorder
    /// has no work to do.
    fn cleanup_after_uninstalls(&mut self, _ctx: &ExecuteContext) -> anyhow::Result<()> {
        Ok(())
    }
}
