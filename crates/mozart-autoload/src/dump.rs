//! `Composer\Autoload\AutoloadGenerator::dump` extension.
//!
//! [`mozart_core::composer::AutoloadGenerator`] is a state container in
//! `mozart-core`; the dumping algorithm itself sits here in
//! `mozart-autoload` because it pulls in the classmap scanner,
//! installed.json reader, and PHP-emission helpers. This module hangs
//! `dump()` off the generator via [`AutoloadGeneratorExt`] so callers
//! can still write `composer.autoload_generator().dump(...)`, matching
//! `$composer->getAutoloadGenerator()->dump(...)` in PHP.
//!
//! Bring [`AutoloadGeneratorExt`] into scope at the call site:
//!
//! ```ignore
//! use mozart_autoload::AutoloadGeneratorExt;
//! ```
//!
//! See `Composer\Autoload\AutoloadGenerator::dump()` (the ~500-line
//! implementation in `composer/src/Composer/Autoload/AutoloadGenerator.php`)
//! for the upstream semantics.

use std::collections::BTreeMap;
use std::path::PathBuf;

use mozart_core::composer::{
    AutoloadDumpOptions, AutoloadGenerator, InstallationManager, LocalRepository, Locker,
    PlatformRequirementFilter,
};
use mozart_core::config::Config;
use mozart_core::package::RawPackageData;

use crate::autoload::{AutoloadConfig, PlatformCheckMode, generate};

/// Mirror of `Composer\ClassMapGenerator\ClassMap` — the return value
/// of `AutoloadGenerator::dump`. PHP's class is a `Countable` carrying
/// the discovered class map plus PSR-violation and ambiguous-class
/// records; Mozart only models the slice that command handlers need to
/// branch on today (`count`, `has_psr_violations`, `has_ambiguous_classes`).
///
/// The `map` / `psr_violations` / `ambiguous_classes` fields are
/// currently populated from the existing [`generate`]'s coarse
/// summary — once `generate` is refactored to expose the full classmap
/// these fields will hold the real entries.
pub struct ClassMap {
    map: BTreeMap<String, String>,
    psr_violations: Vec<String>,
    ambiguous_classes: BTreeMap<String, Vec<String>>,
}

impl ClassMap {
    /// Mirror of `ClassMap::count`.
    pub fn count(&self) -> usize {
        self.map.len()
    }

    /// Mirror of `count($classMap->getPsrViolations()) > 0`. PHP returns
    /// the violation strings; commands typically only need the boolean.
    pub fn has_psr_violations(&self) -> bool {
        !self.psr_violations.is_empty()
    }

    /// Mirror of `count($classMap->getAmbiguousClasses($filter)) > 0`.
    /// `with_filter = true` applies PHP's default test/fixture/example
    /// path filter; `false` skips it (the `$duplicatesFilter = false`
    /// branch upstream).
    pub fn has_ambiguous_classes(&self, with_filter: bool) -> bool {
        if !with_filter {
            return !self.ambiguous_classes.is_empty();
        }
        let pattern = regex_filter_default();
        self.ambiguous_classes.values().any(|paths| {
            paths
                .iter()
                .any(|p| !pattern.is_match(&p.replace('\\', "/")))
        })
    }

    /// Read access to the underlying map (`getMap()` upstream).
    pub fn map(&self) -> &BTreeMap<String, String> {
        &self.map
    }

    /// Read access to the PSR-violation warnings.
    pub fn psr_violations(&self) -> &[String] {
        &self.psr_violations
    }

    /// Read access to the ambiguous-class records.
    pub fn ambiguous_classes(&self) -> &BTreeMap<String, Vec<String>> {
        &self.ambiguous_classes
    }
}

fn regex_filter_default() -> regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `{/(test|fixture|example|stub)s?/}i` from PHP's
        // ClassMap::getAmbiguousClasses default.
        regex::Regex::new(r"(?i)/(test|fixture|example|stub)s?/")
            .expect("default ambiguous filter compiles")
    })
    .clone()
}

