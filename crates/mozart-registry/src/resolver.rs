//! Dependency resolver using the SAT solver.
//!
//! This module fetches package metadata from Packagist, builds a Pool of all
//! candidate packages, generates SAT rules, and runs the CDCL solver to find
//! a compatible set of packages to install.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::cache::Cache;
use crate::packagist;
use crate::vcs_bridge;
use mozart_core::package::{RawRepository, Stability};
use mozart_sat_resolver::{
    DefaultPolicy, PoolBuilder, PoolPackageInput, RuleSetGenerator, Solver, make_pool_links,
};
use mozart_semver::Version;

// ─────────────────────────────────────────────────────────────────────────────
// Version helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Determine the `Stability` of a `Version` from its pre_release string.
pub(crate) fn version_stability(v: &Version) -> Stability {
    match &v.pre_release {
        None => Stability::Stable,
        Some(pre) => {
            let lower = pre.to_lowercase();
            if lower.starts_with("dev") {
                Stability::Dev
            } else if lower.starts_with("alpha") || lower.starts_with('a') {
                Stability::Alpha
            } else if lower.starts_with("beta") || lower.starts_with('b') {
                Stability::Beta
            } else if lower.starts_with("rc") {
                Stability::RC
            } else {
                // patch/pl/p and unknown → stable
                Stability::Stable
            }
        }
    }
}

/// Parse a Packagist normalized version string like "1.2.3.0", "1.0.0.0-beta1".
/// Returns `None` for dev branches (dev-master, dev-*, *.x-dev).
pub(crate) fn parse_normalized(normalized: &str) -> Option<Version> {
    let s = normalized.trim();

    // Reject dev branches
    if s.to_lowercase().starts_with("dev-") {
        return None;
    }
    // Reject *.x-dev style
    if s.to_lowercase().ends_with("-dev") && s.contains(".x") {
        return None;
    }
    // Packagist uses 9999999.9999999.9999999.9999999 for dev branches
    if s.starts_with("9999999") {
        return None;
    }

    Version::parse(s).ok()
}

