//! Dependency resolver using the SAT solver.
//!
//! This module fetches package metadata from Packagist, builds a Pool of all
//! candidate packages, generates SAT rules, and runs the CDCL solver to find
//! a compatible set of packages to install.

use indexmap::{IndexMap, IndexSet};
use std::fmt;
use std::sync::Arc;

use crate::packagist;
use crate::repository::{PackageQuery, RepositorySet};
use crate::vcs_bridge;
use mozart_core::package::{RawRepository, Stability};
use mozart_sat_resolver::{
    DefaultPolicy, PoolBuilder, PoolLink, PoolPackageInput, RuleSetGenerator, Solver,
    make_pool_links,
};
use mozart_semver::Version;

// ─────────────────────────────────────────────────────────────────────────────
// Version helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Strip a `@stability` suffix from a constraint string and return the
/// cleaned constraint plus the parsed stability. Mirrors Composer's
/// `RootPackageLoader::extractStabilityFlags` (single-constraint case):
/// `"3.2.*@dev"` → (`"3.2.*"`, `Some(Stability::Dev)`).
pub(crate) fn extract_stability_suffix(constraint: &str) -> (String, Option<Stability>) {
    let trimmed = constraint.trim();
    if let Some(at_pos) = trimmed.rfind('@') {
        let suffix = &trimmed[at_pos + 1..];
        let stability = match suffix.to_lowercase().as_str() {
            "dev" => Some(Stability::Dev),
            "alpha" => Some(Stability::Alpha),
            "beta" => Some(Stability::Beta),
            "rc" => Some(Stability::RC),
            "stable" => Some(Stability::Stable),
            _ => None,
        };
        if let Some(s) = stability {
            let cleaned = trimmed[..at_pos].trim().to_string();
            // An empty constraint left after the strip means "any version" —
            // mirrors Composer's `@dev` shorthand (no version constraint).
            let cleaned = if cleaned.is_empty() {
                "*".to_string()
            } else {
                cleaned
            };
            return (cleaned, Some(s));
        }
    }
    (trimmed.to_string(), None)
}

/// Mirror Composer's `VersionParser::parseStability` for a single-atom
/// constraint string (no `@flag` suffix). Returns `Some(stability)` for
/// recognised non-stable constraints (`dev-foo`, `1.0.x-dev`, `1.0.0-beta1`,
/// …), `None` for stable or unrecognised forms (in which case
/// `minimum_stability` already applies).
///
/// Composer first strips a trailing `#hash` (handled here), then checks
/// `dev-` prefix / `-dev` suffix / a `(stab)?\d*` modifier. We follow the
/// same shape — the regex variant is overkill for inferring a flag.
pub(crate) fn infer_constraint_stability(constraint: &str) -> Option<Stability> {
    let s = constraint.trim();
    // Strip `#ref` (matches Composer's `parseStability` line 54).
    let s = match s.find('#') {
        Some(p) => &s[..p],
        None => s,
    };
    // Reject multi-atom constraints — extractStabilityFlags inspects each
    // sub-constraint individually but the most common single-atom case is
    // all we need for `dev-foo` / `1.0.x-dev` style root requires.
    if s.contains([' ', ',']) || s.contains("||") {
        return None;
    }
    // Strip a leading comparison operator (`>=1.0-beta` → `1.0-beta`).
    let s = s
        .strip_prefix(">=")
        .or_else(|| s.strip_prefix("<="))
        .or_else(|| s.strip_prefix("!="))
        .or_else(|| s.strip_prefix("=="))
        .or_else(|| s.strip_prefix('>'))
        .or_else(|| s.strip_prefix('<'))
        .or_else(|| s.strip_prefix('='))
        .or_else(|| s.strip_prefix('^'))
        .or_else(|| s.strip_prefix('~'))
        .unwrap_or(s);
    let lower = s.to_lowercase();
    if lower.starts_with("dev-") || lower.ends_with("-dev") {
        return Some(Stability::Dev);
    }
    // Match `<modifier><digits?>` at the end after the last `-`/`@`.
    // Composer uses `{(stable|RC|beta|alpha|dev)([.-]?\d+)?(?:\+.*)?$}`.
    let tail = lower
        .rsplit_once('-')
        .or_else(|| lower.rsplit_once('@'))
        .map(|(_, t)| t)
        .unwrap_or(&lower);
    let tail_word: String = tail.chars().take_while(|c| c.is_alphabetic()).collect();
    match tail_word.as_str() {
        "alpha" | "a" => Some(Stability::Alpha),
        "beta" | "b" => Some(Stability::Beta),
        "rc" => Some(Stability::RC),
        "patch" | "pl" | "p" | "stable" => Some(Stability::Stable),
        _ => None,
    }
}

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

/// Mirror Composer's `VersionParser::parseNumericAliasPrefix`: returns true
/// when the input is a numeric branch like `1.2-dev` / `1.2.3-dev` /
/// `1.2.x-dev` (i.e. the prefix is suitable for version comparison).
/// Non-numeric branches like `dev-main` / `dev-feature/x` return false.
fn has_numeric_alias_prefix(branch: &str) -> bool {
    let lower = branch.trim().to_lowercase();
    let lower = lower.strip_prefix('v').unwrap_or(&lower);
    let Some(base) = lower.strip_suffix("-dev") else {
        return false;
    };
    let base = base.strip_suffix(".x").unwrap_or(base);
    if base.is_empty() {
        return false;
    }
    // Allow only digit segments separated by `.`.
    base.split('.')
        .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()))
}

