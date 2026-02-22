//! Dependency resolver using the pubgrub v0.3.0 algorithm.
//!
//! This module converts Composer-style dependency constraints into pubgrub's `Ranges<ComposerVersion>`
//! and implements `DependencyProvider` for Mozart's package resolution.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

use pubgrub::{
    DefaultStringReporter, Dependencies, DependencyConstraints, DependencyProvider,
    PackageResolutionStatistics, PubGrubError, Ranges, Reporter,
};

use crate::cache::Cache;
use crate::packagist;
use mozart_constraint::{Constraint, VersionConstraint};
use mozart_core::package::Stability;

// ─────────────────────────────────────────────────────────────────────────────
// Stability constants
// ─────────────────────────────────────────────────────────────────────────────

const STABILITY_DEV: u16 = 0;
const STABILITY_ALPHA_BASE: u16 = 1000;
const STABILITY_BETA_BASE: u16 = 2000;
const STABILITY_RC_BASE: u16 = 3000;
const STABILITY_STABLE: u16 = 4000;
const STABILITY_PATCH_BASE: u16 = 5000;

// ─────────────────────────────────────────────────────────────────────────────
// ComposerVersion
// ─────────────────────────────────────────────────────────────────────────────

/// A Composer version suitable for use with pubgrub.
///
/// Encodes a 4-segment Composer version plus stability into an ordered struct.
/// Stability is encoded numerically so that higher values are more stable:
/// - dev=0, alpha(N)=1000+N, beta(N)=2000+N, RC(N)=3000+N, stable=4000, patch(N)=5000+N
///
/// This ensures natural `Ord` comparison matches Composer's version ordering.
/// Dev branches (dev-master, dev-*) are NOT representable and return `None` from `from_normalized`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComposerVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub build: u16,
    /// Stability encoded as a comparable integer. Higher = more stable.
    pub stability: u16,
}

impl PartialOrd for ComposerVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ComposerVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (
            self.major,
            self.minor,
            self.patch,
            self.build,
            self.stability,
        )
            .cmp(&(
                other.major,
                other.minor,
                other.patch,
                other.build,
                other.stability,
            ))
    }
}

impl fmt::Display for ComposerVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.patch, self.build
        )?;
        let s = self.stability;
        if s == STABILITY_STABLE {
            // no suffix
        } else if s >= STABILITY_PATCH_BASE {
            write!(f, "-patch{}", s - STABILITY_PATCH_BASE)?;
        } else if s >= STABILITY_RC_BASE {
            write!(f, "-RC{}", s - STABILITY_RC_BASE)?;
        } else if s >= STABILITY_BETA_BASE {
            write!(f, "-beta{}", s - STABILITY_BETA_BASE)?;
        } else if s >= STABILITY_ALPHA_BASE {
            write!(f, "-alpha{}", s - STABILITY_ALPHA_BASE)?;
        } else {
            write!(f, "-dev")?;
        }
        Ok(())
    }
}

impl ComposerVersion {
    /// Parse a branch alias target like "2.x-dev" or "1.0.x-dev" into a ComposerVersion
    /// with dev stability.
    ///
    /// Used to represent aliased dev branches in the resolver. The version number is taken
    /// from the numeric prefix (e.g. "2.x-dev" → major=2, minor=0, patch=0, build=0, stability=dev).
    /// This allows constraints like `^2.0` to match `dev-master` when it is aliased to `2.x-dev`.
    pub fn from_branch_alias_target(alias_target: &str) -> Option<ComposerVersion> {
        let s = alias_target.trim().to_lowercase();
        // Must end with "-dev" or ".x-dev"
        if !s.ends_with("-dev") {
            return None;
        }
        // Strip the trailing "-dev"
        let base = &s[..s.len() - 4];
        // Strip optional trailing ".x" segments (e.g. "2.x" → "2", "1.0.x" → "1.0")
        let base = base.trim_end_matches(".x");
        // Now parse whatever numeric segments remain
        let parts: Vec<&str> = base.split('.').collect();
        let major: u16 = parts.first().and_then(|p| p.parse().ok())?;
        let minor: u16 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch: u16 = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
        let build: u16 = parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);
        Some(ComposerVersion {
            major,
            minor,
            patch,
            build,
            stability: STABILITY_DEV,
        })
    }

    /// Parse from a Packagist normalized version string like "1.2.3.0", "1.0.0.0-beta1", "1.0.0.0-RC2".
    /// Returns `None` for dev branches (dev-master, dev-*, *.x-dev).
    pub fn from_normalized(normalized: &str) -> Option<ComposerVersion> {
        let s = normalized.trim();

        // Reject dev branches
        if s.to_lowercase().starts_with("dev-") {
            return None;
        }
        // Reject *.x-dev style (e.g. "9999999.9999999.9999999.9999999-dev" from packagist sometimes)
        // Also reject anything like "2.1.x-dev"
        if s.to_lowercase().ends_with("-dev") && s.contains(".x") {
            return None;
        }
        // Packagist uses 9999999.9999999.9999999.9999999 for dev branches too
        if s.starts_with("9999999") {
            return None;
        }

        // Split on '-' for pre-release
        let (version_part, pre_part) = if let Some(pos) = s.find('-') {
            (&s[..pos], Some(&s[pos + 1..]))
        } else {
            (s, None)
        };

        let segments: Vec<&str> = version_part.split('.').collect();
        if segments.is_empty() || segments[0].is_empty() {
            return None;
        }

        let major: u16 = segments[0].parse().ok()?;
        let minor: u16 = segments.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch: u16 = segments.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
        let build: u16 = segments.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);

        let stability = match pre_part {
            None => STABILITY_STABLE,
            Some(pre) => encode_pre_release_str(pre),
        };

        Some(ComposerVersion {
            major,
            minor,
            patch,
            build,
            stability,
        })
    }

    /// Construct a stable version from numeric segments.
    pub fn stable(major: u16, minor: u16, patch: u16, build: u16) -> ComposerVersion {
        ComposerVersion {
            major,
            minor,
            patch,
            build,
            stability: STABILITY_STABLE,
        }
    }

    /// Get the `Stability` enum value for this version.
    pub fn stability_enum(&self) -> Stability {
        if self.stability < STABILITY_ALPHA_BASE {
            // Covers both STABILITY_DEV (0) and any value below ALPHA_BASE
            Stability::Dev
        } else if self.stability < STABILITY_BETA_BASE {
            Stability::Alpha
        } else if self.stability < STABILITY_RC_BASE {
            Stability::Beta
        } else if self.stability < STABILITY_STABLE {
            Stability::RC
        } else {
            // >= STABILITY_STABLE (includes patch)
            Stability::Stable
        }
    }
}

fn encode_pre_release_str(pre: &str) -> u16 {
    let lower = pre.to_lowercase();
    if lower == "dev" {
        STABILITY_DEV
    } else if lower.starts_with("alpha") || lower.starts_with('a') {
        let n = extract_pre_release_number_from(
            &lower,
            if lower.starts_with("alpha") {
                "alpha"
            } else {
                "a"
            },
        );
        STABILITY_ALPHA_BASE + n
    } else if lower.starts_with("beta") || lower.starts_with('b') {
        let n = extract_pre_release_number_from(
            &lower,
            if lower.starts_with("beta") {
                "beta"
            } else {
                "b"
            },
        );
        STABILITY_BETA_BASE + n
    } else if lower.starts_with("rc") {
        let n = extract_pre_release_number_from(&lower, "rc");
        STABILITY_RC_BASE + n
    } else if lower.starts_with("patch") || lower.starts_with("pl") {
        let n = extract_pre_release_number_from(
            &lower,
            if lower.starts_with("patch") {
                "patch"
            } else {
                "pl"
            },
        );
        STABILITY_PATCH_BASE + n
    } else if lower == "p" {
        STABILITY_PATCH_BASE
    } else {
        STABILITY_STABLE
    }
}