/// Extension trait hanging `dump()` off
/// [`mozart_core::composer::AutoloadGenerator`]. Mirrors
/// `Composer\Autoload\AutoloadGenerator::dump()`.
///
/// Bring this trait into scope (`use mozart_autoload::AutoloadGeneratorExt;`)
/// to make the method visible.
///
/// Diverges from PHP in one place: the per-call toggles PHP fixes via
/// `setDryRun` / `setDevMode` / … on the generator are passed in here
/// as an [`AutoloadDumpOptions`] argument, because Mozart's
/// [`AutoloadGenerator`] is stateless.
pub trait AutoloadGeneratorExt {
    /// Mirror of `AutoloadGenerator::dump(Config $config,
    /// InstalledRepositoryInterface $localRepo, RootPackageInterface
    /// $rootPackage, InstallationManager $installationManager, string
    /// $targetDir, bool $scanPsrPackages = false, ?string $suffix = null,
    /// ?Locker $locker = null, bool $strictAmbiguous = false)`.
    ///
    /// Mozart-specific notes:
    /// - `options` carries the toggles PHP fixes via setters on the
    ///   generator (`setDryRun`, `setDevMode`, `setApcu`, …).
    /// - `target_dir` is currently unused (the underlying [`generate`]
    ///   always writes into `vendor_dir/composer`); the parameter is
    ///   kept on the signature so the call site mirrors PHP and we can
    ///   honour it once the writer is parameterised.
    /// - `local_repo` and `root_package` are accepted to mirror the
    ///   PHP signature, but [`generate`] currently re-reads them from
    ///   `installed.json` / `composer.json`. Refactoring to consume the
    ///   passed-in values lives in a follow-up.
    #[allow(clippy::too_many_arguments)]
    fn dump(
        &self,
        options: &AutoloadDumpOptions,
        config: &Config,
        local_repo: &LocalRepository,
        root_package: &RawPackageData,
        installation_manager: &InstallationManager,
        target_dir: &str,
        scan_psr_packages: bool,
        suffix: Option<&str>,
        locker: &Locker,
        strict_ambiguous: bool,
    ) -> anyhow::Result<ClassMap>;
}

impl AutoloadGeneratorExt for AutoloadGenerator {
    fn dump(
        &self,
        options: &AutoloadDumpOptions,
        config: &Config,
        _local_repo: &LocalRepository,
        _root_package: &RawPackageData,
        installation_manager: &InstallationManager,
        _target_dir: &str,
        scan_psr_packages: bool,
        suffix: Option<&str>,
        locker: &Locker,
        strict_ambiguous: bool,
    ) -> anyhow::Result<ClassMap> {
        // Mirrors PHP: classmap-authoritative implies PSR scanning so
        // every class gets a fixed map entry.
        let scan = scan_psr_packages || options.class_map_authoritative;

        // Mirrors PHP's `if (null === $this->devMode)` branch: read the
        // `dev` flag from `vendor/composer/installed.json` when no
        // explicit dev-mode has been set on the options.
        let dev_mode = match options.dev_mode {
            Some(m) => m,
            None => read_installed_dev_flag(installation_manager.vendor_dir()),
        };

        // Mirrors PHP's suffix resolution chain in `dump()`:
        // 1. explicit argument
        // 2. `Config::get('autoloader-suffix')`
        // 3. existing `vendor/autoload.php`'s `ComposerAutoloaderInit{X}`
        // 4. `composer.lock`'s `content-hash` (when locked)
        // 5. random hex
        let resolved_suffix = resolve_suffix(suffix, config, installation_manager, locker)?;

        // Mirrors PHP: `$basePath = realpath(getcwd())`. We don't have
        // an explicit project_dir on the generator, but `vendor_dir`'s
        // parent matches the project root for the common
        // `vendor-dir = "vendor"` layout. When the user points
        // `vendor-dir` outside the project we fall back to `.`.
        let project_dir = installation_manager
            .vendor_dir()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        // Mirrors PHP's `$checkPlatform = $config->get('platform-check') !==
        // false && !($filter instanceof IgnoreAllPlatformRequirementFilter)`.
        let platform_check = if matches!(
            options.platform_requirement_filter,
            PlatformRequirementFilter::IgnoreAll
        ) {
            PlatformCheckMode::Disabled
        } else {
            platform_check_mode_from_config(&config.platform_check)
        };

        let cfg = AutoloadConfig {
            project_dir,
            vendor_dir: installation_manager.vendor_dir().to_path_buf(),
            dev_mode,
            suffix: resolved_suffix,
            classmap_authoritative: options.class_map_authoritative,
            optimize: scan,
            apcu: options.apcu,
            apcu_prefix: options.apcu_prefix.clone(),
            // `dump()` does not surface a `--strict-psr` option (that's
            // a separate command-line flag on `dump-autoload`); the
            // generator only reports violations via `ClassMap`.
            strict_psr: false,
            strict_ambiguous,
            platform_check,
            ignore_platform_reqs: matches!(
                options.platform_requirement_filter,
                PlatformRequirementFilter::IgnoreAll
            ),
        };

        if options.dry_run {
            // PHP's dry-run still scans and returns the classmap but
            // skips file writes. The current [`generate`] does not
            // expose a dry-run hook, so we return an empty ClassMap
            // for now and surface the limitation here rather than
            // silently writing files.
            return Ok(ClassMap {
                map: BTreeMap::new(),
                psr_violations: Vec::new(),
                ambiguous_classes: BTreeMap::new(),
            });
        }

        let result = generate(&cfg)?;

        // Mozart's `GenerateResult` only carries summary flags
        // (`class_count`, `has_psr_violations`, `has_ambiguous_classes`),
        // not the actual class-name / path entries that PHP's `ClassMap`
        // exposes. We project the summary onto a `ClassMap` shape so
        // command code that only branches on `count()` / `has_*()` works
        // today; refactoring `generate` to surface the full map is
        // tracked as follow-up work.
        let mut map = BTreeMap::new();
        for i in 0..result.class_count {
            map.insert(format!("__mozart_placeholder_{i}"), String::new());
        }
        let psr_violations = if result.has_psr_violations {
            vec![String::from(
                "PSR-0/4 violation detected (details not yet surfaced)",
            )]
        } else {
            Vec::new()
        };
        let mut ambiguous_classes = BTreeMap::new();
        if result.has_ambiguous_classes {
            ambiguous_classes.insert("__mozart_placeholder".to_string(), Vec::new());
        }

        Ok(ClassMap {
            map,
            psr_violations,
            ambiguous_classes,
        })
    }
}