/// Mirror Composer's `VersionParser::normalizeBranch` for branch-alias
/// targets: turn a string like `"3.2.x-dev"` into the canonical numeric form
/// `"3.2.9999999.9999999-dev"`. Returns `None` if the input is not a numeric
/// branch (i.e. cannot be expanded to a four-segment numeric version).
///
/// Composer's flow for an `extra.branch-alias` value:
/// 1. Strip the trailing `-dev`.
/// 2. Pad missing segments with `.x`.
/// 3. Replace each `x` with `9999999`.
/// 4. Re-append `-dev`.
///
/// This is the form Composer's `Locker::lockPackages` writes into the
/// `aliases` block of `composer.lock` and the form `Pool` indexes for
/// constraint matching, so Mozart needs to use it too.
pub fn normalize_branch_alias_target(alias_target: &str) -> Option<String> {
    let trimmed = alias_target.trim();
    let lower = trimmed.to_lowercase();
    let base = lower.strip_suffix("-dev")?;
    // Strip leading v/V before normalizing, mirroring Composer's regex
    let base = base.strip_prefix('v').unwrap_or(base);
    let mut segments: Vec<String> = Vec::with_capacity(4);
    for seg in base.split('.') {
        if seg == "x" || seg == "X" || seg == "*" {
            segments.push("x".to_string());
        } else if seg.chars().all(|c| c.is_ascii_digit()) && !seg.is_empty() {
            segments.push(seg.to_string());
        } else {
            return None;
        }
    }
    if segments.is_empty() {
        return None;
    }
    while segments.len() < 4 {
        segments.push("x".to_string());
    }
    let expanded: Vec<String> = segments
        .into_iter()
        .map(|s| if s == "x" { "9999999".to_string() } else { s })
        .collect();
    Some(format!("{}-dev", expanded.join(".")))
}

/// Mirror Composer's `VersionParser::normalize` for the values that appear on
/// either side of an `as` clause (`require: "1.0.x-dev as dev-master"`).
///
/// Composer sends both sides through `normalize`, which:
/// - Maps `master` / `trunk` / `default` (with optional `dev-` prefix) to
///   `9999999-dev`. Mozart's pool uses the four-segment expansion
///   `9999999.9999999.9999999.9999999-dev`, which is what
///   `make_default_branch_alias` emits — keep the same form here so a root
///   `as dev-master` lines up with synthetic default-branch aliases.
/// - Strips a leading `v` and treats numeric `*.x-dev` branches via
///   `normalizeBranch` (= `normalize_branch_alias_target`).
/// - Leaves other `dev-NAME` strings as `dev-NAME`.
fn normalize_root_alias_atom(atom: &str) -> Option<String> {
    let trimmed = atom.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    let stripped = lower.strip_prefix("dev-").unwrap_or(&lower);
    if matches!(stripped, "master" | "trunk" | "default") {
        return Some("9999999.9999999.9999999.9999999-dev".to_string());
    }
    if let Some(numeric) = normalize_branch_alias_target(trimmed) {
        return Some(numeric);
    }
    if let Some(rest) = lower.strip_prefix("dev-") {
        return Some(format!("dev-{rest}"));
    }
    parse_normalized(trimmed).map(|_| trimmed.to_string())
}

/// A root-level alias declared via the `require: "X as Y"` shorthand on the
/// root composer.json. Mirrors Composer's
/// `RootPackageLoader::extractAliases` entries: when the resolver loads a
/// package matching `(package, version_normalized)`, it materializes an extra
/// alias entry exposing the same install under `alias_normalized`/`alias`.
#[derive(Debug, Clone)]
struct RootAlias {
    package: String,
    /// Normalized form of the LEFT-hand side (the actual constraint).
    version_normalized: String,
    /// Pretty form of the RIGHT-hand side (the alias to expose).
    alias: String,
    /// Normalized form of the RIGHT-hand side.
    alias_normalized: String,
}

/// Strip a single-atom `<X> as <Y>` clause from a constraint string. Returns
/// the cleaned constraint plus the `(left, right)` pieces when an alias is
/// present. Mirrors Composer's `VersionParser::parseConstraint` `as`-strip:
/// the constraint passed to the resolver is the LEFT side, and a separate
/// alias entry is recorded for the RIGHT side. A trailing `#hex` reference
/// (`dev-main#abcd`) is also stripped — Composer's `extractAliases` regex
/// `([^,\s#|]+)(?:#[^ ]+)?` excludes it from the captured constraint, and
/// `RootPackageLoader::extractReferences` records the hash separately for
/// the post-resolve `setSourceDistReferences` pass.
fn strip_root_alias_clause(constraint: &str) -> (String, Option<(String, String)>) {
    let trimmed = constraint.trim();
    if let Some(idx) = trimmed.find(" as ") {
        let before = trimmed[..idx].trim();
        let after = trimmed[idx + 4..].trim();
        if !before.is_empty()
            && !after.is_empty()
            && !before.contains([' ', '\t', ',', '|'])
            && !after.contains([' ', '\t', ',', '|'])
        {
            let cleaned = strip_inline_reference(before);
            return (cleaned.clone(), Some((cleaned, after.to_string())));
        }
    }
    (strip_inline_reference(trimmed), None)
}

/// Drop a trailing `#hex` reference from a single-atom `dev-*` / `*-dev`
/// constraint, matching Composer's `'{^[^,\s@]+?#([a-f0-9]+)$}'` guard.
/// Lockfile generation records the reference separately via
/// `extract_root_references` and applies it after resolution, so the SAT
/// constraint itself only needs the bare branch name.
fn strip_inline_reference(s: &str) -> String {
    if let Some((head, hash)) = s.rsplit_once('#')
        && !hash.is_empty()
        && hash.chars().all(|c| c.is_ascii_hexdigit())
        && !head.contains([' ', '\t', ',', '@'])
        && (head.to_lowercase().starts_with("dev-") || head.to_lowercase().ends_with("-dev"))
    {
        return head.to_string();
    }
    s.to_string()
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
    pub packages: IndexMap<String, String>,
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
        let mut packages = IndexMap::new();
        for pkg in detected {
            packages.insert(pkg.name, pkg.version);
        }
        Self { packages }
    }

    /// Apply `config.platform` overrides on top of the detected packages.
    ///
    /// Mirrors `Composer\Repository\PlatformRepository::__construct`'s
    /// `$overrides` handling: each override either replaces a detected
    /// package version or adds a virtual one (e.g. `ext-dummy`). A `false`
    /// value disables the package, removing it from the platform.
    pub fn apply_overrides(&mut self, overrides: &serde_json::Value) {
        let Some(obj) = overrides.as_object() else {
            return;
        };
        for (name, value) in obj {
            let key = name.to_lowercase();
            if value.as_bool() == Some(false) {
                self.packages.shift_remove(&key);
                continue;
            }
            if let Some(s) = value.as_str() {
                self.packages.insert(key, s.to_string());
            }
        }
    }

    /// Parse platform packages into `Version` values.
    pub fn to_versions(&self) -> IndexMap<String, Version> {
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
    stability_flags: &IndexMap<String, Stability>,
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
    ignore_platform_req_list
        .iter()
        .any(|p| mozart_core::matches_wildcard(dep_name, p))
}

