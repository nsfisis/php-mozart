//! Composer-equivalent root state: composer.json + effective config +
//! the manager objects commands look up off the root [`Composer`].
//!
//! Mirrors the role of `Composer\Composer` / `Composer\PartialComposer`
//! (PHP) — a state container with getters for the merged [`Config`], the
//! root [`RawPackageData`], the [`RepositoryManager`], and the
//! [`InstallationManager`]. Wiring lives in [`crate::factory`], the same
//! split as upstream's `Composer\Factory::createComposer`.
//!
//! See `Composer\Command\BaseCommand::requireComposer()` /
//! `Composer\Command\BaseCommand::tryComposer()` for the upstream contract
//! that [`Composer::require`] and [`Composer::try_load`] are modelled on.

use std::path::{Path, PathBuf};

use crate::factory::create_composer;
use mozart_core::composer::{AutoloadGenerator, InstallationManager, Locker, RepositoryManager};
use mozart_core::config::Config;
use mozart_core::package::RawPackageData;
use mozart_registry::download_manager::DownloadManager;

/// Project-level Composer state. Mirrors `Composer\PartialComposer` /
/// `Composer\Composer` in PHP, exposing the subset of getters command
/// handlers need today: config, root package, repository manager,
/// installation manager, autoload generator, and locker. More
/// managers (download, …) can be layered on as commands need them.
pub struct Composer {
    project_dir: PathBuf,
    config: Config,
    package: RawPackageData,
    repository_manager: RepositoryManager,
    installation_manager: InstallationManager,
    download_manager: DownloadManager,
    autoload_generator: AutoloadGenerator,
    locker: Locker,
}

impl Composer {
    /// All-args constructor used by [`crate::factory::create_composer`].
    /// Mirrors the PHP pattern of `new Composer()` followed by
    /// `setConfig` / `setPackage` / `setRepositoryManager` /
    /// `setInstallationManager` / `setAutoloadGenerator` / `setLocker`,
    /// collapsed into a single immutable build.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_dir: PathBuf,
        config: Config,
        package: RawPackageData,
        repository_manager: RepositoryManager,
        installation_manager: InstallationManager,
        download_manager: DownloadManager,
        autoload_generator: AutoloadGenerator,
        locker: Locker,
    ) -> Self {
        Self {
            project_dir,
            config,
            package,
            repository_manager,
            installation_manager,
            download_manager,
            autoload_generator,
            locker,
        }
    }

    /// Load Composer state for `project_dir`, requiring a composer.json.
    /// Mirrors `BaseCommand::requireComposer()`, which delegates to
    /// `Factory::createComposer` after asserting the file exists.
    pub fn require(project_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let project_dir = project_dir.into();
        let composer_json = project_dir.join("composer.json");
        if !composer_json.exists() {
            anyhow::bail!(
                "Composer could not find a composer.json file in {}",
                project_dir.display()
            );
        }
        create_composer(project_dir, &composer_json)
    }

    /// Load Composer state for `project_dir`, returning `None` if no
    /// composer.json exists. Other I/O or parse errors still propagate.
    /// Mirrors `BaseCommand::tryComposer()`.
    pub fn try_load(project_dir: impl Into<PathBuf>) -> anyhow::Result<Option<Self>> {
        let project_dir = project_dir.into();
        let composer_json = project_dir.join("composer.json");
        if !composer_json.exists() {
            return Ok(None);
        }
        create_composer(project_dir, &composer_json).map(Some)
    }

    /// Load Composer state keyed on a specific `composer.json` file, deriving
    /// the project directory from `file.parent()`. Mirrors
    /// `ValidateCommand::createComposerInstance($file)` — Composer keys
    /// instances on a file rather than a directory for non-default paths.
    pub fn try_load_from_file(file: &Path) -> anyhow::Result<Option<Self>> {
        let project_dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::try_load(project_dir)
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Root package loaded from the project's `composer.json`. Mirrors
    /// `Composer::getPackage()`; ideally this would return a fully
    /// resolved `RootPackageInterface` equivalent, but Mozart does not
    /// yet have a `RootPackageLoader` port — for now callers see the
    /// raw, pre-normalised JSON shape.
    pub fn package(&self) -> &RawPackageData {
        &self.package
    }

    /// Mirror of `Composer::getRepositoryManager()`.
    pub fn repository_manager(&self) -> &RepositoryManager {
        &self.repository_manager
    }

    /// Mirror of `Composer::getInstallationManager()`.
    pub fn installation_manager(&self) -> &InstallationManager {
        &self.installation_manager
    }

    pub fn download_manager(&self) -> &DownloadManager {
        &self.download_manager
    }

    /// Mirror of `Composer::getAutoloadGenerator()`.
    ///
    /// Returned by shared reference because Mozart's
    /// [`AutoloadGenerator`] is stateless — per-call toggles live on
    /// [`AutoloadDumpOptions`] passed into `dump()`, not on the
    /// generator itself. Diverges from PHP's
    /// `$composer->getAutoloadGenerator()->setDryRun(...)` chain.
    pub fn autoload_generator(&self) -> &AutoloadGenerator {
        &self.autoload_generator
    }

    /// Mirror of `Composer::getLocker()`.
    pub fn locker(&self) -> &Locker {
        &self.locker
    }
}