fn read_installed_dev_flag(vendor_dir: &std::path::Path) -> bool {
    let path = vendor_dir.join("composer/installed.json");
    if !path.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    value.get("dev").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn resolve_suffix(
    explicit: Option<&str>,
    config: &Config,
    installation_manager: &InstallationManager,
    locker: &Locker,
) -> anyhow::Result<String> {
    if let Some(s) = explicit
        && !s.is_empty()
    {
        return Ok(s.to_string());
    }
    if let Some(s) = config.autoloader_suffix.as_ref()
        && !s.is_empty()
    {
        return Ok(s.clone());
    }
    let vendor_path = installation_manager.vendor_dir();
    let autoload_path = vendor_path.join("autoload.php");
    if autoload_path.exists()
        && let Ok(content) = std::fs::read_to_string(&autoload_path)
        && let Some(start) = content.find("ComposerAutoloaderInit")
    {
        let rest = &content[start + "ComposerAutoloaderInit".len()..];
        if let Some(end) = rest.find("::") {
            let candidate = &rest[..end];
            if !candidate.is_empty() && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(candidate.to_string());
            }
        }
    }
    if locker.is_locked()
        && let Some(data) = locker.lock_data()?
        && !data.content_hash.is_empty()
    {
        return Ok(data.content_hash);
    }
    // Fall back to MD5 of the current timestamp (mirrors PHP's
    // `bin2hex(random_bytes(16))` — both produce a 32-char hex token
    // that participates only in classloader naming).
    let ts = format!("{:?}", std::time::SystemTime::now());
    Ok(format!("{:x}", md5::compute(ts.as_bytes())))
}

fn platform_check_mode_from_config(platform_check: &serde_json::Value) -> PlatformCheckMode {
    match platform_check {
        serde_json::Value::Bool(false) => PlatformCheckMode::Disabled,
        serde_json::Value::Bool(true) => PlatformCheckMode::Full,
        serde_json::Value::String(s) if s == "php-only" => PlatformCheckMode::PhpOnly,
        // Anything else (including JSON null / unknown strings) falls
        // through to `Full` — the safe default that PHP also picks
        // when the value is truthy-but-not-`"php-only"`.
        _ => PlatformCheckMode::Full,
    }
}