// ─────────────────────────────────────────────────────────────────────────────
// Packagist → PoolPackageInput conversion
// ─────────────────────────────────────────────────────────────────────────────

/// Mirrors `Composer\Package\CompletePackage::isAbandoned`: any
/// `abandoned: true` or `abandoned: "<replacement>"` value is truthy.
/// `abandoned: false` and an empty string both register as not-abandoned.
fn is_abandoned(pv: &packagist::PackagistVersion) -> bool {
    match &pv.abandoned {
        None => false,
        Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Convert a Packagist version entry to PoolPackageInput(s).
/// May return multiple entries if branch aliases are present.
fn packagist_to_pool_inputs(
    package_name: &str,
    pv: &packagist::PackagistVersion,
    minimum_stability: Stability,
    stability_flags: &IndexMap<String, Stability>,
) -> Vec<PoolPackageInput> {
    let mut results = Vec::new();

    let make_input = |version_str: &str,
                      version_normalized: &str,
                      is_alias_of: Option<String>|
     -> PoolPackageInput {
        PoolPackageInput {
            name: package_name.to_string(),
            version: version_normalized.to_string(),
            pretty_version: version_str.to_string(),
            requires: make_pool_links(
                package_name,
                version_normalized,
                &pv.require
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            replaces: make_pool_links(
                package_name,
                version_normalized,
                &pv.replace
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            provides: make_pool_links(
                package_name,
                version_normalized,
                &pv.provide
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            conflicts: make_pool_links(
                package_name,
                version_normalized,
                &pv.conflict
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            ),
            is_fixed: false,
            is_alias_of,
        }
    };

    match parse_normalized(&pv.version_normalized) {
        Some(v) => {
            if passes_stability_filter(package_name, &v, minimum_stability, stability_flags) {
                results.push(make_input(&pv.version, &pv.version_normalized, None));
            }
        }
        None => {
            // Dev branch — emit the original entry (so the alias has a target
            // to point at) and one alias entry per matching `extra.branch-alias`.
            // Mirrors Composer's `ArrayRepository::addPackage` which adds the
            // base package and then calls `createAliasPackage` for each
            // branch-alias declaration on it.
            let original_passes = passes_stability_filter(
                package_name,
                &Version {
                    major: 0,
                    minor: 0,
                    patch: 0,
                    build: 0,
                    pre_release: Some("dev".to_string()),
                    is_dev_branch: true,
                    dev_branch_name: None,
                },
                minimum_stability,
                stability_flags,
            );
            if !original_passes {
                return results;
            }
            results.push(make_input(&pv.version, &pv.version_normalized, None));

            let aliases = pv.branch_aliases();
            let mut emitted_explicit_alias = false;
            for (branch, alias_target) in &aliases {
                if branch.to_lowercase() != pv.version.to_lowercase() {
                    continue;
                }
                if parse_branch_alias_target(alias_target).is_none() {
                    continue;
                }
                let Some(alias_normalized) = normalize_branch_alias_target(alias_target) else {
                    continue;
                };
                results.push(make_input(
                    alias_target,
                    &alias_normalized,
                    Some(pv.version_normalized.clone()),
                ));
                emitted_explicit_alias = true;
            }

            // Mirror Composer's `ArrayLoader::getBranchAlias`: when a
            // `dev-` package carries `default-branch: true` and the version
            // has no numeric prefix (i.e. it isn't already a `1.0.x-dev` form
            // that would be its own alias), synthesize the `9999999-dev`
            // alias so root constraints like `dev-main` pick up a default
            // branch surfaced as `9999999-dev` in the lock + trace output.
            //
            // `getBranchAlias` returns the *first* matching branch-alias when
            // one exists — i.e. an explicit `branch-alias` entry takes
            // precedence over the `default-branch` synthetic one. Skip the
            // synthetic alias when an explicit one has already been emitted
            // for this version.
            if pv.default_branch
                && !emitted_explicit_alias
                && !has_numeric_alias_prefix(&pv.version)
            {
                let default_alias = "9999999-dev";
                let default_normalized = "9999999.9999999.9999999.9999999-dev";
                let already_present = results
                    .iter()
                    .any(|r| r.version == default_normalized && r.name == package_name);
                if !already_present {
                    results.push(make_input(
                        default_alias,
                        default_normalized,
                        Some(pv.version_normalized.clone()),
                    ));
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
    /// Root package version from composer.json "version" field. `None` falls
    /// back to Composer's `RootPackage::DEFAULT_PRETTY_VERSION` (1.0.0+no-version-set).
    /// Used to seed a fixed pool entry for the root so transitive requires
    /// pointing at the root (legal circular dependencies via an intermediate
    /// package) can be satisfied.
    pub root_version: Option<String>,
    /// Dependencies from composer.json "require" section.
    pub require: Vec<(String, String)>,
    /// Dependencies from composer.json "require-dev" section.
    pub require_dev: Vec<(String, String)>,
    /// Whether to include require-dev in resolution.
    pub include_dev: bool,
    /// Minimum stability from composer.json.
    pub minimum_stability: Stability,
    /// Per-package stability overrides.
    pub stability_flags: IndexMap<String, Stability>,
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
    /// Repository set used to fetch package metadata. Mirrors Composer's
    /// `RepositoryManager`. Production builders construct this with a single
    /// `PackagistRepository`; in-process test harnesses can construct one
    /// without any HTTP-backed repos to mimic Composer's
    /// `'packagist' => false` test config.
    pub repositories: Arc<RepositorySet>,
    /// Temporary version constraint overrides (from --with flag).
    /// Maps package name (lowercase) to constraint string.
    pub temporary_constraints: IndexMap<String, String>,
    /// VCS / inline-package repository entries from composer.json's
    /// `repositories` section, used by the eager VCS scan and inline-package
    /// preload that still live in `resolve()` (Step B follow-up will move
    /// these through `RepositorySet` too).
    pub raw_repositories: Vec<RawRepository>,
    /// Root composer.json's `provide` map (target → constraint string). Drives
    /// the self-fulfilling-rule check in the SAT generator: when a root
    /// `require` names something the root itself `provide`s with a matching
    /// constraint, no install-one-of rule is emitted, mirroring Composer's
    /// `RuleSetGenerator::createRequireRule` self-fulfillment branch.
    pub root_provide: IndexMap<String, String>,
    /// Root composer.json's `replace` map. Same role as `root_provide` for the
    /// `replace` link: a replaced target counts as fulfilled by the root.
    pub root_replace: IndexMap<String, String>,
    /// Root composer.json's `conflict` map (target → constraint). Composer's
    /// `RootPackageRepository` carries these onto the in-pool root package
    /// entry; the SAT generator then forbids any candidate matching the
    /// constraint, so a root `conflict` blocks both direct selection of the
    /// targeted version and any alias / replace / provide that would resolve
    /// to it.
    pub root_conflict: IndexMap<String, String>,
    /// Lowercase names of packages that are pinned to their lock-file version
    /// for this resolve (a partial update where the package is not in the
    /// update list). Mirrors the `propagateUpdate=false` branch of Composer's
    /// `PoolBuilder::loadPackage`: locked-only packages do not pick up
    /// `require: "X as Y"` root aliases. Empty for installs and full updates,
    /// where every package can take aliases as usual.
    pub locked_package_names: IndexSet<String>,
    /// Full data of packages pinned to their lock-file version (a partial
    /// update). Each entry is added to the pool as a fixed entry, mirroring
    /// Composer's `Request::lockPackage` + `PoolBuilder::buildPool`'s
    /// `getFixedOrLockedPackages` loop: a locked-only package's pretty/normalized
    /// version, requires, replaces, provides and conflicts all enter the pool
    /// at exactly one version, so the SAT solver cannot pick a different
    /// version (whether directly or via another package's `replace`). Empty
    /// for installs and full updates.
    pub locked_packages: Vec<LockedPackageInfo>,
    /// When true, drop abandoned packages (`abandoned: true|<replacement>`)
    /// from the pool before solving. Mirrors Composer's
    /// `audit.block-abandoned` config feeding into
    /// `SecurityAdvisoryPoolFilter`: the resolver simply never sees these
    /// versions, so a root requirement that only matches abandoned candidates
    /// fails with the standard "could not be resolved" error.
    pub block_abandoned: bool,
    /// Pretty form of the root's `extra.branch-alias` target when the root's
    /// version matches a key in that map (e.g. `dev-master` → `2.0-dev`).
    /// Mirrors Composer's `RootAliasPackage`: an extra alias entry is added
    /// to the pool exposing the root under the numeric branch-alias version,
    /// with `replace`/`provide`/`conflict` links extended to advertise the
    /// alias's version for any link originally written as `self.version`.
    /// `None` when the root carries no matching `branch-alias` entry.
    pub root_branch_alias: Option<String>,
    /// `name → normalized version` map fed to the policy's preferred-version
    /// override. Used by `update --minimal-changes` so the solver only moves
    /// a package when a constraint actually forces a different version.
    /// Empty for a normal full update.
    pub preferred_versions: IndexMap<String, String>,
}

/// Full data for a lock-pinned package, used in partial updates. Carried on
/// `ResolveRequest::locked_packages` and turned into a fixed pool entry
/// inside `resolve()`. Mirrors what Composer's `PoolBuilder` reads off a
/// `BasePackage` retrieved from the locked repository.
pub struct LockedPackageInfo {
    pub name: String,
    /// Pretty (display) version, e.g. "1.2.3".
    pub pretty_version: String,
    /// Normalized version, e.g. "1.2.3.0".
    pub version_normalized: String,
    pub requires: Vec<(String, String)>,
    pub replaces: Vec<(String, String)>,
    pub provides: Vec<(String, String)>,
    pub conflicts: Vec<(String, String)>,
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
    /// When `Some`, this entry is an `AliasPackage` rather than a real
    /// install target. The value is the target's normalized version, used
    /// by lock-file generation to populate the `aliases[]` block (and by
    /// the installer to emit `Marking ... as installed, alias of ...`
    /// trace lines). Real packages have `alias_of: None`.
    pub alias_of_normalized: Option<String>,
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
    let mut root_requires: IndexMap<String, Option<String>> = IndexMap::new();
    // Per-package stability overrides extracted from `@dev`/`@beta`/etc.
    // suffixes on root constraints. Mirrors Composer's
    // `RootPackageLoader::extractStabilityFlags`. Merged on top of the
    // request's caller-supplied flags (which today are usually empty).
    let mut stability_flags: IndexMap<String, Stability> = request.stability_flags.clone();
    // Root-level aliases extracted from `require: "X as Y"`. Mirrors
    // Composer's `RootPackageLoader::extractAliases`: each entry adds a new
    // alias package to the pool exposing the matched real package under the
    // RIGHT-hand version label.
    let mut root_aliases: Vec<RootAlias> = Vec::new();

    let minimum_stability = request.minimum_stability;
    let mut insert_root_require = |name: &str, constraint: &str| {
        // Strip any `<X> as <Y>` clause first (mirrors Composer's
        // `parseConstraint` strip + `extractAliases` capture). The cleaned
        // constraint feeds the resolver; the alias is recorded for a second
        // pool-population pass once real packages are in.
        let (constraint_no_as, alias_pieces) = strip_root_alias_clause(constraint);
        if let Some((target_atom, alias_atom)) = alias_pieces
            && let (Some(target_normalized), Some(alias_normalized)) = (
                normalize_root_alias_atom(&target_atom),
                normalize_root_alias_atom(&alias_atom),
            )
        {
            root_aliases.push(RootAlias {
                package: name.to_lowercase(),
                version_normalized: target_normalized,
                alias: alias_atom,
                alias_normalized,
            });
        }
        let (clean, stability) = extract_stability_suffix(&constraint_no_as);
        let lower = name.to_lowercase();
        if let Some(s) = stability {
            let entry = stability_flags.entry(lower.clone()).or_insert(s);
            if (*entry as u8) > (s as u8) {
                *entry = s;
            }
        } else if let Some(inferred) = infer_constraint_stability(&clean) {
            // Mirrors `RootPackageLoader::extractStabilityFlags` second loop:
            // when a single-atom constraint like `dev-main` or `1.0.x-dev`
            // implies a non-stable stability and no explicit `@flag` was
            // given, raise that package's stability ceiling so the pool
            // accepts it. Only applied when the inferred level is *more*
            // permissive than `minimum_stability` and any existing flag.
            if (inferred as u8) > (minimum_stability as u8) {
                let entry = stability_flags.entry(lower.clone()).or_insert(inferred);
                if (*entry as u8) < (inferred as u8) {
                    *entry = inferred;
                }
            }
        }
        root_requires.insert(lower, Some(clean));
    };

    for (name, constraint) in &request.require {
        if should_skip_platform_dep(
            name,
            request.ignore_platform_reqs,
            &request.ignore_platform_req_list,
        ) {
            continue;
        }
        insert_root_require(name, constraint);
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
            insert_root_require(name, constraint);
        }
    }

    // Apply temporary constraints (from --with flag or inline shorthand).
    // These override existing root constraints or add new ones for transitive deps.
    for (name, constraint) in &request.temporary_constraints {
        insert_root_require(name, constraint);
    }

    // 2. Build pool, generate rules, and solve
    let mut builder = PoolBuilder::new();

    // Set up ignore list for platform requirements
    let mut ignore_set: IndexSet<String> = IndexSet::new();
    for name in &request.ignore_platform_req_list {
        ignore_set.insert(name.clone());
    }
    builder.set_ignore_platform_reqs(ignore_set.clone());
    builder.set_ignore_all_platform_reqs(request.ignore_platform_reqs);

    // Add platform packages as fixed entries
    let platform_config = request.platform.to_versions();
    let mut fixed_packages_by_name: IndexMap<String, u32> = IndexMap::new();
    for (name, version) in &platform_config {
        if should_skip_platform_dep(
            name,
            request.ignore_platform_reqs,
            &request.ignore_platform_req_list,
        ) {
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
            is_alias_of: None,
        };
        builder.add_package(input);
    }

    // Mirror Composer's `RootPackageRepository`: put the root package itself
    // in the pool as a fixed entry so transitive requires pointing at the
    // root (legal circular dependencies via an intermediate package) can
    // resolve. Composer clears the root's `require` / `require-dev` on this
    // copy because the root requires are already plumbed through the
    // rule generator's root-require path; carrying them here too would
    // emit duplicate rules. Provide / replace links survive, so virtual
    // packages declared on the root keep working for transitive consumers.
    let root_name_lower = request.root_name.to_lowercase();
    if !root_name_lower.is_empty() {
        let (root_pretty, root_normalized) = match request.root_version.as_deref() {
            Some(v) if !v.is_empty() => (v.to_string(), v.to_string()),
            _ => ("1.0.0+no-version-set".to_string(), "1.0.0.0".to_string()),
        };
        // Resolve `self.version` against the root's normalized version when
        // building base links. Mirrors Composer's `ArrayLoader::createLink`:
        // a `self.version` constraint is parsed against the declaring package's
        // pretty version (here, the root's). The base entry only carries this
        // resolved form; any branch-alias entry below extends each base link
        // with an extra link tagged at the alias's version, matching
        // `AliasPackage::replaceSelfVersionDependencies`.
        let make_base_links = |raw: &IndexMap<String, String>| -> Vec<PoolLink> {
            raw.iter()
                .map(|(target, constraint)| PoolLink {
                    target: target.to_lowercase(),
                    constraint: if constraint.trim() == "self.version" {
                        root_normalized.clone()
                    } else {
                        constraint.clone()
                    },
                    source: root_name_lower.clone(),
                })
                .collect()
        };
        let base_replaces = make_base_links(&request.root_replace);
        let base_provides = make_base_links(&request.root_provide);
        let base_conflicts = make_base_links(&request.root_conflict);
        let root_input = PoolPackageInput {
            name: root_name_lower.clone(),
            version: root_normalized.clone(),
            pretty_version: root_pretty.clone(),
            requires: vec![],
            replaces: base_replaces.clone(),
            provides: base_provides.clone(),
            conflicts: base_conflicts.clone(),
            is_fixed: true,
            is_alias_of: None,
        };
        builder.add_package(root_input);

        // Materialize a branch-alias entry for the root when `extra.branch-alias`
        // mapped this version to a numeric alias (e.g. dev-master → 2.0-dev).
        // Mirrors Composer's `RootAliasPackage`: the alias copies the base's
        // resolved replace/provide/conflict links and then ADDS one more link
        // per `self.version` original, this time pinned at the alias's own
        // version. So a transitive `provided/dependency 2.*` lookup can be
        // satisfied through the alias even though the base resolved
        // `self.version` to a non-matching dev version.
        if let Some(alias_pretty) = &request.root_branch_alias
            && let Some(alias_normalized) = normalize_branch_alias_target(alias_pretty)
        {
            let extra_self_version_links = |raw: &IndexMap<String, String>| -> Vec<PoolLink> {
                raw.iter()
                    .filter(|(_, constraint)| constraint.trim() == "self.version")
                    .map(|(target, _)| PoolLink {
                        target: target.to_lowercase(),
                        constraint: alias_normalized.clone(),
                        source: root_name_lower.clone(),
                    })
                    .collect()
            };
            let mut alias_replaces = base_replaces.clone();
            alias_replaces.extend(extra_self_version_links(&request.root_replace));
            let mut alias_provides = base_provides.clone();
            alias_provides.extend(extra_self_version_links(&request.root_provide));
            let mut alias_conflicts = base_conflicts.clone();
            alias_conflicts.extend(extra_self_version_links(&request.root_conflict));
            builder.add_package(PoolPackageInput {
                name: root_name_lower.clone(),
                version: alias_normalized,
                pretty_version: alias_pretty.clone(),
                requires: vec![],
                replaces: alias_replaces,
                provides: alias_provides,
                conflicts: alias_conflicts,
                is_fixed: false,
                is_alias_of: Some(root_normalized),
            });
        }
    }

    // Add lock-pinned packages as pool entries (partial-update case).
    //
    // Mirrors Composer's `PoolBuilder::buildPool` flow: every locked package
    // not in the `updateAllowList` is added through `Request::lockPackage`,
    // then re-entered into the pool via the `getFixedOrLockedPackages`
    // loop. Crucially, a *locked* package is NOT a *fixed* package
    // (Request.php:89-98): the SAT solver does not force its installation,
    // so a locked package whose root require has been removed will simply
    // drop out of the result. The locked entry's purpose is to constrain
    // the pool to *only* the locked version for that name — every other
    // version is filtered out below — so other packages cannot pick a
    // different version (whether directly, or via `replace`, which would
    // otherwise let an upgraded replacer silently drop the dependency).
    //
    // Pre-check: a locked package whose version is rejected by the
    // current minimum-stability (composer.json may have tightened
    // stability or dropped a `stability-flags` entry the lock relied on)
    // cannot be reused as a fixed pool entry. Mirrors what Composer
    // surfaces via `Pool::isUnacceptableFixedOrLockedPackage` +
    // `Problem::getPrettyString`: bail with the "fixed to <v> (lock file
    // version) but that version is rejected by your minimum-stability"
    // pointer so the user knows to add the package to the update
    // arguments (or use `--with-all-dependencies`).
    {
        let mut rejected: Vec<String> = Vec::new();
        for locked in &request.locked_packages {
            let Ok(v) = Version::parse(&locked.version_normalized) else {
                continue;
            };
            if !passes_stability_filter(
                &locked.name,
                &v,
                request.minimum_stability,
                &stability_flags,
            ) {
                rejected.push(format!(
                    "    - {} is fixed to {} (lock file version) by a partial update but that version is rejected by your minimum-stability. Make sure you list it as an argument for the update command.",
                    locked.name, locked.pretty_version
                ));
            }
        }
        if !rejected.is_empty() {
            let report = rejected
                .into_iter()
                .enumerate()
                .map(|(i, msg)| format!("  Problem {}\n{}", i + 1, msg))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(ResolveError::NoSolution(report));
        }
    }

    // Build a map first so the filter below knows which (name, version)
    // pairs are the only allowed entries for locked names.
    let locked_name_to_version: IndexMap<String, String> = request
        .locked_packages
        .iter()
        .map(|p| (p.name.to_lowercase(), p.version_normalized.clone()))
        .collect();
    let lock_filter_allows = |name: &str, version: &str| -> bool {
        match locked_name_to_version.get(&name.to_lowercase()) {
            Some(locked_version) => locked_version == version,
            None => true,
        }
    };
    for locked in &request.locked_packages {
        let locked_name_lower = locked.name.to_lowercase();
        let input = PoolPackageInput {
            name: locked_name_lower.clone(),
            version: locked.version_normalized.clone(),
            pretty_version: locked.pretty_version.clone(),
            requires: make_pool_links(
                &locked_name_lower,
                &locked.version_normalized,
                &locked.requires,
            ),
            replaces: make_pool_links(
                &locked_name_lower,
                &locked.version_normalized,
                &locked.replaces,
            ),
            provides: make_pool_links(
                &locked_name_lower,
                &locked.version_normalized,
                &locked.provides,
            ),
            conflicts: make_pool_links(
                &locked_name_lower,
                &locked.version_normalized,
                &locked.conflicts,
            ),
            is_fixed: false,
            is_alias_of: None,
        };
        builder.add_package(input);
    }

    // Scan VCS repositories and collect packages from them
    let vcs_packages = vcs_bridge::scan_vcs_repositories(&request.raw_repositories).await;
    let mut vcs_package_names: IndexSet<String> = IndexSet::new();
    for vpkg in &vcs_packages {
        vcs_package_names.insert(vpkg.name.clone());
    }

    // Add VCS packages to the pool
    for vpkg in &vcs_packages {
        let inputs =
            vcs_bridge::vcs_to_pool_inputs(vpkg, request.minimum_stability, &stability_flags);
        for input in inputs {
            if !lock_filter_allows(&input.name, &input.version) {
                continue;
            }
            builder.add_package(input);
        }
    }

    // Collect inline `type: package` repositories. These don't require any
    // network fetch, but we mirror Composer's `PackageRepository` (which
    // extends `ArrayRepository`) and only emit packages whose own `name`
    // matches a queried name — `replace`/`provide` targets do NOT pull in
    // their replacers eagerly. So we build a name-indexed lookup and add
    // entries to the builder on demand from the seed/transitive loops.
    // Loading every inline package up front would let the SAT resolver
    // pick a replacer that nothing required by name (e.g.
    // `broken-deps-do-not-replace.test`), where Composer would correctly
    // surface the broken dependency instead.
    let inline_packages = crate::inline_package::collect_inline_packages(&request.raw_repositories);
    let mut inline_packages_by_name: IndexMap<String, Vec<&crate::inline_package::InlinePackage>> =
        IndexMap::new();
    for ipkg in &inline_packages {
        inline_packages_by_name
            .entry(ipkg.name.clone())
            .or_default()
            .push(ipkg);
    }
    let add_inline_for = |name: &str, builder: &mut PoolBuilder| -> bool {
        let Some(packages) = inline_packages_by_name.get(name) else {
            return false;
        };
        for ipkg in packages {
            if request.block_abandoned && is_abandoned(&ipkg.version) {
                continue;
            }
            let inputs = packagist_to_pool_inputs(
                &ipkg.name,
                &ipkg.version,
                request.minimum_stability,
                &stability_flags,
            );
            for input in inputs {
                if !lock_filter_allows(&input.name, &input.version) {
                    continue;
                }
                builder.add_package(input);
            }
        }
        true
    };

    // Collect packages from `type: composer` repositories with file:// URLs.
    // The harness rewrites `file://foobar` to `file:///abs/path` before this
    // call so the read can be a plain `std::fs::read_to_string`. Same idea
    // as inline packages — they bypass the RepositorySet and go straight
    // into the pool, with names recorded so Packagist loops skip them.
    let composer_repo_packages =
        crate::composer_repo::collect_composer_packages(&request.raw_repositories);
    let mut composer_repo_names: IndexSet<String> = IndexSet::new();
    for cpkg in &composer_repo_packages {
        composer_repo_names.insert(cpkg.name.clone());
        if request.block_abandoned && is_abandoned(&cpkg.version) {
            continue;
        }
        let inputs = packagist_to_pool_inputs(
            &cpkg.name,
            &cpkg.version,
            request.minimum_stability,
            &stability_flags,
        );
        for input in inputs {
            if !lock_filter_allows(&input.name, &input.version) {
                continue;
            }
            builder.add_package(input);
        }
    }

    // The repository set is supplied by the caller. Today production
    // builders pass a single-Packagist set; in-process tests can pass a
    // set with no HTTP-backed repos. VCS and inline packages above are
    // still preloaded directly, and their names go into the skip lists so
    // we don't double-load them through this set.
    let repo_set: &RepositorySet = &request.repositories;

    // Seed the builder with packages for root requirements. Inline
    // `type: package` matches are added directly via the name-indexed
    // lookup; everything else falls through to the network-backed
    // repository set.
    let seed_names: Vec<String> = root_requires
        .keys()
        .filter(|name| !PackageName((*name).clone()).is_platform())
        .filter(|name| !vcs_package_names.contains(*name) && !composer_repo_names.contains(*name))
        .cloned()
        .collect();
    let mut seed_queries: Vec<PackageQuery<'_>> = Vec::new();
    for name in &seed_names {
        if add_inline_for(name.as_str(), &mut builder) {
            continue;
        }
        seed_queries.push(PackageQuery {
            name: name.as_str(),
            constraint: root_requires.get(name).and_then(|c| c.as_deref()),
        });
    }
    let seed_results = repo_set
        .load_packages(&seed_queries)
        .await
        .map_err(|e| ResolveError::DependencyFetchError(e.to_string()))?;
    for r in &seed_results {
        if request.block_abandoned && is_abandoned(&r.version) {
            continue;
        }
        let inputs = packagist_to_pool_inputs(
            &r.name,
            &r.version,
            request.minimum_stability,
            &stability_flags,
        );
        for input in inputs {
            if !lock_filter_allows(&input.name, &input.version) {
                continue;
            }
            builder.add_package(input);
        }
    }

    // Explore transitive dependencies.
    while let Some(name) = builder.next_pending() {
        if PackageName(name.clone()).is_platform() {
            continue;
        }

        // Skip packages already provided by VCS or `type: composer` repos
        // (those still get eager-loaded above). Inline `type: package`
        // matches are loaded on demand by name, mirroring Composer's
        // ArrayRepository semantics.
        if vcs_package_names.contains(&name) || composer_repo_names.contains(&name) {
            continue;
        }
        if add_inline_for(name.as_str(), &mut builder) {
            continue;
        }

        let queries = [PackageQuery {
            name: name.as_str(),
            constraint: None,
        }];
        let results = match repo_set.load_packages(&queries).await {
            Ok(v) => v,
            Err(_) => {
                // Virtual/meta packages (e.g. "psr/http-client-implementation")
                // don't exist on Packagist. They are resolved via provides/replaces
                // from other packages already in the pool.
                continue;
            }
        };
        for r in &results {
            if request.block_abandoned && is_abandoned(&r.version) {
                continue;
            }
            let inputs = packagist_to_pool_inputs(
                &r.name,
                &r.version,
                request.minimum_stability,
                &request.stability_flags,
            );
            for input in inputs {
                if !lock_filter_allows(&input.name, &input.version) {
                    continue;
                }
                builder.add_package(input);
            }
        }
    }

    // Second pass: materialize root aliases (`require: "X as Y"`).
    //
    // Mirrors Composer's `PoolBuilder::loadPackage` post-load step: when a
    // package whose `(name, version)` matches a `rootAliases` entry is added,
    // an extra `AliasPackage` exposing that install under
    // `(alias_normalized, alias)` is appended to the pool. When the matched
    // input is already an alias (e.g. an `extra.branch-alias` entry from
    // `packagist_to_pool_inputs`), Composer follows `getAliasOf()` to the
    // base package — we replicate by carrying the input's `is_alias_of`
    // value forward, so the new alias points straight at the real package
    // rather than chaining through the intermediate alias.
    if !root_aliases.is_empty() {
        let mut new_aliases: Vec<PoolPackageInput> = Vec::new();
        for input in builder.inputs() {
            // Skip alias creation for packages locked to their lock-file
            // version (partial update where this package wasn't requested).
            // Mirrors Composer's `propagateUpdate=false` skip in
            // `PoolBuilder::loadPackage`.
            if request
                .locked_package_names
                .contains(&input.name.to_lowercase())
            {
                continue;
            }
            for alias in &root_aliases {
                if input.name.to_lowercase() != alias.package {
                    continue;
                }
                if input.version != alias.version_normalized {
                    continue;
                }
                let target_normalized = input
                    .is_alias_of
                    .clone()
                    .unwrap_or_else(|| input.version.clone());
                new_aliases.push(PoolPackageInput {
                    name: input.name.clone(),
                    version: alias.alias_normalized.clone(),
                    pretty_version: alias.alias.clone(),
                    requires: input.requires.clone(),
                    replaces: input.replaces.clone(),
                    provides: input.provides.clone(),
                    conflicts: input.conflicts.clone(),
                    is_fixed: false,
                    is_alias_of: Some(target_normalized),
                });
            }
        }
        for alias_input in new_aliases {
            builder.add_package(alias_input);
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
    generator.set_ignore_all_platform_reqs(request.ignore_platform_reqs);
    let (rules, missing_root_requires) = generator.generate(
        &root_requires,
        &fixed_ids,
        &request.root_provide,
        &request.root_replace,
    );

    // Mirror Composer's `Solver::checkForRootRequireProblems`: a root require
    // with no providers in the pool yields no SAT rule, so the solver would
    // succeed with an empty plan. Surface it as an unresolvable problem
    // instead, matching Composer's exit code 2 behaviour.
    if !missing_root_requires.is_empty() {
        let problems: Vec<String> = missing_root_requires
            .iter()
            .map(|(name, constraint)| match constraint.as_deref() {
                Some(c) if !c.is_empty() => format!(
                    "    - Root composer.json requires {name} {c}, no matching package found."
                ),
                _ => {
                    format!("    - Root composer.json requires {name}, no matching package found.")
                }
            })
            .collect();
        let report = problems
            .into_iter()
            .enumerate()
            .map(|(i, msg)| format!("  Problem {}\n{}", i + 1, msg))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ResolveError::NoSolution(report));
    }

    // Create policy and solve. When `preferred_versions` is non-empty (the
    // `--minimal-changes` flow) feed it through the policy so the locked
    // version wins over the regular highest/lowest pick whenever a candidate
    // matches it. Mirrors Composer's
    // `Installer::createPolicy` minimal-update branch.
    let policy = if request.preferred_versions.is_empty() {
        DefaultPolicy::new(request.prefer_stable, request.prefer_lowest)
    } else {
        DefaultPolicy::with_preferred(
            request.prefer_stable,
            request.prefer_lowest,
            request.preferred_versions.clone(),
        )
    };
    let fixed_set: IndexSet<u32> = fixed_ids.into_iter().collect();
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

                // Skip the root package itself. It's in the pool as a fixed
                // entry only so transitive requires pointing back at it
                // can resolve; it must not appear in the lock file or
                // operations list. Mirrors Composer's `LockTransaction`
                // which discards fixed packages from the result.
                if !root_name_lower.is_empty() && pkg.name == root_name_lower {
                    continue;
                }

                let is_dev = if let Ok(v) = Version::parse(&pkg.version) {
                    version_stability(&v) == Stability::Dev
                } else {
                    false
                };

                let alias_of_normalized = pkg
                    .is_alias_of
                    .map(|tid| pool.package_by_id(tid).version.clone());

                resolved.push(ResolvedPackage {
                    name: pkg.name.clone(),
                    version: pkg.pretty_version.clone(),
                    version_normalized: pkg.version.clone(),
                    is_dev,
                    alias_of_normalized,
                });
            }
            Ok(resolved)
        }
        Err(e) => Err(ResolveError::NoSolution(e.to_string())),
    }
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

        let flags = IndexMap::new();

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

        let flags = IndexMap::new();

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
        let flags = IndexMap::new();
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
                    is_alias_of: None,
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
                    is_alias_of: None,
                },
            ],
            vec![],
        );

        let mut requires = IndexMap::new();
        requires.insert("foo/foo".to_string(), Some("^1.0".to_string()));

        let generator = RuleSetGenerator::new(&mut pool);
        let (rules, _) = generator.generate(&requires, &[], &IndexMap::new(), &IndexMap::new());

        let policy = DefaultPolicy::default();
        let solver = Solver::new(rules, &pool, policy, IndexSet::new());
        let result = solver.solve().unwrap();

        // Should install foo/foo (id=1) and bar/bar (id=2)
        assert!(result.installed.contains(&1));
        assert!(result.installed.contains(&2));
    }

    // ──────────── End-to-end tests (require network, marked #[ignore]) ────────────

    #[tokio::test]
    #[ignore]
    async fn test_resolve_monolog_e2e() {
        use crate::cache::Cache;
        let request = ResolveRequest {
            root_name: String::new(),
            root_version: None,
            require: vec![("monolog/monolog".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: Stability::Stable,
            stability_flags: IndexMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repositories: Arc::new(RepositorySet::with_packagist(Cache::new(
                std::env::temp_dir().join("mozart-test-cache"),
                false,
            ))),
            temporary_constraints: IndexMap::new(),
            raw_repositories: vec![],
            root_provide: IndexMap::new(),
            root_replace: IndexMap::new(),
            root_conflict: IndexMap::new(),
            locked_package_names: IndexSet::new(),
            locked_packages: Vec::new(),
            block_abandoned: false,
            root_branch_alias: None,
            preferred_versions: IndexMap::new(),
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
