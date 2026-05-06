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

use regex::Regex;

use crate::config::Config;
use crate::factory::create_composer;
use crate::package::RawPackageData;

/// Return the Composer home directory, respecting `COMPOSER_HOME` and falling
/// back to the platform default using Composer-compatible logic.
///
/// On Unix:
/// - If XDG is in use (any `XDG_*` env var exists, or `/etc/xdg` exists),
///   prefer `$XDG_CONFIG_HOME/composer` (or `$HOME/.config/composer`).
/// - Always include `$HOME/.composer` as a fallback candidate.
/// - Return the first candidate directory that exists on disk;
///   if none exist, return the first candidate.
pub fn composer_home() -> PathBuf {
    if let Ok(val) = std::env::var("COMPOSER_HOME")
        && !val.is_empty()
    {
        return PathBuf::from(val);
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.is_empty()
        {
            return PathBuf::from(appdata).join("Composer");
        }
        return PathBuf::from("C:/ProgramData/ComposerSetup/bin");
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));

        let mut candidates: Vec<PathBuf> = Vec::new();

        if use_xdg() {
            let xdg_config = std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home_dir.join(".config"));
            candidates.push(xdg_config.join("composer"));
        }

        candidates.push(home_dir.join(".composer"));

        // Return first candidate that exists; otherwise return the first
        candidates
            .iter()
            .find(|p| p.is_dir())
            .cloned()
            .unwrap_or_else(|| candidates.into_iter().next().unwrap())
    }
}

#[cfg(not(target_os = "windows"))]
fn use_xdg() -> bool {
    std::env::vars().any(|(k, _)| k.starts_with("XDG_"))
        || std::path::Path::new("/etc/xdg").is_dir()
}

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
    autoload_generator: AutoloadGenerator,
    locker: Locker,
}

/// Subset of `Composer\Package\PackageInterface` needed by the
/// installation manager. Today only the fields referenced by
/// `LibraryInstaller::getInstallPath` (`prettyName`, `targetDir`).
#[derive(Debug, Clone)]
pub struct LocalPackage {
    pretty_name: String,
    target_dir: Option<String>,
}

impl LocalPackage {
    pub fn new(pretty_name: String, target_dir: Option<String>) -> Self {
        Self {
            pretty_name,
            target_dir,
        }
    }

    /// Original case-preserving package name (`vendor/Name`).
    /// Mirrors `PackageInterface::getPrettyName`.
    pub fn pretty_name(&self) -> &str {
        &self.pretty_name
    }

    /// Optional sub-directory inside the install path that holds the
    /// package code. Mirrors `PackageInterface::getTargetDir`.
    pub fn target_dir(&self) -> Option<&str> {
        self.target_dir.as_deref()
    }
}

/// In-memory mirror of `Composer\Repository\InstalledFilesystemRepository`
/// (`vendor/composer/installed.json`). Carries enough information for
/// commands that walk the local install (currently: `dump-autoload`).
pub struct LocalRepository {
    packages: Vec<LocalPackage>,
}

impl LocalRepository {
    pub fn new(packages: Vec<LocalPackage>) -> Self {
        Self { packages }
    }

    /// Mirror of `WritableRepositoryInterface::getCanonicalPackages` —
    /// "at most one package of each name, with aliases unfolded". Mozart
    /// does not yet model alias packages, so this is currently a straight
    /// pass-through over the loaded packages.
    pub fn canonical_packages(&self) -> impl Iterator<Item = &LocalPackage> {
        self.packages.iter()
    }
}

/// Mirror of `Composer\Repository\RepositoryManager`. Today only the
/// local repository is wired up; remote repositories are loaded ad hoc by
/// commands and will move here as the registry layer is ported.
pub struct RepositoryManager {
    local_repository: LocalRepository,
}

impl RepositoryManager {
    pub fn new(local_repository: LocalRepository) -> Self {
        Self { local_repository }
    }

    /// Mirror of `RepositoryManager::getLocalRepository`.
    pub fn local_repository(&self) -> &LocalRepository {
        &self.local_repository
    }
}

/// Mirror of `Composer\Installer\InstallationManager`. Without an
/// installer plugin chain Mozart only supports the `LibraryInstaller`
/// behaviour (`vendor-dir/<pretty-name>(/<target-dir>)`).
pub struct InstallationManager {
    vendor_dir: PathBuf,
}