/// Parse a branch alias target like "2.x-dev" or "1.0.x-dev" into a `Version` with dev pre-release.
fn parse_branch_alias_target(alias_target: &str) -> Option<Version> {
    let s = alias_target.trim().to_lowercase();
    if !s.ends_with("-dev") {
        return None;
    }
    let base = &s[..s.len() - 4];
    let base = base.trim_end_matches(".x");
    let parts: Vec<&str> = base.split('.').collect();
    let major: u64 = parts.first().and_then(|p| p.parse().ok())?;
    let minor: u64 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch: u64 = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
    let build: u64 = parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);
    Some(Version {
        major,
        minor,
        patch,
        build,
        pre_release: Some("dev".to_string()),
        is_dev_branch: false,
        dev_branch_name: None,
    })
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

    /// Returns true if this is a platform package (php, ext-*, lib-*, composer pseudo packages).
    pub fn is_platform(&self) -> bool {
        mozart_core::platform::is_platform_package(&self.0)
    }

    /// Returns true if this is the virtual root package.
    pub fn is_root(&self) -> bool {
        self.0 == Self::ROOT
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
    pub fn new() -> Self {
        let detected = mozart_core::platform::detect_platform();
        let mut packages = HashMap::new();
        for pkg in detected {
            packages.insert(pkg.name, pkg.version);
        }
        Self { packages }
    }

    /// Parse platform packages into `Version` values.
    pub fn to_versions(&self) -> HashMap<String, Version> {
        self.packages
            .iter()
            .filter_map(|(name, version_str)| {
                Version::parse(version_str).ok().map(|v| (name.clone(), v))
            })
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Error types
// ─────────────────────────────────────────────────────────────────────────────

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
// Stability filter
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a version passes the minimum-stability filter for the given package.
fn passes_stability_filter(
    package_name: &str,
    version: &Version,
    minimum_stability: Stability,
    stability_flags: &HashMap<String, Stability>,
) -> bool {
    let min_stability = stability_flags
        .get(package_name)
        .copied()
        .unwrap_or(minimum_stability);
    let vs = version_stability(version);
    vs <= min_stability
}

/// Check whether a platform dependency should be skipped.
fn should_skip_platform_dep(
    dep_name: &str,
    ignore_platform_reqs: bool,
    ignore_platform_req_list: &[String],
) -> bool {
    if !PackageName(dep_name.to_string()).is_platform() {
        return false;
    }
    if ignore_platform_reqs {
        return true;
    }
    ignore_platform_req_list.iter().any(|p| p == dep_name)
}

// ─────────────────────────────────────────────────────────────────────────────
// Packagist → PoolPackageInput conversion
// ─────────────────────────────────────────────────────────────────────────────

/// Convert a Packagist version entry to PoolPackageInput(s).
/// May return multiple entries if branch aliases are present.
fn packagist_to_pool_inputs(
    package_name: &str,
    pv: &packagist::PackagistVersion,
    minimum_stability: Stability,
    stability_flags: &HashMap<String, Stability>,
) -> Vec<PoolPackageInput> {
    let mut results = Vec::new();

    let make_input = |version_str: &str, version_normalized: &str| -> PoolPackageInput {
        PoolPackageInput {
            name: package_name.to_string(),
            version: version_normalized.to_string(),
            pretty_version: version_str.to_string(),
            requires: make_pool_links(
                package_name,
                &pv.require
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            replaces: make_pool_links(
                package_name,
                &pv.replace
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            provides: make_pool_links(
                package_name,
                &pv.provide
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            conflicts: make_pool_links(
                package_name,
                &pv.conflict
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            is_fixed: false,
        }
    };

    match parse_normalized(&pv.version_normalized) {
        Some(v) => {
            if passes_stability_filter(package_name, &v, minimum_stability, stability_flags) {
                results.push(make_input(&pv.version, &pv.version_normalized));
            }
        }
        None => {
            // Dev branch — check for branch aliases
            let aliases = pv.branch_aliases();
            for (branch, alias_target) in &aliases {
                if branch.to_lowercase() != pv.version.to_lowercase() {
                    continue;
                }
                if let Some(alias_v) = parse_branch_alias_target(alias_target)
                    && passes_stability_filter(
                        package_name,
                        &alias_v,
                        minimum_stability,
                        stability_flags,
                    )
                {
                    results.push(make_input(&pv.version, alias_target));
                }
            }
        }
    }

    results
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API types
// ─────────────────────────────────────────────────────────────────────────────

/// Input to the resolver.
pub struct ResolveRequest {
    /// Root package name from composer.json "name" field (e.g. "laravel/laravel").
    /// Used in error messages. Falls back to `__root__` if empty.
    pub root_name: String,
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
    /// Temporary version constraint overrides (from --with flag).
    /// Maps package name (lowercase) to constraint string.
    pub temporary_constraints: HashMap<String, String>,
    /// VCS repositories from composer.json "repositories" section.
    /// Used to fetch packages from VCS before falling back to Packagist.
    pub repositories: Vec<RawRepository>,
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
    // 1. Build root requirements
    let mut root_requires: HashMap<String, Option<String>> = HashMap::new();

    for (name, constraint) in &request.require {
        if should_skip_platform_dep(
            name,
            request.ignore_platform_reqs,
            &request.ignore_platform_req_list,
        ) {
            continue;
        }
        root_requires.insert(name.to_lowercase(), Some(constraint.clone()));
    }

    if request.include_dev {
        for (name, constraint) in &request.require_dev {
            if should_skip_platform_dep(
                name,
                request.ignore_platform_reqs,
                &request.ignore_platform_req_list,
            ) {
                continue;
            }
            root_requires.insert(name.to_lowercase(), Some(constraint.clone()));
        }
    }

    // Apply temporary constraints (from --with flag or inline shorthand).
    // These override existing root constraints or add new ones for transitive deps.
    for (name, constraint) in &request.temporary_constraints {
        root_requires.insert(name.clone(), Some(constraint.clone()));
    }

    // Capture data needed by spawn_blocking
    let handle = tokio::runtime::Handle::current();
    let repo_cache = request.repo_cache.clone();
    let platform_config = request.platform.to_versions();
    let minimum_stability = request.minimum_stability;
    let stability_flags = request.stability_flags.clone();
    let prefer_stable = request.prefer_stable;
    let prefer_lowest = request.prefer_lowest;
    let ignore_platform_reqs = request.ignore_platform_reqs;
    let ignore_platform_req_list = request.ignore_platform_req_list.clone();
    let vcs_repositories = request.repositories.clone();

    // 2. Build pool, generate rules, and solve on a blocking thread
    tokio::task::spawn_blocking(move || -> Result<Vec<ResolvedPackage>, ResolveError> {
        let mut builder = PoolBuilder::new();

        // Set up ignore list for platform requirements
        let mut ignore_set: HashSet<String> = HashSet::new();
        if ignore_platform_reqs {
            // We'll skip platform deps in the loop below
        }
        for name in &ignore_platform_req_list {
            ignore_set.insert(name.clone());
        }
        builder.set_ignore_platform_reqs(ignore_set.clone());

        // Add platform packages as fixed entries
        let mut fixed_packages_by_name: HashMap<String, u32> = HashMap::new();
        for (name, version) in &platform_config {
            if should_skip_platform_dep(name, ignore_platform_reqs, &ignore_platform_req_list) {
                continue;
            }
            let input = PoolPackageInput {
                name: name.clone(),
                version: version.to_string(),
                pretty_version: version.to_string(),
                requires: vec![],
                replaces: vec![],
                provides: vec![],
                conflicts: vec![],
                is_fixed: true,
            };
            builder.add_package(input);
        }

        // Scan VCS repositories and collect packages from them
        let vcs_repos = &vcs_repositories;
        let vcs_packages = vcs_bridge::scan_vcs_repositories(vcs_repos);
        let mut vcs_package_names: HashSet<String> = HashSet::new();
        for vpkg in &vcs_packages {
            vcs_package_names.insert(vpkg.name.clone());
        }

        // Add VCS packages to the pool
        for vpkg in &vcs_packages {
            let inputs = vcs_bridge::vcs_to_pool_inputs(vpkg, minimum_stability, &stability_flags);
            for input in inputs {
                builder.add_package(input);
            }
        }

        // Seed the builder with packages for root requirements
        for name in root_requires.keys() {
            if PackageName(name.clone()).is_platform() {
                continue; // platform packages already added
            }

            // Skip packages already provided by VCS repositories
            if vcs_package_names.contains(name) {
                continue;
            }

            // Fetch available versions from Packagist
            let versions = handle
                .block_on(packagist::fetch_package_versions(name, repo_cache.as_ref()))
                .map_err(|e| {
                    ResolveError::DependencyFetchError(format!("Failed to fetch {}: {}", name, e))
                })?;

            for pv in &versions {
                let inputs =
                    packagist_to_pool_inputs(name, pv, minimum_stability, &stability_flags);
                for input in inputs {
                    builder.add_package(input);
                }
            }
        }

        // Explore transitive dependencies
        while let Some(name) = builder.next_pending() {
            if PackageName(name.clone()).is_platform() {
                // Platform package: already added if available, skip fetching
                continue;
            }

            // Skip packages already provided by VCS repositories
            if vcs_package_names.contains(&name) {
                continue;
            }

            let versions = match handle.block_on(packagist::fetch_package_versions(
                &name,
                repo_cache.as_ref(),
            )) {
                Ok(v) => v,
                Err(_) => {
                    // Virtual/meta packages (e.g. "psr/http-client-implementation")
                    // don't exist on Packagist. They are resolved via provides/replaces
                    // from other packages already in the pool.
                    continue;
                }
            };

            for pv in &versions {
                let inputs =
                    packagist_to_pool_inputs(&name, pv, minimum_stability, &stability_flags);
                for input in inputs {
                    builder.add_package(input);
                }
            }
        }

        // Build the pool
        let mut pool = builder.build();

        // Collect fixed package IDs
        let mut fixed_ids: Vec<u32> = Vec::new();
        for pkg in pool.packages() {
            if pkg.is_fixed {
                fixed_ids.push(pkg.id);
                fixed_packages_by_name.insert(pkg.name.clone(), pkg.id);
            }
        }

        // Generate rules
        let mut generator = RuleSetGenerator::new(&mut pool);
        generator.set_ignore_platform_reqs(ignore_set);
        let rules = generator.generate(&root_requires, &fixed_ids);

        // Create policy and solve
        let policy = DefaultPolicy::new(prefer_stable, prefer_lowest);
        let fixed_set: HashSet<u32> = fixed_ids.into_iter().collect();
        let solver = Solver::new(rules, &pool, policy, fixed_set);

        match solver.solve() {
            Ok(result) => {
                let mut resolved = Vec::new();
                for pkg_id in result.installed {
                    let pkg = pool.package_by_id(pkg_id);

                    // Skip platform packages from output
                    if PackageName(pkg.name.clone()).is_platform() {
                        continue;
                    }

                    let is_dev = if let Ok(v) = Version::parse(&pkg.version) {
                        version_stability(&v) == Stability::Dev
                    } else {
                        false
                    };

                    resolved.push(ResolvedPackage {
                        name: pkg.name.clone(),
                        version: pkg.pretty_version.clone(),
                        version_normalized: pkg.version.clone(),
                        is_dev,
                    });
                }
                Ok(resolved)
            }
            Err(e) => Err(ResolveError::NoSolution(e.to_string())),
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

    // ──────────── Version parsing helpers ────────────

    fn v(major: u64, minor: u64, patch: u64, build: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            build,
            pre_release: None,
            is_dev_branch: false,
            dev_branch_name: None,
        }
    }

    fn v_pre(major: u64, minor: u64, patch: u64, build: u64, pre: &str) -> Version {
        Version {
            major,
            minor,
            patch,
            build,
            pre_release: Some(pre.to_string()),
            is_dev_branch: false,
            dev_branch_name: None,
        }
    }

    // ──────────── parse_normalized ────────────

    #[test]
    fn test_parse_normalized_stable() {
        let ver = parse_normalized("1.2.3.0").unwrap();
        assert_eq!((ver.major, ver.minor, ver.patch, ver.build), (1, 2, 3, 0));
        assert_eq!(ver.pre_release, None);
    }

    #[test]
    fn test_parse_normalized_beta() {
        let ver = parse_normalized("1.0.0.0-beta1").unwrap();
        assert_eq!(ver.major, 1);
        assert_eq!(ver.pre_release, Some("beta1".to_string()));
    }

    #[test]
    fn test_parse_normalized_rc() {
        let ver = parse_normalized("2.0.0.0-RC3").unwrap();
        assert_eq!(ver.major, 2);
        assert_eq!(ver.pre_release, Some("RC3".to_string()));
    }

    #[test]
    fn test_parse_normalized_alpha() {
        let ver = parse_normalized("1.0.0.0-alpha2").unwrap();
        assert_eq!(ver.pre_release, Some("alpha2".to_string()));
    }

    #[test]
    fn test_parse_normalized_dev() {
        let ver = parse_normalized("1.0.0.0-dev").unwrap();
        assert_eq!(ver.pre_release, Some("dev".to_string()));
    }

    #[test]
    fn test_parse_normalized_dev_branch() {
        let ver = parse_normalized("dev-master");
        assert!(
            ver.is_none(),
            "dev-master should not parse as normalized version"
        );
    }

    #[test]
    fn test_parse_normalized_x_dev() {
        let ver = parse_normalized("dev-feature/foo");
        assert!(ver.is_none());
    }

    #[test]
    fn test_parse_normalized_9999999_dev() {
        let ver = parse_normalized("9999999.9999999.9999999.9999999-dev");
        assert!(ver.is_none());
    }

    #[test]
    fn test_parse_normalized_large_version() {
        let ver = parse_normalized("20031129").unwrap();
        assert_eq!(ver.major, 20031129);
        assert_eq!(ver.pre_release, None);
    }

    #[test]
    fn test_version_ordering_stable() {
        let v1 = parse_normalized("2.0.0.0").unwrap();
        let v2 = parse_normalized("1.0.0.0").unwrap();
        assert!(v1 > v2);
    }

    #[test]
    fn test_version_ordering_stability() {
        let stable = parse_normalized("1.0.0.0").unwrap();
        let rc = parse_normalized("1.0.0.0-RC1").unwrap();
        let beta = parse_normalized("1.0.0.0-beta1").unwrap();
        let alpha = parse_normalized("1.0.0.0-alpha1").unwrap();
        let dev = parse_normalized("1.0.0.0-dev").unwrap();
        assert!(stable > rc);
        assert!(rc > beta);
        assert!(beta > alpha);
        assert!(alpha > dev);
    }

    #[test]
    fn test_version_ordering_pre_number() {
        let beta2 = parse_normalized("1.0.0.0-beta2").unwrap();
        let beta1 = parse_normalized("1.0.0.0-beta1").unwrap();
        assert!(beta2 > beta1);
    }

    #[test]
    fn test_version_display() {
        let stable = v(1, 2, 3, 0);
        assert_eq!(format!("{stable}"), "1.2.3.0");

        let beta1 = v_pre(1, 0, 0, 0, "beta1");
        assert_eq!(format!("{beta1}"), "1.0.0.0-beta1");

        let rc2 = v_pre(2, 0, 0, 0, "RC2");
        assert_eq!(format!("{rc2}"), "2.0.0.0-RC2");

        let dev = v_pre(1, 0, 0, 0, "dev");
        assert_eq!(format!("{dev}"), "1.0.0.0-dev");
    }

    #[test]
    fn test_version_stability_fn() {
        assert_eq!(version_stability(&v(1, 0, 0, 0)), Stability::Stable);
        assert_eq!(version_stability(&v_pre(1, 0, 0, 0, "RC1")), Stability::RC);
        assert_eq!(
            version_stability(&v_pre(1, 0, 0, 0, "beta1")),
            Stability::Beta
        );
        assert_eq!(
            version_stability(&v_pre(1, 0, 0, 0, "alpha1")),
            Stability::Alpha
        );
        assert_eq!(version_stability(&v_pre(1, 0, 0, 0, "dev")), Stability::Dev);
        assert_eq!(
            version_stability(&v_pre(1, 0, 0, 0, "patch1")),
            Stability::Stable
        );
    }

    // ──────────── PackageName ────────────

    #[test]
    fn test_package_name_is_platform() {
        assert!(PackageName("php".to_string()).is_platform());
        assert!(PackageName("ext-json".to_string()).is_platform());
        assert!(PackageName("lib-curl".to_string()).is_platform());
        assert!(PackageName("composer".to_string()).is_platform());
        assert!(PackageName("composer-plugin-api".to_string()).is_platform());
        assert!(PackageName("composer-runtime-api".to_string()).is_platform());
        assert!(!PackageName("monolog/monolog".to_string()).is_platform());
        assert!(!PackageName("vendor/package".to_string()).is_platform());
    }

    #[test]
    fn test_package_name_is_root() {
        assert!(PackageName::root().is_root());
        assert!(!PackageName("monolog/monolog".to_string()).is_root());
    }

    // ──────────── Stability filter ────────────

    #[test]
    fn test_stability_filter() {
        let stable_v = v(1, 0, 0, 0);
        let alpha_v = v_pre(1, 1, 0, 0, "alpha1");
        let beta_v = v_pre(1, 0, 0, 0, "beta1");
        let rc_v = v_pre(1, 0, 0, 0, "RC1");
        let dev_v = v_pre(1, 0, 0, 0, "dev");

        let flags = HashMap::new();

        assert!(passes_stability_filter(
            "foo/foo",
            &stable_v,
            Stability::Stable,
            &flags
        ));
        assert!(!passes_stability_filter(
            "foo/foo",
            &alpha_v,
            Stability::Stable,
            &flags
        ));
        assert!(!passes_stability_filter(
            "foo/foo",
            &beta_v,
            Stability::Stable,
            &flags
        ));
        assert!(!passes_stability_filter(
            "foo/foo",
            &rc_v,
            Stability::Stable,
            &flags
        ));
        assert!(!passes_stability_filter(
            "foo/foo",
            &dev_v,
            Stability::Stable,
            &flags
        ));
    }

    #[test]
    fn test_stability_filter_beta() {
        let stable_v = v(1, 0, 0, 0);
        let beta_v = v_pre(1, 0, 0, 0, "beta1");
        let alpha_v = v_pre(1, 0, 0, 0, "alpha1");
        let dev_v = v_pre(1, 0, 0, 0, "dev");

        let flags = HashMap::new();

        assert!(passes_stability_filter(
            "foo/foo",
            &stable_v,
            Stability::Beta,
            &flags
        ));
        assert!(passes_stability_filter(
            "foo/foo",
            &beta_v,
            Stability::Beta,
            &flags
        ));
        assert!(!passes_stability_filter(
            "foo/foo",
            &alpha_v,
            Stability::Beta,
            &flags
        ));
        assert!(!passes_stability_filter(
            "foo/foo",
            &dev_v,
            Stability::Beta,
            &flags
        ));
    }

    #[test]
    fn test_stability_filter_dev() {
        let dev_v = v_pre(1, 0, 0, 0, "dev");
        let flags = HashMap::new();
        assert!(passes_stability_filter(
            "foo/foo",
            &dev_v,
            Stability::Dev,
            &flags
        ));
    }

    #[test]
    fn test_skip_platform_dep() {
        assert!(should_skip_platform_dep("php", true, &[]));
        assert!(should_skip_platform_dep("ext-json", true, &[]));
        assert!(!should_skip_platform_dep("monolog/monolog", true, &[]));
    }

    #[test]
    fn test_skip_specific_platform_dep() {
        let list = vec!["ext-intl".to_string()];
        assert!(should_skip_platform_dep("ext-intl", false, &list));
        assert!(!should_skip_platform_dep("ext-json", false, &list));
        assert!(!should_skip_platform_dep("php", false, &list));
        assert!(!should_skip_platform_dep("monolog/monolog", false, &list));
    }

    // ──────────── Branch alias tests ────────────

    #[test]
    fn test_parse_branch_alias_target_x_dev() {
        let ver = parse_branch_alias_target("2.x-dev").unwrap();
        assert_eq!((ver.major, ver.minor, ver.patch, ver.build), (2, 0, 0, 0));
        assert_eq!(ver.pre_release, Some("dev".to_string()));
    }

    #[test]
    fn test_parse_branch_alias_target_minor_x_dev() {
        let ver = parse_branch_alias_target("1.5.x-dev").unwrap();
        assert_eq!((ver.major, ver.minor, ver.patch), (1, 5, 0));
        assert_eq!(ver.pre_release, Some("dev".to_string()));
    }

    #[test]
    fn test_parse_branch_alias_target_patch_x_dev() {
        let ver = parse_branch_alias_target("1.0.2.x-dev").unwrap();
        assert_eq!((ver.major, ver.minor, ver.patch), (1, 0, 2));
        assert_eq!(ver.pre_release, Some("dev".to_string()));
    }

    #[test]
    fn test_parse_branch_alias_target_invalid() {
        assert!(parse_branch_alias_target("dev-master").is_none());
        assert!(parse_branch_alias_target("2.0.0").is_none());
        assert!(parse_branch_alias_target("").is_none());
    }

    // ──────────── SAT solver integration tests (offline) ────────────

    #[test]
    fn test_sat_resolve_simple_offline() {
        use mozart_sat_resolver::*;

        let mut pool = Pool::new(
            vec![
                PoolPackageInput {
                    name: "foo/foo".to_string(),
                    version: "1.0.0.0".to_string(),
                    pretty_version: "1.0.0".to_string(),
                    requires: vec![PoolLink {
                        target: "bar/bar".to_string(),
                        constraint: "^2.0".to_string(),
                        source: "foo/foo".to_string(),
                    }],
                    replaces: vec![],
                    provides: vec![],
                    conflicts: vec![],
                    is_fixed: false,
                },
                PoolPackageInput {
                    name: "bar/bar".to_string(),
                    version: "2.0.0.0".to_string(),
                    pretty_version: "2.0.0".to_string(),
                    requires: vec![],
                    replaces: vec![],
                    provides: vec![],
                    conflicts: vec![],
                    is_fixed: false,
                },
            ],
            vec![],
        );

        let mut requires = HashMap::new();
        requires.insert("foo/foo".to_string(), Some("^1.0".to_string()));

        let generator = RuleSetGenerator::new(&mut pool);
        let rules = generator.generate(&requires, &[]);

        let policy = DefaultPolicy::default();
        let solver = Solver::new(rules, &pool, policy, HashSet::new());
        let result = solver.solve().unwrap();

        // Should install foo/foo (id=1) and bar/bar (id=2)
        assert!(result.installed.contains(&1));
        assert!(result.installed.contains(&2));
    }

    // ──────────── End-to-end tests (require network, marked #[ignore]) ────────────

    #[tokio::test]
    #[ignore]
    async fn test_resolve_monolog_e2e() {
        let request = ResolveRequest {
            root_name: String::new(),
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
            temporary_constraints: HashMap::new(),
            repositories: vec![],
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