fn extract_pre_release_number_from(s: &str, prefix: &str) -> u16 {
    let after = &s[prefix.len()..];
    let digits: String = after.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// PackageName
// ─────────────────────────────────────────────────────────────────────────────

/// A normalized package name (lowercase, e.g. "monolog/monolog").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageName(pub String);

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PackageName {
    pub const ROOT: &'static str = "__root__";

    pub fn root() -> Self {
        PackageName(Self::ROOT.to_string())
    }

    /// Returns true if this is a platform package (php, ext-*, lib-*).
    pub fn is_platform(&self) -> bool {
        self.0 == "php"
            || self.0.starts_with("ext-")
            || self.0.starts_with("lib-")
            || self.0 == "php-64bit"
            || self.0 == "php-ipv6"
            || self.0 == "php-zts"
            || self.0 == "php-debug"
    }

    /// Returns true if this is the virtual root package.
    pub fn is_root(&self) -> bool {
        self.0 == Self::ROOT
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Type alias
// ─────────────────────────────────────────────────────────────────────────────

/// The version set type used throughout the resolver.
pub type ComposerVS = Ranges<ComposerVersion>;

// ─────────────────────────────────────────────────────────────────────────────
// Constraint-to-Ranges conversion
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a Composer version constraint string to a pubgrub `Ranges<ComposerVersion>`.
///
/// Supports: exact, >=, >, <=, <, !=, ^, ~, *, wildcards, hyphen ranges, AND, OR.
pub fn constraint_to_ranges(constraint: &str) -> Result<ComposerVS, String> {
    let vc = VersionConstraint::parse(constraint)
        .map_err(|e| format!("Failed to parse constraint '{}': {}", constraint, e))?;
    version_constraint_to_ranges(&vc)
}

fn version_constraint_to_ranges(vc: &VersionConstraint) -> Result<ComposerVS, String> {
    match vc {
        VersionConstraint::Single(c) => single_constraint_to_ranges(c),
        VersionConstraint::And(cs) => {
            let mut result = Ranges::full();
            for c in cs {
                result = result.intersection(&version_constraint_to_ranges(c)?);
            }
            Ok(result)
        }
        VersionConstraint::Or(cs) => {
            let mut result = Ranges::empty();
            for c in cs {
                result = result.union(&version_constraint_to_ranges(c)?);
            }
            Ok(result)
        }
    }
}

fn single_constraint_to_ranges(c: &Constraint) -> Result<ComposerVS, String> {
    match c {
        Constraint::Any => Ok(Ranges::full()),
        Constraint::Exact(v) => {
            let cv = version_to_composer(v)?;
            Ok(Ranges::singleton(cv))
        }
        Constraint::GreaterThan(v) => {
            let cv = version_to_composer(v)?;
            Ok(Ranges::strictly_higher_than(cv))
        }
        Constraint::GreaterThanOrEqual(v) => {
            let cv = version_to_composer(v)?;
            Ok(Ranges::higher_than(cv))
        }
        Constraint::LessThan(v) => {
            let cv = version_to_composer(v)?;
            Ok(Ranges::strictly_lower_than(cv))
        }
        Constraint::LessThanOrEqual(v) => {
            let cv = version_to_composer(v)?;
            // No Ranges::lower_than in version-ranges 0.1.x, so use complement of strictly_higher_than
            Ok(Ranges::strictly_higher_than(cv).complement())
        }
        Constraint::NotEqual(v) => {
            let cv = version_to_composer(v)?;
            Ok(Ranges::singleton(cv).complement())
        }
    }
}

/// Convert a `constraint::Version` to a `ComposerVersion`.
fn version_to_composer(v: &mozart_constraint::Version) -> Result<ComposerVersion, String> {
    // Dev branches cannot be represented as ComposerVersion
    if v.is_dev_branch {
        return Err(format!(
            "Dev branch versions cannot be used in Ranges (branch: {:?})",
            v.dev_branch_name
        ));
    }

    let major: u16 = v
        .major
        .try_into()
        .map_err(|_| format!("Major version {} too large for u16", v.major))?;
    let minor: u16 = v
        .minor
        .try_into()
        .map_err(|_| format!("Minor version {} too large for u16", v.minor))?;
    let patch: u16 = v
        .patch
        .try_into()
        .map_err(|_| format!("Patch version {} too large for u16", v.patch))?;
    let build: u16 = v
        .build
        .try_into()
        .map_err(|_| format!("Build version {} too large for u16", v.build))?;

    let stability = encode_pre_release(&v.pre_release);

    Ok(ComposerVersion {
        major,
        minor,
        patch,
        build,
        stability,
    })
}

fn encode_pre_release(pre: &Option<String>) -> u16 {
    match pre {
        None => STABILITY_STABLE,
        Some(s) => encode_pre_release_str(s),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Platform configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Platform package configuration.
/// Maps package names to version strings (normalized, e.g. "8.1.0.0").
pub struct PlatformConfig {
    pub packages: HashMap<String, String>,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformConfig {
    /// Detect platform packages from the local PHP installation.
    ///
    /// Runs `php -r …` to discover the PHP version, extensions and
    /// capabilities.  Returns an empty config when PHP is not found.
    pub fn new() -> Self {
        let detected = mozart_core::platform::detect_platform();
        let mut packages = HashMap::new();
        for pkg in detected {
            // Normalize version to four-component form (e.g. "8.2.1" → "8.2.1.0")
            let normalized = normalize_platform_version(&pkg.version);
            packages.insert(pkg.name, normalized);
        }
        Self { packages }
    }

    /// Parse platform packages into `ComposerVersion` values.
    pub fn to_versions(&self) -> HashMap<String, ComposerVersion> {
        self.packages
            .iter()
            .filter_map(|(name, version_str)| {
                ComposerVersion::from_normalized(version_str).map(|v| (name.clone(), v))
            })
            .collect()
    }
}

/// Pad a version string to four dot-separated components (e.g. "8.2.1" → "8.2.1.0").
fn normalize_platform_version(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    match parts.len() {
        1 => format!("{}.0.0.0", parts[0]),
        2 => format!("{}.{}.0.0", parts[0], parts[1]),
        3 => format!("{}.{}.{}.0", parts[0], parts[1], parts[2]),
        _ => version.to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Error types
// ─────────────────────────────────────────────────────────────────────────────

/// Error returned by `DependencyProvider` methods (internal to the solver).
#[derive(Debug)]
pub enum ResolverError {
    /// Network or API error fetching package metadata.
    PackagistError(String),
    /// Internal error.
    Internal(String),
}

impl fmt::Display for ResolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackagistError(msg) => write!(f, "Packagist error: {}", msg),
            Self::Internal(msg) => write!(f, "Internal resolver error: {}", msg),
        }
    }
}

impl std::error::Error for ResolverError {}

/// Error returned by the public `resolve()` function.
#[derive(Debug)]
pub enum ResolveError {
    /// No solution exists. Contains a human-readable explanation.
    NoSolution(String),
    /// Error parsing a version constraint.
    ConstraintParseError(String, String, String), // (package, constraint, error)
    /// Error fetching dependency metadata.
    DependencyFetchError(String),
    /// Internal error.
    Internal(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSolution(report) => {
                writeln!(
                    f,
                    "Your requirements could not be resolved to an installable set of packages."
                )?;
                writeln!(f)?;
                write!(f, "{}", report)
            }
            Self::ConstraintParseError(pkg, constraint, err) => {
                write!(
                    f,
                    "Could not parse version constraint '{}' for package {}: {}",
                    constraint, pkg, err
                )
            }
            Self::DependencyFetchError(msg) => write!(f, "{}", msg),
            Self::Internal(msg) => write!(f, "Internal resolver error: {}", msg),
        }
    }
}

impl std::error::Error for ResolveError {}

// ─────────────────────────────────────────────────────────────────────────────
// Priority type
// ─────────────────────────────────────────────────────────────────────────────

/// Priority for package resolution ordering.
/// Higher priority = resolved first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolverPriority {
    conflict_count: u32,
    version_count_inverse: Reverse<usize>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider internals
// ─────────────────────────────────────────────────────────────────────────────

/// Cached version data for a single package.
struct PackageVersions {
    /// All versions that pass the stability filter, sorted by ComposerVersion.
    versions: BTreeMap<ComposerVersion, VersionDependencies>,
}

/// Dependencies of a specific package version.
struct VersionDependencies {
    /// Required packages: (package_name, constraint_string)
    require: Vec<(String, String)>,
    /// Replace declarations: (package_name, constraint_string)
    /// Stored for future replace/provide support (Phase 3.8+).
    #[allow(dead_code)]
    replace: Vec<(String, String)>,
    /// Provide declarations: (package_name, constraint_string)
    /// Stored for future replace/provide support (Phase 3.8+).
    #[allow(dead_code)]
    provide: Vec<(String, String)>,
    /// Conflict declarations: (package_name, constraint_string)
    conflict: Vec<(String, String)>,
    /// Original version string (for output).
    version_string: String,
    /// Normalized version string.
    version_normalized: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// MozartProvider
// ─────────────────────────────────────────────────────────────────────────────

/// pubgrub `DependencyProvider` that fetches package metadata from Packagist.
pub struct MozartProvider {
    /// Tokio runtime handle for calling async functions from sync trait methods.
    handle: tokio::runtime::Handle,

    /// Cache of fetched package metadata. Populated lazily from Packagist.
    package_cache: RefCell<HashMap<String, PackageVersions>>,

    /// Optional on-disk repo cache for Packagist API responses.
    repo_cache: Option<Cache>,

    /// Platform packages (php, ext-*, lib-*) with their fixed versions.
    platform_packages: HashMap<String, ComposerVersion>,

    /// Minimum stability threshold. Versions below this are excluded.
    minimum_stability: Stability,

    /// Per-package stability overrides from composer.json.
    stability_flags: HashMap<String, Stability>,

    /// Whether prefer-stable is enabled.
    prefer_stable: bool,

    /// Whether prefer-lowest is enabled (for testing).
    prefer_lowest: bool,

    /// Root package dependencies (require + optionally require-dev).
    root_dependencies: Vec<(PackageName, ComposerVS)>,

    /// Root package conflicts.
    root_conflicts: Vec<(PackageName, ComposerVS)>,

    /// Ignore all platform requirements.
    ignore_platform_reqs: bool,

    /// Specific platform requirements to ignore.
    ignore_platform_req_list: Vec<String>,
}

impl MozartProvider {
    /// Ensure package metadata is fetched from Packagist and stored in cache.
    fn ensure_fetched(&self, package_name: &str) -> Result<(), ResolverError> {
        // Check if already cached
        {
            let cache = self.package_cache.borrow();
            if cache.contains_key(package_name) {
                return Ok(());
            }
        }

        // Fetch from Packagist (with optional on-disk repo cache)
        // Uses block_on because pubgrub's DependencyProvider trait is synchronous.
        let packagist_versions = self
            .handle
            .block_on(packagist::fetch_package_versions(
                package_name,
                self.repo_cache.as_ref(),
            ))
            .map_err(|e| {
                ResolverError::PackagistError(format!("Failed to fetch {}: {}", package_name, e))
            })?;

        // Convert and filter
        let mut versions = BTreeMap::new();
        for pv in &packagist_versions {
            // Build the dependency metadata once (used for both the normal entry
            // and any branch-alias synthetic entry).
            let make_deps =
                |version_string: String, version_normalized: String| VersionDependencies {
                    require: pv
                        .require
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    replace: pv
                        .replace
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    provide: pv
                        .provide
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    conflict: pv
                        .conflict
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    version_string,
                    version_normalized,
                };

            match ComposerVersion::from_normalized(&pv.version_normalized) {
                Some(cv) => {
                    // Regular (non-dev) version
                    if self.passes_stability_filter(package_name, &cv) {
                        let deps = make_deps(pv.version.clone(), pv.version_normalized.clone());
                        versions.insert(cv, deps);
                    }
                }
                None => {
                    // Dev branch — check for branch aliases
                    let aliases = pv.branch_aliases();
                    for (branch, alias_target) in &aliases {
                        // The key in branch-alias is the full branch name, e.g. "dev-master".
                        // Verify it matches this version.
                        if branch.to_lowercase() != pv.version.to_lowercase() {
                            continue;
                        }
                        if let Some(alias_cv) =
                            ComposerVersion::from_branch_alias_target(alias_target)
                            && self.passes_stability_filter(package_name, &alias_cv)
                        {
                            // Use the alias target as the normalized version string so
                            // that constraint matching works correctly.
                            let deps = make_deps(pv.version.clone(), alias_target.clone());
                            // Only insert if no real release already occupies this slot
                            versions.entry(alias_cv).or_insert(deps);
                        }
                    }
                }
            }
        }

        let mut cache = self.package_cache.borrow_mut();
        cache.insert(package_name.to_string(), PackageVersions { versions });

        Ok(())
    }

    /// Check if a version passes the minimum-stability filter for the given package.
    fn passes_stability_filter(&self, package_name: &str, version: &ComposerVersion) -> bool {
        // Per-package stability override takes precedence
        let min_stability = self
            .stability_flags
            .get(package_name)
            .copied()
            .unwrap_or(self.minimum_stability);

        let version_stability = version.stability_enum();

        // `Stability` enum: Stable=0, RC=5, Beta=10, Alpha=15, Dev=20
        // Lower enum value = more stable.
        // version_stability must be <= min_stability (i.e., at least as stable as minimum).
        version_stability <= min_stability
    }

    /// Check whether a platform dependency should be skipped.
    fn should_skip_platform_dep(&self, dep_name: &str) -> bool {
        if !PackageName(dep_name.to_string()).is_platform() {
            return false;
        }
        if self.ignore_platform_reqs {
            return true;
        }
        self.ignore_platform_req_list.iter().any(|p| p == dep_name)
    }
}

impl DependencyProvider for MozartProvider {
    type P = PackageName;
    type V = ComposerVersion;
    type VS = ComposerVS;
    type Priority = ResolverPriority;
    type M = String;
    type Err = ResolverError;

    fn choose_version(
        &self,
        package: &PackageName,
        range: &ComposerVS,
    ) -> Result<Option<ComposerVersion>, ResolverError> {
        // Root package: always version 0.0.0.0-stable
        if package.is_root() {
            let root_v = ComposerVersion::stable(0, 0, 0, 0);
            if range.contains(&root_v) {
                return Ok(Some(root_v));
            }
            return Ok(None);
        }

        // Platform packages: return the fixed version if it satisfies the range
        if package.is_platform() {
            if let Some(v) = self.platform_packages.get(&package.0)
                && range.contains(v)
            {
                return Ok(Some(*v));
            }
            return Ok(None);
        }

        // Regular packages: ensure metadata is fetched
        self.ensure_fetched(&package.0)?;

        let cache = self.package_cache.borrow();
        let Some(pkg_versions) = cache.get(&package.0) else {
            return Ok(None);
        };

        if self.prefer_lowest {
            // Pick the lowest matching version
            return Ok(pkg_versions
                .versions
                .keys()
                .find(|v| range.contains(*v))
                .copied());
        }

        if self.prefer_stable {
            // First try: highest stable version in range
            if let Some(v) = pkg_versions
                .versions
                .keys()
                .rev()
                .find(|v| v.stability >= STABILITY_STABLE && range.contains(*v))
            {
                return Ok(Some(*v));
            }
        }

        // Default: pick highest version in range
        Ok(pkg_versions
            .versions
            .keys()
            .rev()
            .find(|v| range.contains(*v))
            .copied())
    }

    fn prioritize(
        &self,
        package: &PackageName,
        range: &ComposerVS,
        package_conflicts_counts: &PackageResolutionStatistics,
    ) -> Self::Priority {
        // Root and platform packages: highest priority (resolved first)
        if package.is_root() || package.is_platform() {
            return ResolverPriority {
                conflict_count: u32::MAX,
                version_count_inverse: Reverse(0),
            };
        }

        let cache = self.package_cache.borrow();
        let count = cache
            .get(&package.0)
            .map(|pvs| pvs.versions.keys().filter(|v| range.contains(*v)).count())
            .unwrap_or(0);

        ResolverPriority {
            conflict_count: package_conflicts_counts.conflict_count(),
            version_count_inverse: Reverse(count),
        }
    }

    fn get_dependencies(
        &self,
        package: &PackageName,
        version: &ComposerVersion,
    ) -> Result<Dependencies<PackageName, ComposerVS, String>, ResolverError> {
        // Root package: return the configured root dependencies
        if package.is_root() {
            let mut deps = DependencyConstraints::default();
            for (name, range) in &self.root_dependencies {
                deps.insert(name.clone(), range.clone());
            }
            // Apply root conflicts as complement ranges
            for (name, range) in &self.root_conflicts {
                let anti_range = range.complement();
                deps.entry(name.clone())
                    .and_modify(|existing| *existing = existing.intersection(&anti_range))
                    .or_insert(anti_range);
            }
            return Ok(Dependencies::Available(deps));
        }

        // Platform packages: no dependencies
        if package.is_platform() {
            return Ok(Dependencies::Available(DependencyConstraints::default()));
        }

        // Regular packages: fetch metadata and build dependency map
        self.ensure_fetched(&package.0)?;

        let cache = self.package_cache.borrow();
        let Some(pkg_versions) = cache.get(&package.0) else {
            return Ok(Dependencies::Unavailable(format!(
                "package {} has no available versions",
                package
            )));
        };

        let Some(version_deps) = pkg_versions.versions.get(version) else {
            return Ok(Dependencies::Unavailable(format!(
                "{} {} is not available",
                package, version
            )));
        };

        let mut deps = DependencyConstraints::default();

        // Process `require` constraints
        for (dep_name, constraint_str) in &version_deps.require {
            // Skip self-dependencies
            if dep_name == &package.0 {
                continue;
            }

            // Skip platform dependencies if configured
            if self.should_skip_platform_dep(dep_name) {
                continue;
            }

            let dep_pkg = PackageName(dep_name.clone());

            match constraint_to_ranges(constraint_str) {
                Ok(range) => {
                    deps.insert(dep_pkg, range);
                }
                Err(e) => {
                    // Unparseable constraint: mark this version as unavailable
                    return Ok(Dependencies::Unavailable(format!(
                        "cannot parse constraint '{}' for dependency {} of {} {}: {}",
                        constraint_str, dep_name, package, version, e
                    )));
                }
            }
        }

        // Process `conflict` declarations as complement ranges
        for (conflict_name, constraint_str) in &version_deps.conflict {
            if self.should_skip_platform_dep(conflict_name) {
                continue;
            }
            let conflict_pkg = PackageName(conflict_name.clone());
            if let Ok(range) = constraint_to_ranges(constraint_str) {
                let anti_range = range.complement();
                deps.entry(conflict_pkg)
                    .and_modify(|existing| *existing = existing.intersection(&anti_range))
                    .or_insert(anti_range);
            }
        }

        Ok(Dependencies::Available(deps))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API types
// ─────────────────────────────────────────────────────────────────────────────

/// Input to the resolver.
pub struct ResolveRequest {
    /// Dependencies from composer.json "require" section.
    pub require: Vec<(String, String)>,
    /// Dependencies from composer.json "require-dev" section.
    pub require_dev: Vec<(String, String)>,
    /// Whether to include require-dev in resolution.
    pub include_dev: bool,
    /// Minimum stability from composer.json.
    pub minimum_stability: Stability,
    /// Per-package stability overrides.
    pub stability_flags: HashMap<String, Stability>,
    /// Whether prefer-stable is enabled.
    pub prefer_stable: bool,
    /// Whether prefer-lowest is enabled.
    pub prefer_lowest: bool,
    /// Platform package configuration.
    pub platform: PlatformConfig,
    /// Ignore all platform requirements.
    pub ignore_platform_reqs: bool,
    /// Specific platform requirements to ignore.
    pub ignore_platform_req_list: Vec<String>,
    /// Optional on-disk repo cache for Packagist API responses.
    pub repo_cache: Option<Cache>,
}

/// A single package in the resolution output.
pub struct ResolvedPackage {
    pub name: String,
    /// Human-readable version string (e.g. "1.2.3").
    pub version: String,
    /// Normalized version string (e.g. "1.2.3.0").
    pub version_normalized: String,
    /// True if the resolved version is a dev/pre-release version.
    pub is_dev: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Public resolve() function
// ─────────────────────────────────────────────────────────────────────────────

/// Run the dependency resolver.
///
/// Returns a list of resolved packages (excluding root and platform packages),
/// or a human-readable error.
pub async fn resolve(request: &ResolveRequest) -> Result<Vec<ResolvedPackage>, ResolveError> {
    // 1. Build root dependencies (parsing is CPU-only, no async needed)
    let mut root_deps: Vec<(PackageName, ComposerVS)> = Vec::new();
    let root_conflicts: Vec<(PackageName, ComposerVS)> = Vec::new();

    let parse_dep =
        |name: &str, constraint: &str| -> Result<Option<(PackageName, ComposerVS)>, ResolveError> {
            let pkg = PackageName(name.to_string());

            // Skip platform deps if ignore_platform_reqs is set
            if pkg.is_platform()
                && (request.ignore_platform_reqs
                    || request.ignore_platform_req_list.contains(&name.to_string()))
            {
                return Ok(None);
            }

            let range = constraint_to_ranges(constraint).map_err(|e| {
                ResolveError::ConstraintParseError(name.to_string(), constraint.to_string(), e)
            })?;
            Ok(Some((pkg, range)))
        };

    for (name, constraint) in &request.require {
        if let Some(dep) = parse_dep(name, constraint)? {
            root_deps.push(dep);
        }
    }

    if request.include_dev {
        for (name, constraint) in &request.require_dev {
            if let Some(dep) = parse_dep(name, constraint)? {
                root_deps.push(dep);
            }
        }
    }

    // Capture the current tokio Handle so the provider can call async functions
    // from within pubgrub's synchronous DependencyProvider trait methods.
    let handle = tokio::runtime::Handle::current();

    // Clone data needed by spawn_blocking (which requires 'static)
    let repo_cache = request.repo_cache.clone();
    let platform_packages = request.platform.to_versions();
    let minimum_stability = request.minimum_stability;
    let stability_flags = request.stability_flags.clone();
    let prefer_stable = request.prefer_stable;
    let prefer_lowest = request.prefer_lowest;
    let ignore_platform_reqs = request.ignore_platform_reqs;
    let ignore_platform_req_list = request.ignore_platform_req_list.clone();

    // 2. Run pubgrub on a blocking thread (it is CPU-bound + uses block_on for I/O)
    tokio::task::spawn_blocking(move || {
        let provider = MozartProvider {
            handle,
            package_cache: RefCell::new(HashMap::new()),
            repo_cache,
            platform_packages,
            minimum_stability,
            stability_flags,
            prefer_stable,
            prefer_lowest,
            root_dependencies: root_deps,
            root_conflicts,
            ignore_platform_reqs,
            ignore_platform_req_list,
        };

        let root = PackageName::root();
        let root_version = ComposerVersion::stable(0, 0, 0, 0);

        match pubgrub::resolve(&provider, root, root_version) {
            Ok(solution) => {
                let mut result = Vec::new();
                for (pkg, version) in solution {
                    if pkg.is_root() || pkg.is_platform() {
                        continue;
                    }

                    let cache = provider.package_cache.borrow();
                    let (version_str, version_normalized) = if let Some(pvs) = cache.get(&pkg.0) {
                        if let Some(vd) = pvs.versions.get(&version) {
                            (vd.version_string.clone(), vd.version_normalized.clone())
                        } else {
                            (version.to_string(), version.to_string())
                        }
                    } else {
                        (version.to_string(), version.to_string())
                    };

                    result.push(ResolvedPackage {
                        name: pkg.0.clone(),
                        version: version_str,
                        version_normalized,
                        is_dev: version.stability < STABILITY_ALPHA_BASE,
                    });
                }
                Ok(result)
            }
            Err(PubGrubError::NoSolution(mut derivation_tree)) => {
                derivation_tree.collapse_no_versions();
                let report = DefaultStringReporter::report(&derivation_tree);
                Err(ResolveError::NoSolution(report))
            }
            Err(PubGrubError::ErrorRetrievingDependencies {
                package,
                version,
                source,
            }) => Err(ResolveError::DependencyFetchError(format!(
                "Error retrieving dependencies for {} {}: {}",
                package, version, source
            ))),
            Err(PubGrubError::ErrorChoosingVersion { package, source }) => {
                Err(ResolveError::DependencyFetchError(format!(
                    "Error choosing version for {}: {}",
                    package, source
                )))
            }
            Err(PubGrubError::ErrorInShouldCancel(e)) => {
                Err(ResolveError::Internal(format!("Resolver cancelled: {}", e)))
            }
        }
    })
    .await
    .unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pubgrub::{OfflineDependencyProvider, Ranges};

    fn test_handle() -> tokio::runtime::Handle {
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RT.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
            .handle()
            .clone()
    }

    // ──────────── ComposerVersion parsing ────────────

    #[test]
    fn test_composer_version_parse_stable() {
        let v = ComposerVersion::from_normalized("1.2.3.0").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.build, 0);
        assert_eq!(v.stability, STABILITY_STABLE);
    }

    #[test]
    fn test_composer_version_parse_beta() {
        let v = ComposerVersion::from_normalized("1.0.0.0-beta1").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
        assert_eq!(v.build, 0);
        assert_eq!(v.stability, STABILITY_BETA_BASE + 1);
    }

    #[test]
    fn test_composer_version_parse_rc() {
        let v = ComposerVersion::from_normalized("2.0.0.0-RC3").unwrap();
        assert_eq!(v.major, 2);
        assert_eq!(v.stability, STABILITY_RC_BASE + 3);
    }

    #[test]
    fn test_composer_version_parse_alpha() {
        let v = ComposerVersion::from_normalized("1.0.0.0-alpha2").unwrap();
        assert_eq!(v.stability, STABILITY_ALPHA_BASE + 2);
    }

    #[test]
    fn test_composer_version_parse_dev() {
        let v = ComposerVersion::from_normalized("1.0.0.0-dev").unwrap();
        assert_eq!(v.stability, STABILITY_DEV);
    }

    #[test]
    fn test_composer_version_parse_dev_branch() {
        let v = ComposerVersion::from_normalized("dev-master");
        assert!(
            v.is_none(),
            "dev-master should not parse as ComposerVersion"
        );
    }

    #[test]
    fn test_composer_version_parse_x_dev() {
        let v = ComposerVersion::from_normalized("dev-feature/foo");
        assert!(v.is_none());
    }

    #[test]
    fn test_composer_version_parse_9999999_dev() {
        // Packagist sometimes uses 9999999.9999999.9999999.9999999 for dev
        let v = ComposerVersion::from_normalized("9999999.9999999.9999999.9999999-dev");
        assert!(v.is_none());
    }

    #[test]
    fn test_composer_version_ordering_stable() {
        let v1 = ComposerVersion::from_normalized("2.0.0.0").unwrap();
        let v2 = ComposerVersion::from_normalized("1.0.0.0").unwrap();
        assert!(v1 > v2);
    }

    #[test]
    fn test_composer_version_ordering_stability() {
        let stable = ComposerVersion::from_normalized("1.0.0.0").unwrap();
        let rc = ComposerVersion::from_normalized("1.0.0.0-RC1").unwrap();
        let beta = ComposerVersion::from_normalized("1.0.0.0-beta1").unwrap();
        let alpha = ComposerVersion::from_normalized("1.0.0.0-alpha1").unwrap();
        let dev = ComposerVersion::from_normalized("1.0.0.0-dev").unwrap();
        assert!(stable > rc);
        assert!(rc > beta);
        assert!(beta > alpha);
        assert!(alpha > dev);
    }

    #[test]
    fn test_composer_version_ordering_pre_number() {
        let beta2 = ComposerVersion::from_normalized("1.0.0.0-beta2").unwrap();
        let beta1 = ComposerVersion::from_normalized("1.0.0.0-beta1").unwrap();
        assert!(beta2 > beta1);
    }

    #[test]
    fn test_composer_version_display() {
        let stable = ComposerVersion::stable(1, 2, 3, 0);
        assert_eq!(format!("{stable}"), "1.2.3.0");

        let beta1 = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_BETA_BASE + 1,
        };
        assert_eq!(format!("{beta1}"), "1.0.0.0-beta1");

        let rc2 = ComposerVersion {
            major: 2,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_RC_BASE + 2,
        };
        assert_eq!(format!("{rc2}"), "2.0.0.0-RC2");

        let dev = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_DEV,
        };
        assert_eq!(format!("{dev}"), "1.0.0.0-dev");
    }

    #[test]
    fn test_composer_version_stability_enum() {
        let stable = ComposerVersion::stable(1, 0, 0, 0);
        assert_eq!(stable.stability_enum(), Stability::Stable);

        let rc = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_RC_BASE,
        };
        assert_eq!(rc.stability_enum(), Stability::RC);

        let beta = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_BETA_BASE,
        };
        assert_eq!(beta.stability_enum(), Stability::Beta);

        let alpha = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_ALPHA_BASE,
        };
        assert_eq!(alpha.stability_enum(), Stability::Alpha);

        let dev = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_DEV,
        };
        assert_eq!(dev.stability_enum(), Stability::Dev);
    }

    // ──────────── Constraint conversion ────────────

    fn cv(major: u16, minor: u16, patch: u16, build: u16) -> ComposerVersion {
        ComposerVersion::stable(major, minor, patch, build)
    }

    fn cv_dev(major: u16, minor: u16, patch: u16, build: u16) -> ComposerVersion {
        ComposerVersion {
            major,
            minor,
            patch,
            build,
            stability: STABILITY_DEV,
        }
    }

    #[test]
    fn test_constraint_any() {
        let range = constraint_to_ranges("*").unwrap();
        assert!(range.contains(&cv(1, 2, 3, 0)));
        assert!(range.contains(&cv(0, 0, 0, 0)));
    }

    #[test]
    fn test_constraint_exact() {
        let range = constraint_to_ranges("1.2.3").unwrap();
        // Exact "1.2.3" is parsed as Version { 1, 2, 3, 0, pre_release: None } → stable
        assert!(range.contains(&cv(1, 2, 3, 0)));
        assert!(!range.contains(&cv(1, 2, 4, 0)));
        assert!(!range.contains(&cv(1, 2, 2, 0)));
    }

    #[test]
    fn test_constraint_gte() {
        let range = constraint_to_ranges(">=1.0").unwrap();
        // >=1.0 parses "1.0" as a stable version (no dev_boundary), so >= 1.0.0.0 (stable)
        assert!(range.contains(&cv(1, 0, 0, 0)));
        assert!(range.contains(&cv(2, 0, 0, 0)));
        // 0.9.0.0 should not be in range
        assert!(!range.contains(&cv(0, 9, 0, 0)));
        // 1.0.0.0-dev (stability=0) is LESS than 1.0.0.0 (stability=4000), so NOT in >=1.0
        assert!(!range.contains(&cv_dev(1, 0, 0, 0)));
    }

    #[test]
    fn test_constraint_lt() {
        let range = constraint_to_ranges("<2.0").unwrap();
        // <2.0 parses "2.0" as a stable version, so strictly < 2.0.0.0 (stable)
        // 2.0.0.0-dev (stability=0) is LESS than 2.0.0.0 (stability=4000), so IS in <2.0
        assert!(range.contains(&cv(1, 9, 9, 0)));
        assert!(range.contains(&cv_dev(2, 0, 0, 0))); // 2.0.0.0-dev < 2.0.0.0 (stable)
        // 2.0.0.0 (stable) and higher should not be in range
        assert!(!range.contains(&cv(2, 0, 0, 0)));
    }

    #[test]
    fn test_constraint_caret() {
        // ^1.2 → >=1.2.0.0-dev <2.0.0.0-dev
        let range = constraint_to_ranges("^1.2").unwrap();
        assert!(range.contains(&cv_dev(1, 2, 0, 0)));
        assert!(range.contains(&cv(1, 2, 0, 0)));
        assert!(range.contains(&cv(1, 9, 9, 0)));
        assert!(!range.contains(&cv_dev(2, 0, 0, 0)));
        assert!(!range.contains(&cv(2, 0, 0, 0)));
        // Below 1.2.0.0-dev should not match
        assert!(!range.contains(&cv(1, 1, 9, 0)));
    }

    #[test]
    fn test_constraint_caret_zero() {
        // ^0.2.3 → >=0.2.3.0-dev <0.3.0.0-dev
        let range = constraint_to_ranges("^0.2.3").unwrap();
        assert!(range.contains(&cv(0, 2, 3, 0)));
        assert!(range.contains(&cv(0, 2, 9, 0)));
        assert!(!range.contains(&cv_dev(0, 3, 0, 0)));
        assert!(!range.contains(&cv(1, 0, 0, 0)));
    }

    #[test]
    fn test_constraint_tilde() {
        // ~1.2.3 → >=1.2.3.0-dev <1.3.0.0-dev
        let range = constraint_to_ranges("~1.2.3").unwrap();
        assert!(range.contains(&cv(1, 2, 3, 0)));
        assert!(range.contains(&cv(1, 2, 9, 0)));
        assert!(!range.contains(&cv_dev(1, 3, 0, 0)));
    }

    #[test]
    fn test_constraint_wildcard() {
        // 1.2.* → >=1.2.0.0-dev <1.3.0.0-dev
        let range = constraint_to_ranges("1.2.*").unwrap();
        assert!(range.contains(&cv(1, 2, 0, 0)));
        assert!(range.contains(&cv(1, 2, 9, 0)));
        assert!(!range.contains(&cv_dev(1, 3, 0, 0)));
        assert!(!range.contains(&cv(1, 3, 0, 0)));
    }

    #[test]
    fn test_constraint_or() {
        // ^1.0 || ^2.0
        let range = constraint_to_ranges("^1.0 || ^2.0").unwrap();
        assert!(range.contains(&cv(1, 5, 0, 0)));
        assert!(range.contains(&cv(2, 3, 0, 0)));
        assert!(!range.contains(&cv(3, 0, 0, 0)));
    }

    #[test]
    fn test_constraint_and() {
        // >=1.0 <2.0: >=1.0 means >= 1.0.0.0 (stable); <2.0 means < 2.0.0.0 (stable)
        let range = constraint_to_ranges(">=1.0 <2.0").unwrap();
        // 1.0.0.0-dev < 1.0.0.0 (stable), so NOT in >=1.0
        assert!(!range.contains(&cv_dev(1, 0, 0, 0)));
        assert!(range.contains(&cv(1, 0, 0, 0)));
        assert!(range.contains(&cv(1, 9, 9, 0)));
        // 2.0.0.0-dev < 2.0.0.0 (stable), so IS in <2.0 but overall intersection with >=1.0 is yes
        assert!(range.contains(&cv_dev(2, 0, 0, 0)));
        assert!(!range.contains(&cv(2, 0, 0, 0)));
    }

    #[test]
    fn test_constraint_not_equal() {
        let range = constraint_to_ranges("!=1.5.0").unwrap();
        assert!(range.contains(&cv(1, 4, 0, 0)));
        assert!(!range.contains(&cv(1, 5, 0, 0)));
        assert!(range.contains(&cv(1, 6, 0, 0)));
    }

    #[test]
    fn test_constraint_hyphen() {
        // "1.0 - 2.0" → >=1.0.0.0 <=2.0.0.0
        let range = constraint_to_ranges("1.0 - 2.0").unwrap();
        assert!(range.contains(&cv(1, 0, 0, 0)));
        assert!(range.contains(&cv(1, 5, 0, 0)));
        assert!(range.contains(&cv(2, 0, 0, 0)));
        assert!(!range.contains(&cv(2, 1, 0, 0)));
    }

    // ──────────── Provider tests (offline) ────────────

    #[test]
    fn test_package_name_is_platform() {
        assert!(PackageName("php".to_string()).is_platform());
        assert!(PackageName("ext-json".to_string()).is_platform());
        assert!(PackageName("lib-curl".to_string()).is_platform());
        assert!(!PackageName("monolog/monolog".to_string()).is_platform());
        assert!(!PackageName("vendor/package".to_string()).is_platform());
    }

    #[test]
    fn test_package_name_is_root() {
        assert!(PackageName::root().is_root());
        assert!(!PackageName("monolog/monolog".to_string()).is_root());
    }

    #[test]
    fn test_platform_config_to_versions() {
        let config = PlatformConfig::new();
        let versions = config.to_versions();
        // If PHP is available on the system, we should have detected it
        if !config.packages.is_empty() {
            assert!(
                versions.contains_key("php"),
                "detected packages should include php"
            );
        }
    }

    // ──────────── Integration tests (offline, using OfflineDependencyProvider) ────────────

    type TestVS = Ranges<ComposerVersion>;

    fn cv_stable(major: u16, minor: u16, patch: u16) -> ComposerVersion {
        ComposerVersion::stable(major, minor, patch, 0)
    }

    /// Test simple resolution: root → foo ^1.0, foo 1.0 → bar ^2.0, bar 2.0 → (nothing)
    #[test]
    fn test_resolve_simple_offline() {
        let mut provider = OfflineDependencyProvider::<PackageName, TestVS>::new();

        let root = PackageName::root();
        let root_v = ComposerVersion::stable(0, 0, 0, 0);
        let foo = PackageName("foo/foo".to_string());
        let bar = PackageName("bar/bar".to_string());

        let foo_1_0 = cv_stable(1, 0, 0);
        let bar_2_0 = cv_stable(2, 0, 0);

        // root depends on foo ^1.0
        let foo_range = constraint_to_ranges("^1.0").unwrap();
        provider.add_dependencies(root.clone(), root_v, [(foo.clone(), foo_range)]);

        // foo 1.0 depends on bar ^2.0
        let bar_range = constraint_to_ranges("^2.0").unwrap();
        provider.add_dependencies(foo.clone(), foo_1_0, [(bar.clone(), bar_range)]);

        // bar 2.0 has no dependencies
        provider.add_dependencies(bar.clone(), bar_2_0, []);

        let solution = pubgrub::resolve(&provider, root.clone(), root_v).unwrap();

        assert_eq!(*solution.get(&foo).unwrap(), foo_1_0);
        assert_eq!(*solution.get(&bar).unwrap(), bar_2_0);
    }

    /// Test conflict detection: two packages require incompatible versions of a third.
    #[test]
    fn test_resolve_no_solution_offline() {
        let mut provider = OfflineDependencyProvider::<PackageName, TestVS>::new();

        let root = PackageName::root();
        let root_v = ComposerVersion::stable(0, 0, 0, 0);
        let foo = PackageName("foo/foo".to_string());
        let bar = PackageName("bar/bar".to_string());
        let dep = PackageName("dep/dep".to_string());

        let foo_1_0 = cv_stable(1, 0, 0);
        let bar_1_0 = cv_stable(1, 0, 0);
        let dep_1_0 = cv_stable(1, 0, 0);
        let dep_2_0 = cv_stable(2, 0, 0);

        // root depends on foo and bar
        let foo_range = Ranges::singleton(foo_1_0);
        let bar_range = Ranges::singleton(bar_1_0);
        provider.add_dependencies(
            root.clone(),
            root_v,
            [(foo.clone(), foo_range), (bar.clone(), bar_range)],
        );

        // foo 1.0 requires dep ^1.0 (excludes 2.x)
        let dep_range_1 = constraint_to_ranges("^1.0").unwrap();
        provider.add_dependencies(foo.clone(), foo_1_0, [(dep.clone(), dep_range_1)]);

        // bar 1.0 requires dep ^2.0 (excludes 1.x)
        let dep_range_2 = constraint_to_ranges("^2.0").unwrap();
        provider.add_dependencies(bar.clone(), bar_1_0, [(dep.clone(), dep_range_2)]);

        // dep has versions 1.0 and 2.0
        provider.add_dependencies(dep.clone(), dep_1_0, []);
        provider.add_dependencies(dep.clone(), dep_2_0, []);

        let result = pubgrub::resolve(&provider, root.clone(), root_v);
        assert!(result.is_err(), "Expected no solution for conflicting deps");
    }

    /// Test prefer-stable ordering: with prefer-stable, should pick stable over beta.
    #[test]
    fn test_prefer_stable() {
        let stable = ComposerVersion::stable(1, 0, 0, 0);
        let beta = ComposerVersion {
            major: 1,
            minor: 1,
            patch: 0,
            build: 0,
            stability: STABILITY_BETA_BASE + 1,
        };

        // stable should have higher stability numeric value than beta
        assert!(
            stable.stability > beta.stability,
            "stable should be > beta numerically"
        );
        // But stable is 1.0.0.0 and beta is 1.1.0.0-beta1; when prefer-stable is on,
        // we first look for stable version and pick the highest stable
        assert!(stable.stability >= STABILITY_STABLE);
        assert!(beta.stability < STABILITY_STABLE);
    }

    /// Test stability filter: alpha versions should be excluded when minimum_stability = stable.
    #[test]
    fn test_stability_filter() {
        let provider = MozartProvider {
            handle: test_handle(),
            package_cache: RefCell::new(HashMap::new()),
            platform_packages: HashMap::new(),
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: false,
            prefer_lowest: false,
            root_dependencies: vec![],
            root_conflicts: vec![],
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let stable_v = ComposerVersion::stable(1, 0, 0, 0);
        let alpha_v = ComposerVersion {
            major: 1,
            minor: 1,
            patch: 0,
            build: 0,
            stability: STABILITY_ALPHA_BASE,
        };
        let beta_v = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_BETA_BASE,
        };
        let rc_v = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_RC_BASE,
        };
        let dev_v = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_DEV,
        };

        assert!(provider.passes_stability_filter("foo/foo", &stable_v));
        assert!(!provider.passes_stability_filter("foo/foo", &alpha_v));
        assert!(!provider.passes_stability_filter("foo/foo", &beta_v));
        assert!(!provider.passes_stability_filter("foo/foo", &rc_v));
        assert!(!provider.passes_stability_filter("foo/foo", &dev_v));
    }

    #[test]
    fn test_stability_filter_beta() {
        let provider = MozartProvider {
            handle: test_handle(),
            package_cache: RefCell::new(HashMap::new()),
            platform_packages: HashMap::new(),
            minimum_stability: Stability::Beta,
            stability_flags: HashMap::new(),
            prefer_stable: false,
            prefer_lowest: false,
            root_dependencies: vec![],
            root_conflicts: vec![],
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let stable_v = ComposerVersion::stable(1, 0, 0, 0);
        let beta_v = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_BETA_BASE,
        };
        let alpha_v = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_ALPHA_BASE,
        };
        let dev_v = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_DEV,
        };

        assert!(provider.passes_stability_filter("foo/foo", &stable_v));
        assert!(provider.passes_stability_filter("foo/foo", &beta_v));
        assert!(!provider.passes_stability_filter("foo/foo", &alpha_v));
        assert!(!provider.passes_stability_filter("foo/foo", &dev_v));
    }

    #[test]
    fn test_stability_filter_dev() {
        let provider = MozartProvider {
            handle: test_handle(),
            package_cache: RefCell::new(HashMap::new()),
            platform_packages: HashMap::new(),
            minimum_stability: Stability::Dev,
            stability_flags: HashMap::new(),
            prefer_stable: false,
            prefer_lowest: false,
            root_dependencies: vec![],
            root_conflicts: vec![],
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let dev_v = ComposerVersion {
            major: 1,
            minor: 0,
            patch: 0,
            build: 0,
            stability: STABILITY_DEV,
        };
        assert!(provider.passes_stability_filter("foo/foo", &dev_v));
    }

    #[test]
    fn test_skip_platform_dep() {
        let provider = MozartProvider {
            handle: test_handle(),
            package_cache: RefCell::new(HashMap::new()),
            platform_packages: HashMap::new(),
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: false,
            prefer_lowest: false,
            root_dependencies: vec![],
            root_conflicts: vec![],
            ignore_platform_reqs: true,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        assert!(provider.should_skip_platform_dep("php"));
        assert!(provider.should_skip_platform_dep("ext-json"));
        assert!(!provider.should_skip_platform_dep("monolog/monolog"));
    }

    #[test]
    fn test_skip_specific_platform_dep() {
        let provider = MozartProvider {
            handle: test_handle(),
            package_cache: RefCell::new(HashMap::new()),
            platform_packages: HashMap::new(),
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: false,
            prefer_lowest: false,
            root_dependencies: vec![],
            root_conflicts: vec![],
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec!["ext-intl".to_string()],
            repo_cache: None,
        };

        assert!(provider.should_skip_platform_dep("ext-intl"));
        assert!(!provider.should_skip_platform_dep("ext-json"));
        assert!(!provider.should_skip_platform_dep("php"));
        assert!(!provider.should_skip_platform_dep("monolog/monolog"));
    }

    #[test]
    fn test_root_package_choose_version() {
        let provider = MozartProvider {
            handle: test_handle(),
            package_cache: RefCell::new(HashMap::new()),
            platform_packages: HashMap::new(),
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: false,
            prefer_lowest: false,
            root_dependencies: vec![],
            root_conflicts: vec![],
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let root = PackageName::root();
        let root_v = ComposerVersion::stable(0, 0, 0, 0);
        let full_range: ComposerVS = Ranges::full();
        let result = provider.choose_version(&root, &full_range).unwrap();
        assert_eq!(result, Some(root_v));
    }

    #[test]
    fn test_platform_choose_version() {
        let mut platform = HashMap::new();
        let php_v = ComposerVersion::from_normalized("8.1.0.0").unwrap();
        platform.insert("php".to_string(), php_v);

        let provider = MozartProvider {
            handle: test_handle(),
            package_cache: RefCell::new(HashMap::new()),
            platform_packages: platform,
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: false,
            prefer_lowest: false,
            root_dependencies: vec![],
            root_conflicts: vec![],
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let php = PackageName("php".to_string());
        let range = constraint_to_ranges(">=8.0").unwrap();
        let result = provider.choose_version(&php, &range).unwrap();
        assert_eq!(result, Some(php_v));

        // Range that excludes 8.1
        let too_new_range = constraint_to_ranges(">=9.0").unwrap();
        let result2 = provider.choose_version(&php, &too_new_range).unwrap();
        assert_eq!(result2, None);
    }

    /// Test constraint_to_ranges produces correct range with version containment checks.
    #[test]
    fn test_constraint_contains_version() {
        // ^3.0 should contain 3.5.1.0 but not 4.0.0.0
        let range = constraint_to_ranges("^3.0").unwrap();
        assert!(range.contains(&cv_stable(3, 5, 1)));
        assert!(!range.contains(&cv_stable(4, 0, 0)));
        assert!(!range.contains(&cv_stable(2, 9, 9)));
    }

    // ──────────── Integration test with MozartProvider (no network) ────────────

    /// Test resolve() with root dependencies using offline provider
    #[test]
    fn test_resolve_with_offline_provider_simple() {
        let mut provider = OfflineDependencyProvider::<PackageName, TestVS>::new();

        let root = PackageName::root();
        let root_v = ComposerVersion::stable(0, 0, 0, 0);
        let foo = PackageName("foo/foo".to_string());

        let foo_1_0 = cv_stable(1, 0, 0);
        let foo_1_1 = cv_stable(1, 1, 0);

        let foo_range = constraint_to_ranges("^1.0").unwrap();
        provider.add_dependencies(root.clone(), root_v, [(foo.clone(), foo_range)]);
        provider.add_dependencies(foo.clone(), foo_1_0, []);
        provider.add_dependencies(foo.clone(), foo_1_1, []);

        let solution = pubgrub::resolve(&provider, root.clone(), root_v).unwrap();

        // Should pick highest version: 1.1.0
        assert_eq!(*solution.get(&foo).unwrap(), foo_1_1);
    }

    #[test]
    fn test_resolve_or_constraint() {
        let mut provider = OfflineDependencyProvider::<PackageName, TestVS>::new();

        let root = PackageName::root();
        let root_v = ComposerVersion::stable(0, 0, 0, 0);
        let foo = PackageName("foo/foo".to_string());

        // foo has versions 1.5.0 and 2.3.0
        let foo_1_5 = cv_stable(1, 5, 0);
        let foo_2_3 = cv_stable(2, 3, 0);

        // root requires "^1.0 || ^2.0"
        let foo_range = constraint_to_ranges("^1.0 || ^2.0").unwrap();
        provider.add_dependencies(root.clone(), root_v, [(foo.clone(), foo_range)]);
        provider.add_dependencies(foo.clone(), foo_1_5, []);
        provider.add_dependencies(foo.clone(), foo_2_3, []);

        let solution = pubgrub::resolve(&provider, root.clone(), root_v).unwrap();

        // Should pick the highest matching version: 2.3.0
        let picked = *solution.get(&foo).unwrap();
        assert!(
            picked == foo_1_5 || picked == foo_2_3,
            "picked version should be one of the available versions"
        );
    }

    // ──────────── Branch alias tests ────────────

    #[test]
    fn test_from_branch_alias_target_x_dev() {
        let cv = ComposerVersion::from_branch_alias_target("2.x-dev").unwrap();
        assert_eq!(cv.major, 2);
        assert_eq!(cv.minor, 0);
        assert_eq!(cv.patch, 0);
        assert_eq!(cv.build, 0);
        assert_eq!(cv.stability, STABILITY_DEV);
    }

    #[test]
    fn test_from_branch_alias_target_minor_x_dev() {
        let cv = ComposerVersion::from_branch_alias_target("1.5.x-dev").unwrap();
        assert_eq!(cv.major, 1);
        assert_eq!(cv.minor, 5);
        assert_eq!(cv.patch, 0);
        assert_eq!(cv.stability, STABILITY_DEV);
    }

    #[test]
    fn test_from_branch_alias_target_patch_x_dev() {
        let cv = ComposerVersion::from_branch_alias_target("1.0.2.x-dev").unwrap();
        assert_eq!(cv.major, 1);
        assert_eq!(cv.minor, 0);
        assert_eq!(cv.patch, 2);
        assert_eq!(cv.stability, STABILITY_DEV);
    }

    #[test]
    fn test_from_branch_alias_target_invalid() {
        // Must end with -dev
        assert!(ComposerVersion::from_branch_alias_target("dev-master").is_none());
        assert!(ComposerVersion::from_branch_alias_target("2.0.0").is_none());
        assert!(ComposerVersion::from_branch_alias_target("").is_none());
    }

    /// Test that a branch alias entry created from "dev-master" aliased to "2.x-dev"
    /// is contained in the ^2.0 constraint range.
    #[test]
    fn test_branch_alias_in_range() {
        // "2.x-dev" alias target → ComposerVersion { major: 2, stability: STABILITY_DEV }
        let aliased_cv = ComposerVersion::from_branch_alias_target("2.x-dev").unwrap();
        // ^2.0 → >=2.0.0.0-dev <3.0.0.0-dev
        let range = constraint_to_ranges("^2.0").unwrap();
        assert!(
            range.contains(&aliased_cv),
            "dev-master aliased to 2.x-dev should satisfy ^2.0"
        );
    }

    /// Test that a branch alias entry for "1.0.x-dev" satisfies a ^1.0 constraint.
    #[test]
    fn test_branch_alias_1_x_in_range() {
        let aliased_cv = ComposerVersion::from_branch_alias_target("1.0.x-dev").unwrap();
        let range = constraint_to_ranges("^1.0").unwrap();
        assert!(
            range.contains(&aliased_cv),
            "dev branch aliased to 1.0.x-dev should satisfy ^1.0"
        );
        // But should NOT satisfy ^2.0
        let range2 = constraint_to_ranges("^2.0").unwrap();
        assert!(
            !range2.contains(&aliased_cv),
            "1.0.x-dev alias should not satisfy ^2.0"
        );
    }

    // ──────────── End-to-end tests (require network, marked #[ignore]) ────────────

    #[tokio::test]
    #[ignore]
    async fn test_resolve_monolog_e2e() {
        let request = ResolveRequest {
            require: vec![("monolog/monolog".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: Stability::Stable,
            stability_flags: HashMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repo_cache: None,
        };

        let result = resolve(&request).await;
        match result {
            Ok(packages) => {
                println!("Resolved {} packages:", packages.len());
                for pkg in &packages {
                    println!("  {} {}", pkg.name, pkg.version);
                }
                assert!(!packages.is_empty());
                assert!(packages.iter().any(|p| p.name == "monolog/monolog"));
            }
            Err(e) => panic!("Resolution failed: {}", e),
        }
    }
}