impl InstallationManager {
    pub fn new(vendor_dir: PathBuf) -> Self {
        Self { vendor_dir }
    }

    /// Resolved absolute path of the vendor directory. Not on PHP's
    /// `InstallationManager`, but the autoload generator needs it
    /// without the round-trip through `Config::get('vendor-dir')`.
    pub fn vendor_dir(&self) -> &Path {
        &self.vendor_dir
    }

    /// Mirror of `InstallationManager::getInstallPath` — the absolute
    /// path on disk where a package's code is expected to live. Returns
    /// `None` when the package has nothing on disk (metapackages); for
    /// regular library packages this matches `LibraryInstaller::getInstallPath`.
    pub fn get_install_path(&self, package: &LocalPackage) -> Option<PathBuf> {
        let mut path = self.vendor_dir.join(package.pretty_name());
        if let Some(td) = package.target_dir() {
            path = path.join(td);
        }
        Some(path)
    }
}

/// Mirror of `Composer\Autoload\AutoloadGenerator`.
///
/// PHP's class is stateful: `setDryRun`, `setDevMode`, … flip private
/// flags that `dump()` later reads. Mozart deliberately diverges here —
/// the per-call toggles live in [`AutoloadDumpOptions`] which is
/// passed into `dump()` as a parameter, and [`AutoloadGenerator`] is a
/// once-constructed handle that only holds dependencies that are
/// genuinely lifetime-shared (PHP's `EventDispatcher` / `IO` will land
/// here once they're ported). Today there are none, so the struct is
/// empty — but keeping it as a real type preserves the
/// `composer.autoload_generator().dump(...)` calling shape and gives a
/// home for those dependencies later.
pub struct AutoloadGenerator {
    // Intentionally empty. EventDispatcher / IO will move here once
    // ported; for now `dump()` (in `mozart-autoload`) reads everything
    // it needs from its arguments.
    _private: (),
}

impl AutoloadGenerator {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for AutoloadGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-invocation toggles passed to
/// `mozart_autoload::AutoloadGeneratorExt::dump`.
///
/// Diverges from PHP, where these live on `AutoloadGenerator` itself
/// and are flipped by `setDryRun` / `setDevMode` / … . In Mozart the
/// generator carries no transient state, so commands assemble an
/// [`AutoloadDumpOptions`] and hand it to `dump()` directly.
pub struct AutoloadDumpOptions {
    /// `None` mirrors PHP's `private ?bool $devMode = null` — meaning
    /// "auto-detect from `installed.json`'s `dev` flag at dump time".
    /// `Some(_)` corresponds to an explicit `setDevMode` call.
    pub dev_mode: Option<bool>,
    /// `setClassMapAuthoritative`.
    pub class_map_authoritative: bool,
    /// `setApcu` first arg.
    pub apcu: bool,
    /// `setApcu` second arg. The prefix is recorded even when `apcu`
    /// is false, matching the PHP signature.
    pub apcu_prefix: Option<String>,
    /// `setRunScripts`.
    pub run_scripts: bool,
    /// `setDryRun`.
    pub dry_run: bool,
    /// `setPlatformRequirementFilter`. Defaults to
    /// `PlatformRequirementFilterFactory::ignoreNothing()`.
    pub platform_requirement_filter: PlatformRequirementFilter,
}

impl AutoloadDumpOptions {
    /// Same defaults as PHP's `AutoloadGenerator::__construct` — every
    /// toggle off, dev-mode unset (auto-detect), filter set to
    /// `IgnoreNothing`.
    pub fn new() -> Self {
        Self {
            dev_mode: None,
            class_map_authoritative: false,
            apcu: false,
            apcu_prefix: None,
            run_scripts: false,
            dry_run: false,
            platform_requirement_filter: PlatformRequirementFilter::ignore_nothing(),
        }
    }
}

impl Default for AutoloadDumpOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Mirror of `Composer\Filter\PlatformRequirementFilter\PlatformRequirementFilterInterface`
/// and its three concrete implementations.
///
/// The autoload generator and resolver consult this when deciding
/// whether to emit / enforce a `php`, `ext-*`, `lib-*`, or
/// `composer-*` requirement. For non-platform packages every variant
/// returns `false` — matching PHP's `IgnoreListPlatformRequirementFilter`
/// short-circuiting via `PlatformRepository::isPlatformPackage`.
pub enum PlatformRequirementFilter {
    /// `IgnoreNothingPlatformRequirementFilter`. Default.
    IgnoreNothing,
    /// `IgnoreAllPlatformRequirementFilter` — every platform package is
    /// ignored.
    IgnoreAll,
    /// `IgnoreListPlatformRequirementFilter` — match against an explicit
    /// list of names (with `*` glob support). Names suffixed with `+`
    /// only suppress the upper bound, mirroring the PHP constructor.
    /// `None` for either regex means "no entries" (the corresponding
    /// list was empty), short-circuiting to no match.
    IgnoreList {
        ignore_regex: Option<Regex>,
        ignore_upper_bound_regex: Option<Regex>,
    },
}

impl PlatformRequirementFilter {
    /// Mirror of `PlatformRequirementFilterFactory::ignoreNothing`.
    pub fn ignore_nothing() -> Self {
        PlatformRequirementFilter::IgnoreNothing
    }

    /// Mirror of `PlatformRequirementFilterFactory::ignoreAll`.
    pub fn ignore_all() -> Self {
        PlatformRequirementFilter::IgnoreAll
    }

    /// Mirror of `PlatformRequirementFilterFactory::fromBoolOrList` for
    /// the list branch. `reqs` accepts entries suffixed with `+` to
    /// only ignore the upper bound (`IgnoreListPlatformRequirementFilter`'s
    /// constructor splits on the same suffix).
    pub fn from_list(reqs: &[String]) -> anyhow::Result<Self> {
        let mut ignore_all: Vec<String> = Vec::new();
        let mut ignore_upper_bound: Vec<String> = Vec::new();
        for req in reqs {
            if let Some(stripped) = req.strip_suffix('+') {
                ignore_upper_bound.push(stripped.to_string());
            } else {
                ignore_all.push(req.clone());
            }
        }
        Ok(PlatformRequirementFilter::IgnoreList {
            ignore_regex: package_names_to_regexp(&ignore_all)?,
            ignore_upper_bound_regex: package_names_to_regexp(&ignore_upper_bound)?,
        })
    }

    /// Mirror of `PlatformRequirementFilterFactory::fromBoolOrList`.
    pub fn from_bool_or_list(value: BoolOrList) -> anyhow::Result<Self> {
        match value {
            BoolOrList::Bool(true) => Ok(Self::ignore_all()),
            BoolOrList::Bool(false) => Ok(Self::ignore_nothing()),
            BoolOrList::List(list) => Self::from_list(&list),
        }
    }

    /// Mirror of `PlatformRequirementFilterInterface::isIgnored`.
    pub fn is_ignored(&self, req: &str) -> bool {
        match self {
            PlatformRequirementFilter::IgnoreNothing => false,
            PlatformRequirementFilter::IgnoreAll => is_platform_package(req),
            PlatformRequirementFilter::IgnoreList { ignore_regex, .. } => {
                is_platform_package(req) && ignore_regex.as_ref().is_some_and(|re| re.is_match(req))
            }
        }
    }

    /// Mirror of `PlatformRequirementFilterInterface::isUpperBoundIgnored`.
    pub fn is_upper_bound_ignored(&self, req: &str) -> bool {
        match self {
            PlatformRequirementFilter::IgnoreNothing => false,
            PlatformRequirementFilter::IgnoreAll => is_platform_package(req),
            PlatformRequirementFilter::IgnoreList {
                ignore_regex,
                ignore_upper_bound_regex,
            } => {
                if !is_platform_package(req) {
                    return false;
                }
                ignore_regex.as_ref().is_some_and(|re| re.is_match(req))
                    || ignore_upper_bound_regex
                        .as_ref()
                        .is_some_and(|re| re.is_match(req))
            }
        }
    }
}

/// Helper accepted by [`PlatformRequirementFilter::from_bool_or_list`]
/// — mirrors PHP's `bool|string[]` union by replacing it with a tagged
/// enum at the boundary. Commands typically have an
/// `--ignore-platform-reqs` flag (the `Bool` arm) plus an optional
/// `--ignore-platform-req <name>` list (the `List` arm), and convert at
/// the call site.
pub enum BoolOrList {
    Bool(bool),
    List(Vec<String>),
}

/// Compile a list of package names (with `*` glob support) into a
/// case-insensitive regex matching any of them. Mirrors
/// `BasePackage::packageNamesToRegexp` and its `packageNameToRegexp`
/// helper: each name is `preg_quote`'d, then `\*` becomes `.*`.
///
/// Returns `None` when `names` is empty — Rust's `regex` crate refuses
/// regexes that never match, so we model "match nothing" as the
/// absence of a compiled regex and short-circuit at the call site.
fn package_names_to_regexp(names: &[String]) -> anyhow::Result<Option<Regex>> {
    if names.is_empty() {
        return Ok(None);
    }
    let parts: Vec<String> = names
        .iter()
        .map(|n| regex::escape(n).replace("\\*", ".*"))
        .collect();
    let pattern = format!("(?i)^(?:{})$", parts.join("|"));
    Ok(Some(Regex::new(&pattern)?))
}

/// Mirror of `Composer\Repository\PlatformRepository::isPlatformPackage`
/// using the same canonical regex (`PLATFORM_PACKAGE_REGEX`).
fn is_platform_package(name: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:php(?:-64bit|-ipv6|-zts|-debug)?|hhvm|(?:ext|lib)-[a-z0-9](?:[_.-]?[a-z0-9]+)*|composer(?:-(?:plugin|runtime)-api)?)$",
        )
        .expect("PLATFORM_PACKAGE_REGEX compiles")
    });
    re.is_match(name)
}

/// Mirror of `Composer\Package\Locker`. The full PHP class is a thick
/// wrapper around `composer.lock` (lock-data dump/load, freshness
/// check, dev-package tracking, …) — Mozart's port currently just
/// holds the lockfile path and exposes the slice the autoload
/// generator needs (`isLocked()` / `getLockData()['content-hash']`).
/// The richer accessors will land as more commands are ported.
pub struct Locker {
    lock_file_path: PathBuf,
}

impl Locker {
    pub fn new(lock_file_path: PathBuf) -> Self {
        Self { lock_file_path }
    }

    /// Path to the underlying `composer.lock`. Mirrors
    /// `Locker::getJsonFile()->getPath()`.
    pub fn lock_file_path(&self) -> &Path {
        &self.lock_file_path
    }

    /// Mirror of `Locker::isLocked`. PHP additionally checks for the
    /// presence of the `packages` array in a parsed lock; for now the
    /// file-existence check is enough — every command that calls
    /// `lock_data()` afterwards will surface a parse error if the
    /// lockfile is corrupt.
    pub fn is_locked(&self) -> bool {
        self.lock_file_path.exists()
    }

    /// Mirror of `Locker::getLockData`. Returns `Ok(None)` when the
    /// lockfile is absent (PHP would throw `LogicException`; Mozart
    /// commands currently treat "no lock" as "no data" so the autoload
    /// suffix path stays simple).
    pub fn lock_data(&self) -> anyhow::Result<Option<LockData>> {
        if !self.lock_file_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&self.lock_file_path)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        let content_hash = value
            .get("content-hash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(Some(LockData { content_hash }))
    }
}

/// Subset of `composer.lock` fields the autoload generator currently
/// reads. Mirrors `Locker::getLockData()` return shape, narrowed to
/// what's load-bearing today (the `content-hash` used as the autoloader
/// suffix). More fields can be added when other ports start needing
/// them.
pub struct LockData {
    pub content_hash: String,
}

impl Composer {
    /// All-args constructor used by [`crate::factory::create_composer`].
    /// Mirrors the PHP pattern of `new Composer()` followed by
    /// `setConfig` / `setPackage` / `setRepositoryManager` /
    /// `setInstallationManager` / `setAutoloadGenerator` / `setLocker`,
    /// collapsed into a single immutable build.
    pub fn new(
        project_dir: PathBuf,
        config: Config,
        package: RawPackageData,
        repository_manager: RepositoryManager,
        installation_manager: InstallationManager,
        autoload_generator: AutoloadGenerator,
        locker: Locker,
    ) -> Self {
        Self {
            project_dir,
            config,
            package,
            repository_manager,
            installation_manager,
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
