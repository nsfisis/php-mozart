use crate::packagist::{PackagistDist, PackagistSource, PackagistVersion};
use crate::repository::RepositorySet;
use crate::resolver::ResolvedPackage;
use indexmap::IndexMap;
use indexmap::IndexSet;
use mozart_core::package::{RawPackageData, to_json_pretty};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::Path;

fn default_stability() -> String {
    "stable".to_string()
}

fn default_empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Represents the content of a composer.lock file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    #[serde(rename = "_readme", default = "LockFile::default_readme")]
    pub readme: Vec<String>,

    /// Composer lock files written before content-hash existed (or fixtures
    /// covering BC behavior) may omit this field; mirror Composer's BC support
    /// in `Locker::isLocked()` by defaulting to empty.
    #[serde(rename = "content-hash", default)]
    pub content_hash: String,

    pub packages: Vec<LockedPackage>,

    #[serde(rename = "packages-dev")]
    pub packages_dev: Option<Vec<LockedPackage>>,

    #[serde(default)]
    pub aliases: Vec<LockAlias>,

    #[serde(rename = "minimum-stability", default = "default_stability")]
    pub minimum_stability: String,

    #[serde(rename = "stability-flags", default = "default_empty_object")]
    pub stability_flags: serde_json::Value,

    #[serde(rename = "prefer-stable", default)]
    pub prefer_stable: bool,

    #[serde(rename = "prefer-lowest", default)]
    pub prefer_lowest: bool,

    #[serde(default = "default_empty_object")]
    pub platform: serde_json::Value,

    #[serde(rename = "platform-dev", default = "default_empty_object")]
    pub platform_dev: serde_json::Value,

    #[serde(rename = "plugin-api-version", skip_serializing_if = "Option::is_none")]
    pub plugin_api_version: Option<String>,
}

/// A locked package entry in composer.lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,

    #[serde(rename = "version_normalized", skip_serializing_if = "Option::is_none")]
    pub version_normalized: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<LockedSource>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dist: Option<LockedDist>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub require: BTreeMap<String, String>,

    #[serde(
        rename = "require-dev",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub require_dev: BTreeMap<String, String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub conflict: BTreeMap<String, String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provide: BTreeMap<String, String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub replace: BTreeMap<String, String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggest: Option<BTreeMap<String, String>>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub package_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoload: Option<serde_json::Value>,

    #[serde(rename = "autoload-dev", skip_serializing_if = "Option::is_none")]
    pub autoload_dev: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub support: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding: Option<Vec<serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,

    /// Catch-all for extra fields we don't explicitly model
    #[serde(flatten)]
    pub extra_fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub url: String,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedDist {
    #[serde(rename = "type")]
    pub dist_type: String,
    pub url: String,
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shasum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockAlias {
    pub package: String,
    pub version: String,
    pub alias: String,
    pub alias_normalized: String,
}

impl LockFile {
    /// Create default readme entries.
    pub fn default_readme() -> Vec<String> {
        vec![
            "This file locks the dependencies of your project to a known state".to_string(),
            "Read more about it at https://getcomposer.org/doc/01-basic-usage.md#installing-dependencies".to_string(),
            "This file is @generated automatically".to_string(),
        ]
    }

    /// Read a composer.lock file from disk.
    pub fn read_from_file(path: &Path) -> anyhow::Result<LockFile> {
        let content = fs::read_to_string(path)?;
        let lock: LockFile = serde_json::from_str(&content)?;
        Ok(lock)
    }

    /// Write a composer.lock file to disk with deterministic formatting.
    pub fn write_to_file(&self, path: &Path) -> anyhow::Result<()> {
        let json = to_json_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Check if the lock file is fresh (content-hash matches composer.json).
    pub fn is_fresh(&self, composer_json_content: &str) -> bool {
        match Self::compute_content_hash(composer_json_content) {
            Ok(hash) => hash == self.content_hash,
            Err(_) => false,
        }
    }

    /// Compute the content hash from composer.json content.
    /// Matches Composer's `Locker::getContentHash()`.
    pub fn compute_content_hash(composer_json_content: &str) -> anyhow::Result<String> {
        let value: serde_json::Value = serde_json::from_str(composer_json_content)?;
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("composer.json must be a JSON object"))?;

        // Keys that affect the content hash (Composer's relevantKeys)
        let relevant_keys = [
            "name",
            "version",
            "require",
            "require-dev",
            "conflict",
            "replace",
            "provide",
            "minimum-stability",
            "prefer-stable",
            "repositories",
            "extra",
        ];

        // Collect relevant keys into a BTreeMap (auto-sorted by key)
        let mut filtered: BTreeMap<&str, &serde_json::Value> = BTreeMap::new();
        for key in &relevant_keys {
            if let Some(v) = obj.get(*key) {
                filtered.insert(key, v);
            }
        }

        // Also include config.platform if present
        if let Some(config) = obj.get("config")
            && let Some(platform) = config.get("platform")
        {
            filtered.insert("config.platform", platform);
        }

        // Encode to compact JSON
        let compact = serde_json::to_string(&filtered)?;

        // Compute MD5
        let digest = md5::compute(compact.as_bytes());
        Ok(format!("{:x}", digest))
    }

    /// Check that every root `require` (and `require-dev` when `include_dev`)
    /// is satisfied by the locked packages. Returns the list of bullet-prefixed
    /// error lines (plus the trailing merge-conflict hint) if anything is
    /// missing or mismatched, otherwise an empty vec.
    ///
    /// Mirrors `Composer\Package\Locker::getMissingRequirementInfo()`.
    pub fn get_missing_requirement_info(
        &self,
        root: &mozart_core::package::RawPackageData,
        include_dev: bool,
    ) -> Vec<String> {
        let mut messages = Vec::new();
        let mut any_missing = false;

        let base_pool: Vec<LockedSearchEntry> = self
            .packages
            .iter()
            .map(|p| LockedSearchEntry::build(p, &self.aliases))
            .collect();
        let mut dev_pool: Vec<LockedSearchEntry> = base_pool.clone();
        if let Some(dev) = &self.packages_dev {
            dev_pool.extend(
                dev.iter()
                    .map(|p| LockedSearchEntry::build(p, &self.aliases)),
            );
        }

        check_requirement_set(
            &root.require,
            "Required",
            &base_pool,
            &mut messages,
            &mut any_missing,
        );
        if include_dev {
            check_requirement_set(
                &root.require_dev,
                "Required (in require-dev)",
                &dev_pool,
                &mut messages,
                &mut any_missing,
            );
        }

        if any_missing {
            messages.push(
                "This usually happens when composer files are incorrectly merged or the composer.json file is manually edited.".to_string(),
            );
            messages.push(
                "Read more about correctly resolving merge conflicts https://getcomposer.org/doc/articles/resolving-merge-conflicts.md".to_string(),
            );
            messages.push(
                "and prefer using the \"require\" command over editing the composer.json file directly https://getcomposer.org/doc/03-cli.md#require-r".to_string(),
            );
        }

        messages
    }
}

/// A locked package paired with the additional version strings the locked
/// repository would surface for it (branch-alias targets + matching root
/// aliases from `lock.aliases`).
///
/// Mirrors the AliasPackage entries that `Composer\Package\Locker::getLockedRepository`
/// adds alongside each locked package, so requirement checks see the same
/// version surface Composer does.
#[derive(Clone)]
struct LockedSearchEntry<'a> {
    package: &'a LockedPackage,
    alias_versions: Vec<String>,
}

impl<'a> LockedSearchEntry<'a> {
    fn build(package: &'a LockedPackage, root_aliases: &[LockAlias]) -> Self {
        let mut alias_versions: Vec<String> = locked_package_branch_aliases(package)
            .into_iter()
            .map(|a| a.alias_normalized)
            .collect();
        for alias in root_aliases {
            if alias.package.eq_ignore_ascii_case(&package.name)
                && alias.version.eq_ignore_ascii_case(&package.version)
            {
                alias_versions.push(alias.alias_normalized.clone());
            }
        }
        Self {
            package,
            alias_versions,
        }
    }
}

/// Build the synthetic `LockAlias` entries a `dev-*` locked package contributes
/// via `extra.branch-alias`. Mirrors `Composer\Package\Loader\ArrayLoader::getBranchAlias`
/// followed by `VersionParser::normalizeBranch` — the same expansion
/// `Locker::getLockedRepository` performs when constructing AliasPackages
/// alongside each locked package.
pub fn locked_package_branch_aliases(pkg: &LockedPackage) -> Vec<LockAlias> {
    let pkg_version_lower = pkg.version.to_lowercase();
    let is_dev_branch =
        pkg_version_lower.starts_with("dev-") || pkg_version_lower.ends_with("-dev");
    if !is_dev_branch {
        return Vec::new();
    }
    let Some(extra) = pkg.extra_fields.get("extra") else {
        return Vec::new();
    };
    let Some(branch_alias) = extra.get("branch-alias") else {
        return Vec::new();
    };
    let Some(map) = branch_alias.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (source, target) in map.iter() {
        if !source.eq_ignore_ascii_case(&pkg.version) {
            continue;
        }
        let Some(target_str) = target.as_str() else {
            continue;
        };
        if !target_str.to_lowercase().ends_with("-dev") {
            continue;
        }
        let Some(normalized) = crate::resolver::normalize_branch_alias_target(target_str) else {
            continue;
        };
        // Pretty-form trim: Composer's `Preg::replace('{(\.9{7})+}', '.x', ...)`
        // turns the normalized form back into the wildcard form (e.g.
        // `2.1.9999999.9999999-dev` → `2.1.x-dev`). For trace output we want
        // the raw alias target string the package author wrote.
        out.push(LockAlias {
            package: pkg.name.clone(),
            version: pkg.version.clone(),
            alias: target_str.to_string(),
            alias_normalized: normalized,
        });
    }
    out
}

fn check_requirement_set(
    requires: &BTreeMap<String, String>,
    description: &str,
    pool: &[LockedSearchEntry],
    messages: &mut Vec<String>,
    any_missing: &mut bool,
) {
    for (name, constraint_str) in requires {
        if mozart_core::platform::is_platform_package(name) {
            continue;
        }
        if constraint_str.trim() == "self.version" {
            continue;
        }

        let constraint = mozart_semver::VersionConstraint::parse(constraint_str).ok();

        let mut name_only_match: Option<&LockedPackage> = None;
        let mut satisfied = false;
        for entry in pool {
            let pkg = entry.package;
            if pkg.name != *name {
                continue;
            }
            if name_only_match.is_none() {
                name_only_match = Some(pkg);
            }
            let Some(ref c) = constraint else { continue };
            if let Ok(version) = mozart_semver::Version::parse(&pkg.version)
                && c.matches(&version)
            {
                satisfied = true;
                break;
            }
            if entry.alias_versions.iter().any(|alias| {
                mozart_semver::Version::parse(alias)
                    .ok()
                    .is_some_and(|v| c.matches(&v))
            }) {
                satisfied = true;
                break;
            }
        }

        if satisfied {
            continue;
        }

        *any_missing = true;
        if let Some(pkg) = name_only_match {
            messages.push(format!(
                "- {description} package \"{name}\" is in the lock file as \"{}\" but that does not satisfy your constraint \"{constraint_str}\".",
                pkg.version
            ));
        } else {
            messages.push(format!(
                "- {description} package \"{name}\" is not present in the lock file."
            ));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lock file generation
// ─────────────────────────────────────────────────────────────────────────────

/// Input for lock file generation.
pub struct LockFileGenerationRequest {
    /// Resolved packages from the dependency resolver.
    pub resolved_packages: Vec<ResolvedPackage>,
    /// Raw composer.json content string (for content-hash computation).
    pub composer_json_content: String,
    /// Parsed composer.json data (for platform, minimum-stability, etc.).
    pub composer_json: RawPackageData,
    /// Whether require-dev was included in resolution.
    pub include_dev: bool,
    /// Repository set used to fetch full metadata for resolved packages
    /// that aren't already covered by inline `type: package` repositories.
    pub repositories: std::sync::Arc<RepositorySet>,
    /// Previous `composer.lock` (when running update / require / remove).
    /// For each resolved package whose name+normalized-version matches an
    /// entry in this lock, the entry is copied into the new lock verbatim
    /// rather than being re-fetched from the inline / composer-repo /
    /// Packagist sources. Mirrors Composer's `Locker::setLockData` behaviour
    /// during partial updates: lock entries are stable across updates that
    /// don't touch the package, even if the upstream metadata has drifted.
    pub previous_lock: Option<LockFile>,
}

impl LockFileGenerationRequest {
    /// Look up an inline `type: package` definition for `name` (if any).
    /// Returns the matching `PackagistVersion` so callers can short-circuit
    /// the Packagist fetch for resolved packages that came from a `type:
    /// package` repository.
    fn inline_lookup(&self, name: &str, version_normalized: &str) -> Option<PackagistVersion> {
        crate::inline_package::collect_inline_packages(&self.composer_json.repositories)
            .into_iter()
            .find(|ipkg| ipkg.name == name && ipkg.version.version_normalized == version_normalized)
            .map(|ipkg| ipkg.version)
    }

    /// Look up a `type: composer` repository entry for `name@version_normalized`.
    /// Used to short-circuit the Packagist fetch when the resolved package came
    /// from a local Composer repo (the test fixtures' file:// case).
    fn composer_repo_lookup(
        &self,
        name: &str,
        version_normalized: &str,
    ) -> Option<PackagistVersion> {
        crate::composer_repo::collect_composer_packages(&self.composer_json.repositories)
            .into_iter()
            .find(|cpkg| cpkg.name == name && cpkg.version.version_normalized == version_normalized)
            .map(|cpkg| cpkg.version)
    }
}

/// Convert a `PackagistSource` to a `LockedSource`.
fn packagist_source_to_locked(ps: &PackagistSource) -> LockedSource {
    LockedSource {
        source_type: ps.source_type.clone(),
        url: ps.url.clone(),
        reference: ps.reference.clone(),
    }
}

/// Convert a `PackagistDist` to a `LockedDist`.
fn packagist_dist_to_locked(pd: &PackagistDist) -> LockedDist {
    LockedDist {
        dist_type: pd.dist_type.clone(),
        url: pd.url.clone(),
        reference: pd.reference.clone(),
        shasum: pd.shasum.clone(),
    }
}

/// Mirror Composer's `RootPackageLoader::extractReferences`: scan
/// `require`/`require-dev` for `dev-foo#hex` style constraints, returning a
/// lowercase package name → reference map. Constraints whose stability isn't
/// `dev` after stripping the reference are left out (matching the
/// `'dev' === VersionParser::parseStability(...)` guard in PHP).
fn extract_root_references(
    require: &BTreeMap<String, String>,
    require_dev: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, raw_constraint) in require.iter().chain(require_dev.iter()) {
        if let Some(reference) = parse_inline_reference(raw_constraint) {
            out.insert(name.to_lowercase(), reference);
        }
    }
    out
}

/// Pull the `#hex` suffix out of a single-atom dev constraint. Returns
/// `None` for non-`dev-*` / non-`*-dev` constraints, matching Composer's
/// `'{^[^,\s@]+?#([a-f0-9]+)$}'` + `parseStability == 'dev'` guard.
fn parse_inline_reference(constraint: &str) -> Option<String> {
    // Strip `... as alias` first, mirroring extractReferences's
    // `'{^([^,\s@]+) as .+$}'` replacement.
    let core = match constraint.split(" as ").next() {
        Some(c) => c.trim(),
        None => constraint.trim(),
    };
    let (head, hash) = core.rsplit_once('#')?;
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if head.contains([' ', '\t', ',', '@']) {
        return None;
    }
    let lower = head.to_lowercase();
    if !(lower.starts_with("dev-") || lower.ends_with("-dev")) {
        return None;
    }
    Some(hash.to_string())
}

/// Mirror `Composer\Package\Package::setSourceDistReferences`: rewrite both
/// source and dist references to the supplied value, and rewrite the
/// reference inside any auto-generated GitHub/GitLab/Bitbucket dist URL when
/// present. The dist reference is only written if there was already one
/// (Composer leaves `dist.reference == null` packages alone).
fn apply_reference_override(pkg: &mut LockedPackage, reference: &str) {
    if let Some(source) = pkg.source.as_mut() {
        source.reference = Some(reference.to_string());
    }
    if let Some(dist) = pkg.dist.as_mut() {
        let url_carries_known_host = matches_dist_url_with_known_host(Some(&dist.url));
        if dist.reference.is_some() || url_carries_known_host {
            dist.reference = Some(reference.to_string());
        }
        if url_carries_known_host {
            dist.url = rewrite_known_dist_url_reference(&dist.url, reference);
        }
    }
}

/// Match the bitbucket / github / gitlab dist-URL prefixes Composer
/// rewrites. Mirrors the regex
/// `{^https?://(?:(?:www\.)?bitbucket\.org|(api\.)?github\.com|(?:www\.)?gitlab\.com)/}i`.
fn matches_dist_url_with_known_host(url: Option<&str>) -> bool {
    let Some(url) = url else { return false };
    let lower = url.to_lowercase();
    let stripped = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
        .unwrap_or(&lower);
    let stripped = stripped.strip_prefix("www.").unwrap_or(stripped);
    let stripped = stripped.strip_prefix("api.").unwrap_or(stripped);
    stripped.starts_with("bitbucket.org/")
        || stripped.starts_with("github.com/")
        || stripped.starts_with("gitlab.com/")
}

/// Substitute any 40-char hex segment surrounded by `/` or `sha=` (the
/// archive shape produced by GitHub/GitLab/Bitbucket) with the new
/// reference. Matches Composer's
/// `'{(?<=/|sha=)[a-f0-9]{40}(?=/|$)}i'` rewrite.
fn rewrite_known_dist_url_reference(url: &str, reference: &str) -> String {
    let bytes = url.as_bytes();
    let mut out = String::with_capacity(url.len());
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let preceded_by_slash = i > 0 && bytes[i - 1] == b'/';
        let preceded_by_sha = i >= 4 && &bytes[i - 4..i] == b"sha=";
        if (preceded_by_slash || preceded_by_sha) && i + 40 <= bytes.len() {
            let candidate = &url[i..i + 40];
            if candidate.chars().all(|c| c.is_ascii_hexdigit()) {
                let after = bytes.get(i + 40).copied();
                if after == Some(b'/') || after.is_none() {
                    out.push_str(reference);
                    i += 40;
                    continue;
                }
            }
        }
        out.push(url[start..].chars().next().unwrap());
        i += url[start..].chars().next().unwrap().len_utf8();
    }
    out
}

/// Convert a `PackagistVersion` to a `LockedPackage`.
fn packagist_version_to_locked_package(name: &str, pv: &PackagistVersion) -> LockedPackage {
    let mut extra_fields: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    if let Some(extra) = &pv.extra {
        extra_fields.insert("extra".to_string(), extra.clone());
    }
    if let Some(notification_url) = &pv.notification_url {
        extra_fields.insert(
            "notification-url".to_string(),
            serde_json::Value::String(notification_url.clone()),
        );
    }
    // Propagate `abandoned` so the lock (and downstream installed.json
    // round-trip) preserves the package's deprecation state. Mirrors
    // Composer's `ArrayDumper::dump`, which emits the field when truthy
    // (`true` for "abandoned, no replacement", a string for "abandoned,
    // use this instead"). `false`/null collapse to "not abandoned" and
    // are dropped.
    if let Some(abandoned) = &pv.abandoned {
        let keep = match abandoned {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::String(s) => !s.is_empty(),
            serde_json::Value::Null => false,
            _ => true,
        };
        if keep {
            extra_fields.insert("abandoned".to_string(), abandoned.clone());
        }
    }

    LockedPackage {
        name: name.to_string(),
        version: pv.version.clone(),
        version_normalized: Some(pv.version_normalized.clone()),
        source: pv.source.as_ref().map(packagist_source_to_locked),
        dist: pv.dist.as_ref().map(packagist_dist_to_locked),
        require: pv.require.clone(),
        require_dev: pv.require_dev.clone(),
        conflict: pv.conflict.clone(),
        provide: pv.provide.clone(),
        replace: pv.replace.clone(),
        suggest: pv.suggest.clone(),
        package_type: pv.package_type.clone(),
        autoload: pv.autoload.clone(),
        autoload_dev: pv.autoload_dev.clone(),
        license: pv.license.clone(),
        description: pv.description.clone(),
        homepage: pv.homepage.clone(),
        keywords: pv.keywords.clone(),
        authors: pv.authors.clone(),
        support: pv.support.clone(),
        funding: pv.funding.clone(),
        time: pv.time.clone(),
        extra_fields,
    }
}

/// Determine which resolved packages are dev-only.
///
/// A package is dev-only if it is NOT reachable from the non-dev dependency tree
/// (i.e., only reachable through require-dev paths).
///
/// `requires_by_name` and `providers_by_name` are keyed by lowercase package
/// names. `providers_by_name` maps a satisfied name (own name + each `provide`
/// or `replace` target) to the list of resolved package names that satisfy it,
/// so a non-dev `require` like `provided/pkg` reaches `b/b` when `b/b`
/// declares `provide: { provided/pkg: 1.0.0 }`.
fn classify_dev_packages(
    resolved: &[ResolvedPackage],
    require: &BTreeMap<String, String>,
    _require_dev: &BTreeMap<String, String>,
    requires_by_name: &IndexMap<String, Vec<String>>,
    providers_by_name: &IndexMap<String, Vec<String>>,
) -> IndexSet<String> {
    // BFS from non-dev root dependencies through each package's `require` map.
    // All reachable packages are production packages.
    let mut production: IndexSet<String> = IndexSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    let visit = |name: &str, production: &mut IndexSet<String>, queue: &mut VecDeque<String>| {
        let name_lower = name.to_lowercase();
        if is_platform_name(&name_lower) {
            return;
        }
        // A required name is satisfied either by a resolved package whose own
        // name matches (the common case, captured here as `providers_by_name`
        // also indexes own names) or by a resolved package that provides /
        // replaces it. Mirrors Composer's `extractDevPackages` second-solve
        // semantics, which walks the same provide/replace edges through a
        // real Solver call.
        if let Some(provs) = providers_by_name.get(&name_lower) {
            for prov in provs {
                let prov_lower = prov.to_lowercase();
                if production.insert(prov_lower.clone()) {
                    queue.push_back(prov_lower);
                }
            }
        }
    };

    for name in require.keys() {
        visit(name, &mut production, &mut queue);
    }

    while let Some(pkg_name) = queue.pop_front() {
        if let Some(deps) = requires_by_name.get(&pkg_name) {
            for dep_name in deps.clone() {
                visit(&dep_name, &mut production, &mut queue);
            }
        }
    }

    // Any resolved package not in `production` is dev-only
    resolved
        .iter()
        .filter(|p| !production.contains(&p.name.to_lowercase()))
        .map(|p| p.name.clone())
        .collect()
}

/// Returns true if the package name is a platform package (php, ext-*, lib-*, etc.).
fn is_platform_name(name: &str) -> bool {
    name == "php"
        || name.starts_with("ext-")
        || name.starts_with("lib-")
        || name == "php-64bit"
        || name == "php-ipv6"
        || name == "php-zts"
        || name == "php-debug"
}

/// Extract platform requirements from a requirements map.
///
/// Filters the map to include only platform package keys (`php`, `ext-*`, `lib-*`, etc.)
/// and returns them as a JSON object.
fn extract_platform_requirements(requirements: &BTreeMap<String, String>) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = requirements
        .iter()
        .filter(|(k, _)| is_platform_name(k))
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::Value::Object(map)
}

/// Generate a complete `LockFile` from resolution results.
///
/// This function:
/// 1. Fetches full metadata from Packagist for each resolved package
/// 2. Separates packages into production vs dev-only
/// 3. Computes the content-hash
/// 4. Assembles the complete `LockFile` struct
pub async fn generate_lock_file(request: &LockFileGenerationRequest) -> anyhow::Result<LockFile> {
    // Split the resolved set into real packages and alias entries up front.
    // Aliases get emitted as a separate `aliases[]` block and never enter the
    // metadata fetch loop — their target package carries the real metadata.
    let (real_resolved, alias_resolved): (Vec<&ResolvedPackage>, Vec<&ResolvedPackage>) = request
        .resolved_packages
        .iter()
        .partition(|p| p.alias_of_normalized.is_none());

    // 1. Fetch full metadata for real (non-alias) packages.
    //
    // Inline `type: package` repositories carry full metadata in composer.json
    // — short-circuit those before hitting the network. Everything else goes
    // through `RepositorySet`, which today contains only Packagist; future
    // steps will move VCS / inline through the same set.
    // Previous-lock relationship pass-through: when a resolved package
    // matches an entry in `previous_lock` at the same name +
    // version_normalized, capture the entry's relationship-shaped fields
    // (require / require-dev / conflict / replace / provide / suggest).
    // Composer's transaction calculates operation order using these
    // relationship fields off the locked repository, so a partial update
    // shouldn't refresh them from upstream metadata for packages that
    // didn't move — otherwise topological_sort sees a different graph
    // than Composer would.
    //
    // Source/dist references and version-shaped fields still come from
    // the freshly-fetched metadata, so dev packages whose ref bumped (the
    // resolver picked a new commit at the same version label) still get
    // their ref refreshed.
    struct PreservedRelationships {
        require: BTreeMap<String, String>,
        require_dev: BTreeMap<String, String>,
        conflict: BTreeMap<String, String>,
        provide: BTreeMap<String, String>,
        replace: BTreeMap<String, String>,
        suggest: Option<BTreeMap<String, String>>,
    }
    let mut preserved_rel: IndexMap<String, PreservedRelationships> = IndexMap::new();
    if let Some(prev) = &request.previous_lock {
        for prev_pkg in prev
            .packages
            .iter()
            .chain(prev.packages_dev.iter().flatten())
        {
            let prev_normalized = prev_pkg.version_normalized.clone().unwrap_or_else(|| {
                mozart_semver::Version::parse(&prev_pkg.version)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| prev_pkg.version.clone())
            });
            for pkg in &real_resolved {
                if pkg.name.eq_ignore_ascii_case(&prev_pkg.name)
                    && pkg.version_normalized == prev_normalized
                {
                    preserved_rel.insert(
                        pkg.name.clone(),
                        PreservedRelationships {
                            require: prev_pkg.require.clone(),
                            require_dev: prev_pkg.require_dev.clone(),
                            conflict: prev_pkg.conflict.clone(),
                            provide: prev_pkg.provide.clone(),
                            replace: prev_pkg.replace.clone(),
                            suggest: prev_pkg.suggest.clone(),
                        },
                    );
                }
            }
        }
    }

    let mut package_metadata: IndexMap<String, PackagistVersion> = IndexMap::new();
    let repo_set = &request.repositories;
    for pkg in &real_resolved {
        if let Some(inline) = request.inline_lookup(&pkg.name, &pkg.version_normalized) {
            package_metadata.insert(pkg.name.clone(), inline);
            continue;
        }

        if let Some(cv) = request.composer_repo_lookup(&pkg.name, &pkg.version_normalized) {
            package_metadata.insert(pkg.name.clone(), cv);
            continue;
        }

        let queries = [crate::repository::PackageQuery {
            name: pkg.name.as_str(),
            constraint: None,
        }];
        let results = repo_set.load_packages(&queries).await?;
        let matching = results
            .into_iter()
            .find(|r| r.version.version_normalized == pkg.version_normalized)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find version {} for package {} in Packagist response",
                    pkg.version_normalized,
                    pkg.name
                )
            })?;
        package_metadata.insert(pkg.name.clone(), matching.version);
    }

    // 2. Classify dev vs non-dev packages (real packages only).
    let real_owned: Vec<ResolvedPackage> = real_resolved
        .iter()
        .map(|p| ResolvedPackage {
            name: p.name.clone(),
            version: p.version.clone(),
            version_normalized: p.version_normalized.clone(),
            is_dev: p.is_dev,
            alias_of_normalized: None,
        })
        .collect();
    // Build the `name → require keys` view classify_dev_packages walks. Use
    // preserved-from-old-lock requires when available so a partial update
    // sees the same dev-classification graph the previous lock did.
    let mut requires_by_name: IndexMap<String, Vec<String>> = IndexMap::new();
    // Inverse map: `satisfied name → list of resolved packages that satisfy it`.
    // A resolved package satisfies its own name plus each `provide` / `replace`
    // target (Composer's `extractDevPackages` reaches the same edges through
    // its second Solver run; we walk them directly during the dev BFS).
    let mut providers_by_name: IndexMap<String, Vec<String>> = IndexMap::new();
    for (name, pv) in &package_metadata {
        let name_lower = name.to_lowercase();
        let (require_keys, provide_keys, replace_keys): (Vec<String>, Vec<String>, Vec<String>) =
            if let Some(rel) = preserved_rel.get(name) {
                (
                    rel.require.keys().cloned().collect(),
                    rel.provide.keys().cloned().collect(),
                    rel.replace.keys().cloned().collect(),
                )
            } else {
                (
                    pv.require.keys().cloned().collect(),
                    pv.provide.keys().cloned().collect(),
                    pv.replace.keys().cloned().collect(),
                )
            };
        requires_by_name.insert(name_lower.clone(), require_keys);
        providers_by_name
            .entry(name_lower.clone())
            .or_default()
            .push(name_lower.clone());
        for target in provide_keys.iter().chain(replace_keys.iter()) {
            providers_by_name
                .entry(target.to_lowercase())
                .or_default()
                .push(name_lower.clone());
        }
    }
    let dev_only = classify_dev_packages(
        &real_owned,
        &request.composer_json.require,
        &request.composer_json.require_dev,
        &requires_by_name,
        &providers_by_name,
    );

    // 3. Build LockedPackage lists.
    //
    // Apply root-level `#hex` reference overrides extracted from
    // `require`/`require-dev`. Mirrors Composer's
    // `RootPackageLoader::extractReferences` + `PoolBuilder::loadPackage`'s
    // `setSourceDistReferences` call: when the user pinned a dev package via
    // `dev-main#abcd123`, the resolved package's source/dist must show that
    // reference in the lock + trace, not whatever the inline metadata said.
    let root_references = extract_root_references(
        &request.composer_json.require,
        &request.composer_json.require_dev,
    );
    let mut packages: Vec<LockedPackage> = Vec::new();
    let mut packages_dev: Vec<LockedPackage> = Vec::new();
    for pkg in &real_resolved {
        let pv = &package_metadata[&pkg.name];
        let mut locked = packagist_version_to_locked_package(&pkg.name, pv);
        // Overlay relationship fields from the previous lock when applicable
        // — the resolver's transaction-time view came from the lock, so the
        // new lock should mirror those relationships even if the upstream
        // metadata has drifted.
        if let Some(rel) = preserved_rel.get(&pkg.name) {
            locked.require = rel.require.clone();
            locked.require_dev = rel.require_dev.clone();
            locked.conflict = rel.conflict.clone();
            locked.provide = rel.provide.clone();
            locked.replace = rel.replace.clone();
            locked.suggest = rel.suggest.clone();
        }
        if let Some(reference) = root_references.get(&pkg.name.to_lowercase()) {
            apply_reference_override(&mut locked, reference);
        }
        if dev_only.contains(&pkg.name) {
            packages_dev.push(locked);
        } else {
            packages.push(locked);
        }
    }

    // 4. Sort each list alphabetically by name (Composer does this)
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    packages_dev.sort_by(|a, b| a.name.cmp(&b.name));

    // 5. Build the aliases[] block. Each alias entry references the target
    // package (`package` + `version`) and carries the alias's pretty/normalized
    // form (`alias` + `alias_normalized`). Mirrors Composer's
    // `Locker::lockPackages` alias dump.
    let mut alias_blocks: Vec<LockAlias> = Vec::new();
    for alias in &alias_resolved {
        let target_normalized = match &alias.alias_of_normalized {
            Some(t) => t.clone(),
            None => continue,
        };
        let target_pretty = real_resolved
            .iter()
            .find(|p| p.name == alias.name && p.version_normalized == target_normalized)
            .map(|p| p.version.clone())
            .unwrap_or_else(|| target_normalized.clone());
        alias_blocks.push(LockAlias {
            package: alias.name.clone(),
            version: target_pretty,
            alias: alias.version.clone(),
            alias_normalized: alias.version_normalized.clone(),
        });
    }
    alias_blocks.sort_by(|a, b| a.package.cmp(&b.package).then(a.alias.cmp(&b.alias)));

    // 6. Compute content-hash
    let content_hash = LockFile::compute_content_hash(&request.composer_json_content)?;

    // 7. Extract platform requirements
    let platform = extract_platform_requirements(&request.composer_json.require);
    let platform_dev = extract_platform_requirements(&request.composer_json.require_dev);

    // 8. Determine minimum-stability and prefer-stable
    let minimum_stability = request
        .composer_json
        .minimum_stability
        .clone()
        .unwrap_or_else(|| "stable".to_string());

    let prefer_stable = request
        .composer_json
        .extra_fields
        .get("prefer-stable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 9. Assemble LockFile
    Ok(LockFile {
        readme: LockFile::default_readme(),
        content_hash,
        packages,
        packages_dev: if request.include_dev {
            Some(packages_dev)
        } else {
            Some(vec![])
        },
        aliases: alias_blocks,
        minimum_stability,
        stability_flags: serde_json::json!({}),
        prefer_stable,
        prefer_lowest: false,
        platform,
        platform_dev,
        plugin_api_version: Some("2.6.0".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn minimal_lock() -> LockFile {
        LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages: vec![],
            packages_dev: Some(vec![]),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        }
    }

    #[test]
    fn test_roundtrip_minimal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("composer.lock");

        let lock = minimal_lock();
        lock.write_to_file(&path).unwrap();

        let loaded = LockFile::read_from_file(&path).unwrap();
        assert_eq!(loaded.content_hash, "abc123");
        assert_eq!(loaded.minimum_stability, "stable");
        assert!(!loaded.prefer_stable);
        assert_eq!(loaded.packages.len(), 0);
    }

    #[test]
    fn test_roundtrip_with_package() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("composer.lock");

        let mut lock = minimal_lock();
        lock.packages.push(LockedPackage {
            name: "monolog/monolog".to_string(),
            version: "3.8.0".to_string(),
            version_normalized: None,
            source: None,
            dist: Some(LockedDist {
                dist_type: "zip".to_string(),
                url: "https://example.com/monolog.zip".to_string(),
                reference: Some("abc123".to_string()),
                shasum: Some("".to_string()),
            }),
            require: BTreeMap::new(),
            require_dev: BTreeMap::new(),
            conflict: BTreeMap::new(),
            provide: BTreeMap::new(),
            replace: BTreeMap::new(),
            suggest: None,
            package_type: Some("library".to_string()),
            autoload: None,
            autoload_dev: None,
            license: Some(vec!["MIT".to_string()]),
            description: Some("A logging library".to_string()),
            homepage: None,
            keywords: None,
            authors: None,
            support: None,
            funding: None,
            time: None,
            extra_fields: BTreeMap::new(),
        });

        lock.write_to_file(&path).unwrap();
        let loaded = LockFile::read_from_file(&path).unwrap();

        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0].name, "monolog/monolog");
        assert_eq!(loaded.packages[0].version, "3.8.0");
        assert_eq!(
            loaded.packages[0].description.as_deref(),
            Some("A logging library")
        );
    }

    #[test]
    fn test_content_hash_deterministic() {
        let composer_json = r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#;
        let h1 = LockFile::compute_content_hash(composer_json).unwrap();
        let h2 = LockFile::compute_content_hash(composer_json).unwrap();
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn test_content_hash_changes_on_require_change() {
        let composer1 = r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#;
        let composer2 = r#"{"name": "test/project", "require": {"monolog/monolog": "^2.0"}}"#;
        let h1 = LockFile::compute_content_hash(composer1).unwrap();
        let h2 = LockFile::compute_content_hash(composer2).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_is_fresh() {
        let composer_json = r#"{"name": "test/project", "require": {"php": ">=8.1"}}"#;
        let hash = LockFile::compute_content_hash(composer_json).unwrap();

        let mut lock = minimal_lock();
        lock.content_hash = hash;

        assert!(lock.is_fresh(composer_json));
        assert!(!lock.is_fresh(r#"{"name": "test/project", "require": {"php": ">=8.0"}}"#));
    }

    #[test]
    fn test_default_readme() {
        let readme = LockFile::default_readme();
        assert_eq!(readme.len(), 3);
        assert!(readme[0].contains("locks the dependencies"));
    }

    #[test]
    fn parses_lock_without_content_hash() {
        // Composer fixtures (and historical lock files) may omit content-hash;
        // mirror Composer's BC handling by accepting it and treating the lock
        // as not-fresh against any composer.json.
        let raw = r#"{
            "packages": [],
            "packages-dev": [],
            "aliases": [],
            "minimum-stability": "dev",
            "stability-flags": {},
            "prefer-stable": false,
            "prefer-lowest": false
        }"#;
        let lock: LockFile = serde_json::from_str(raw).unwrap();
        assert_eq!(lock.content_hash, "");
        assert!(!lock.is_fresh(r#"{"require": {}}"#));
    }

    // ──────────── Lock file generation tests ────────────

    fn make_packagist_version(
        version: &str,
        version_normalized: &str,
        require: BTreeMap<String, String>,
    ) -> PackagistVersion {
        PackagistVersion {
            version: version.to_string(),
            version_normalized: version_normalized.to_string(),
            require,
            replace: BTreeMap::new(),
            provide: BTreeMap::new(),
            conflict: BTreeMap::new(),
            dist: Some(crate::packagist::PackagistDist {
                dist_type: "zip".to_string(),
                url: format!("https://example.com/{version}.zip"),
                reference: Some("deadbeef".to_string()),
                shasum: Some("abc123".to_string()),
            }),
            source: Some(crate::packagist::PackagistSource {
                source_type: "git".to_string(),
                url: "https://github.com/example/pkg.git".to_string(),
                reference: Some("deadbeef".to_string()),
            }),
            require_dev: BTreeMap::new(),
            suggest: None,
            package_type: Some("library".to_string()),
            autoload: Some(serde_json::json!({"psr-4": {"Example\\": "src/"}})),
            autoload_dev: None,
            license: Some(vec!["MIT".to_string()]),
            description: Some("An example package".to_string()),
            homepage: Some("https://example.com".to_string()),
            keywords: Some(vec!["example".to_string(), "test".to_string()]),
            authors: Some(vec![
                serde_json::json!({"name": "Alice", "email": "alice@example.com"}),
            ]),
            support: Some(serde_json::json!({"issues": "https://github.com/example/pkg/issues"})),
            funding: Some(vec![
                serde_json::json!({"type": "github", "url": "https://github.com/sponsors/alice"}),
            ]),
            time: Some("2024-01-15T10:00:00+00:00".to_string()),
            extra: Some(serde_json::json!({"branch-alias": {"dev-main": "1.0.x-dev"}})),
            notification_url: Some("https://packagist.org/downloads/".to_string()),
            default_branch: false,
            abandoned: None,
        }
    }

    #[test]
    fn test_packagist_version_to_locked_package() {
        let pv = make_packagist_version("1.2.3", "1.2.3.0", BTreeMap::new());
        let locked = packagist_version_to_locked_package("example/pkg", &pv);

        assert_eq!(locked.name, "example/pkg");
        assert_eq!(locked.version, "1.2.3");
        assert_eq!(locked.version_normalized.as_deref(), Some("1.2.3.0"));
        assert_eq!(locked.description.as_deref(), Some("An example package"));
        assert_eq!(locked.homepage.as_deref(), Some("https://example.com"));
        assert_eq!(
            locked.license.as_deref(),
            Some(vec!["MIT".to_string()].as_slice())
        );
        assert_eq!(
            locked.keywords.as_deref(),
            Some(["example".to_string(), "test".to_string()].as_slice())
        );
        assert_eq!(locked.package_type.as_deref(), Some("library"));
        assert!(locked.autoload.is_some());
        assert!(locked.authors.is_some());
        assert!(locked.support.is_some());
        assert!(locked.funding.is_some());
        assert_eq!(locked.time.as_deref(), Some("2024-01-15T10:00:00+00:00"));

        // Check dist
        let dist = locked.dist.as_ref().unwrap();
        assert_eq!(dist.dist_type, "zip");
        assert_eq!(dist.reference.as_deref(), Some("deadbeef"));
        assert_eq!(dist.shasum.as_deref(), Some("abc123"));

        // Check source
        let source = locked.source.as_ref().unwrap();
        assert_eq!(source.source_type, "git");
        assert_eq!(source.reference.as_deref(), Some("deadbeef"));

        // Check extra_fields (extra and notification-url)
        assert!(locked.extra_fields.contains_key("extra"));
        assert!(locked.extra_fields.contains_key("notification-url"));
        assert_eq!(
            locked.extra_fields["notification-url"],
            serde_json::Value::String("https://packagist.org/downloads/".to_string())
        );
    }

    #[test]
    fn test_packagist_version_to_locked_package_no_optional_fields() {
        let pv = PackagistVersion {
            version: "1.0.0".to_string(),
            version_normalized: "1.0.0.0".to_string(),
            require: BTreeMap::new(),
            replace: BTreeMap::new(),
            provide: BTreeMap::new(),
            conflict: BTreeMap::new(),
            dist: None,
            source: None,
            require_dev: BTreeMap::new(),
            suggest: None,
            package_type: None,
            autoload: None,
            autoload_dev: None,
            license: None,
            description: None,
            homepage: None,
            keywords: None,
            authors: None,
            support: None,
            funding: None,
            time: None,
            extra: None,
            notification_url: None,
            default_branch: false,
            abandoned: None,
        };

        let locked = packagist_version_to_locked_package("vendor/pkg", &pv);
        assert_eq!(locked.name, "vendor/pkg");
        assert!(locked.dist.is_none());
        assert!(locked.source.is_none());
        assert!(locked.description.is_none());
        assert!(locked.license.is_none());
        assert!(locked.extra_fields.is_empty());
    }

    #[test]
    fn test_classify_dev_packages_simple() {
        // Root: require={A}, require-dev={B}
        // A depends on C; B depends on D
        // Expected dev-only: {B, D}
        let resolved = vec![
            ResolvedPackage {
                name: "vendor/a".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
                alias_of_normalized: None,
            },
            ResolvedPackage {
                name: "vendor/b".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
                alias_of_normalized: None,
            },
            ResolvedPackage {
                name: "vendor/c".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
                alias_of_normalized: None,
            },
            ResolvedPackage {
                name: "vendor/d".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
                alias_of_normalized: None,
            },
        ];

        let mut require = BTreeMap::new();
        require.insert("vendor/a".to_string(), "^1.0".to_string());

        let mut require_dev = BTreeMap::new();
        require_dev.insert("vendor/b".to_string(), "^1.0".to_string());

        let mut metadata: IndexMap<String, PackagistVersion> = IndexMap::new();

        // A requires C
        let mut a_require = BTreeMap::new();
        a_require.insert("vendor/c".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/a".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", a_require),
        );

        // B requires D
        let mut b_require = BTreeMap::new();
        b_require.insert("vendor/d".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/b".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", b_require),
        );

        // C and D have no deps
        metadata.insert(
            "vendor/c".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", BTreeMap::new()),
        );
        metadata.insert(
            "vendor/d".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", BTreeMap::new()),
        );

        let requires_by_name: IndexMap<String, Vec<String>> = metadata
            .iter()
            .map(|(name, pv)| (name.to_lowercase(), pv.require.keys().cloned().collect()))
            .collect();
        let providers_by_name: IndexMap<String, Vec<String>> = metadata
            .keys()
            .map(|name| {
                let lower = name.to_lowercase();
                (lower.clone(), vec![lower])
            })
            .collect();
        let dev_only = classify_dev_packages(
            &resolved,
            &require,
            &require_dev,
            &requires_by_name,
            &providers_by_name,
        );

        assert!(!dev_only.contains("vendor/a"), "A is a production package");
        assert!(dev_only.contains("vendor/b"), "B is dev-only");
        assert!(
            !dev_only.contains("vendor/c"),
            "C is reachable from A (production)"
        );
        assert!(
            dev_only.contains("vendor/d"),
            "D is only reachable from B (dev)"
        );
    }

    #[test]
    fn test_classify_dev_packages_shared() {
        // Root: require={A}, require-dev={B}
        // Both A and B depend on C — C is NOT dev-only (reachable from production)
        let resolved = vec![
            ResolvedPackage {
                name: "vendor/a".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
                alias_of_normalized: None,
            },
            ResolvedPackage {
                name: "vendor/b".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
                alias_of_normalized: None,
            },
            ResolvedPackage {
                name: "vendor/c".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: "1.0.0.0".to_string(),
                is_dev: false,
                alias_of_normalized: None,
            },
        ];

        let mut require = BTreeMap::new();
        require.insert("vendor/a".to_string(), "^1.0".to_string());

        let mut require_dev = BTreeMap::new();
        require_dev.insert("vendor/b".to_string(), "^1.0".to_string());

        let mut metadata: IndexMap<String, PackagistVersion> = IndexMap::new();

        // A requires C
        let mut a_require = BTreeMap::new();
        a_require.insert("vendor/c".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/a".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", a_require),
        );

        // B also requires C
        let mut b_require = BTreeMap::new();
        b_require.insert("vendor/c".to_string(), "^1.0".to_string());
        metadata.insert(
            "vendor/b".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", b_require),
        );

        // C has no deps
        metadata.insert(
            "vendor/c".to_string(),
            make_packagist_version("1.0.0", "1.0.0.0", BTreeMap::new()),
        );

        let requires_by_name: IndexMap<String, Vec<String>> = metadata
            .iter()
            .map(|(name, pv)| (name.to_lowercase(), pv.require.keys().cloned().collect()))
            .collect();
        let providers_by_name: IndexMap<String, Vec<String>> = metadata
            .keys()
            .map(|name| {
                let lower = name.to_lowercase();
                (lower.clone(), vec![lower])
            })
            .collect();
        let dev_only = classify_dev_packages(
            &resolved,
            &require,
            &require_dev,
            &requires_by_name,
            &providers_by_name,
        );

        assert!(!dev_only.contains("vendor/a"), "A is a production package");
        assert!(dev_only.contains("vendor/b"), "B is dev-only");
        assert!(
            !dev_only.contains("vendor/c"),
            "C is shared but reachable from production (A), so it's not dev-only"
        );
    }

    #[test]
    fn test_extract_platform_requirements() {
        let mut requirements = BTreeMap::new();
        requirements.insert("php".to_string(), ">=8.1".to_string());
        requirements.insert("ext-json".to_string(), "*".to_string());
        requirements.insert("ext-mbstring".to_string(), "*".to_string());
        requirements.insert("monolog/monolog".to_string(), "^3.0".to_string());
        requirements.insert("lib-pcre".to_string(), "*".to_string());

        let platform = extract_platform_requirements(&requirements);
        let obj = platform.as_object().unwrap();

        assert!(obj.contains_key("php"), "php should be in platform");
        assert!(
            obj.contains_key("ext-json"),
            "ext-json should be in platform"
        );
        assert!(
            obj.contains_key("ext-mbstring"),
            "ext-mbstring should be in platform"
        );
        assert!(
            obj.contains_key("lib-pcre"),
            "lib-pcre should be in platform"
        );
        assert!(
            !obj.contains_key("monolog/monolog"),
            "monolog/monolog should NOT be in platform"
        );
        assert_eq!(obj["php"], serde_json::Value::String(">=8.1".to_string()));
        assert_eq!(obj["ext-json"], serde_json::Value::String("*".to_string()));
    }

    #[test]
    fn test_extract_platform_requirements_empty() {
        let requirements = BTreeMap::new();
        let platform = extract_platform_requirements(&requirements);
        assert_eq!(platform, serde_json::json!({}));
    }

    #[tokio::test]
    async fn test_generate_lock_file_minimal() {
        let composer_json_content =
            r#"{"name": "test/project", "require": {"php": ">=8.1"}}"#.to_string();
        let composer_json: RawPackageData = serde_json::from_str(&composer_json_content).unwrap();

        let request = LockFileGenerationRequest {
            resolved_packages: vec![],
            composer_json_content: composer_json_content.clone(),
            composer_json,
            include_dev: true,
            repositories: std::sync::Arc::new(RepositorySet::with_packagist(
                crate::cache::Cache::new(std::env::temp_dir().join("mozart-test-cache"), false),
            )),
            previous_lock: None,
        };

        let lock = generate_lock_file(&request).await.unwrap();

        assert_eq!(lock.packages.len(), 0);
        assert_eq!(lock.packages_dev.as_ref().unwrap().len(), 0);
        assert_eq!(lock.minimum_stability, "stable");
        assert!(!lock.prefer_stable);
        assert!(!lock.prefer_lowest);
        assert_eq!(lock.plugin_api_version.as_deref(), Some("2.6.0"));

        // Verify content-hash matches
        let expected_hash = LockFile::compute_content_hash(&composer_json_content).unwrap();
        assert_eq!(lock.content_hash, expected_hash);

        // Verify platform requirements extracted
        let platform_obj = lock.platform.as_object().unwrap();
        assert!(platform_obj.contains_key("php"));
        assert_eq!(
            platform_obj["php"],
            serde_json::Value::String(">=8.1".to_string())
        );
    }

    #[test]
    fn test_lock_file_packages_sorted() {
        // Verify that packages are sorted alphabetically when assembled in generate_lock_file
        // We test this by constructing two LockedPackages and sorting them the same way

        let mut packages = [
            LockedPackage {
                name: "vendor/zebra".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                provide: BTreeMap::new(),
                replace: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            },
            LockedPackage {
                name: "vendor/alpha".to_string(),
                version: "1.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                provide: BTreeMap::new(),
                replace: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            },
        ];

        packages.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(packages[0].name, "vendor/alpha");
        assert_eq!(packages[1].name, "vendor/zebra");
    }

    #[tokio::test]
    #[ignore]
    async fn test_generate_lock_file_monolog() {
        use crate::cache::Cache;
        use crate::resolver::PlatformConfig;
        use crate::resolver::{ResolveRequest, resolve};
        use mozart_core::package::Stability;
        use std::sync::Arc;

        // Resolve monolog/monolog ^3.0
        let resolve_request = ResolveRequest {
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
        };

        let resolved = resolve(&resolve_request)
            .await
            .expect("Resolution should succeed");
        assert!(!resolved.is_empty());

        let composer_json_content =
            r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#.to_string();
        let composer_json: RawPackageData = serde_json::from_str(&composer_json_content).unwrap();

        let gen_request = LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: composer_json_content.clone(),
            composer_json,
            include_dev: false,
            repositories: Arc::new(RepositorySet::with_packagist(Cache::new(
                std::env::temp_dir().join("mozart-test-cache"),
                false,
            ))),
            previous_lock: None,
        };

        let lock = generate_lock_file(&gen_request)
            .await
            .expect("Lock file generation should succeed");

        // Verify monolog is in packages
        assert!(
            lock.packages.iter().any(|p| p.name == "monolog/monolog"),
            "monolog/monolog should be in packages"
        );

        // Verify packages are sorted alphabetically
        let names: Vec<&str> = lock.packages.iter().map(|p| p.name.as_str()).collect();
        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(
            names, sorted_names,
            "Packages should be sorted alphabetically"
        );

        // Verify content-hash matches
        let expected_hash = LockFile::compute_content_hash(&composer_json_content).unwrap();
        assert_eq!(lock.content_hash, expected_hash);

        // Verify monolog has full metadata
        let monolog = lock
            .packages
            .iter()
            .find(|p| p.name == "monolog/monolog")
            .unwrap();
        assert!(monolog.dist.is_some(), "monolog should have dist info");
        assert!(
            monolog.description.is_some(),
            "monolog should have description"
        );
        assert!(monolog.autoload.is_some(), "monolog should have autoload");

        println!("Generated lock file with {} packages:", lock.packages.len());
        for pkg in &lock.packages {
            println!("  {} {}", pkg.name, pkg.version);
        }
    }

    // ──────────── get_missing_requirement_info tests ────────────

    fn make_locked(name: &str, version: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            require: BTreeMap::new(),
            require_dev: BTreeMap::new(),
            conflict: BTreeMap::new(),
            provide: BTreeMap::new(),
            replace: BTreeMap::new(),
            suggest: None,
            package_type: Some("library".to_string()),
            autoload: None,
            autoload_dev: None,
            license: None,
            description: None,
            homepage: None,
            keywords: None,
            authors: None,
            support: None,
            funding: None,
            time: None,
            extra_fields: BTreeMap::new(),
        }
    }

    fn lock_with(packages: Vec<LockedPackage>, dev: Vec<LockedPackage>) -> LockFile {
        LockFile {
            readme: LockFile::default_readme(),
            content_hash: "x".to_string(),
            packages,
            packages_dev: Some(dev),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        }
    }

    fn root_with_require(
        require: &[(&str, &str)],
        require_dev: &[(&str, &str)],
    ) -> mozart_core::package::RawPackageData {
        let mut root = mozart_core::package::RawPackageData::new("__root__".to_string());
        for (k, v) in require {
            root.require.insert((*k).to_string(), (*v).to_string());
        }
        for (k, v) in require_dev {
            root.require_dev.insert((*k).to_string(), (*v).to_string());
        }
        root
    }

    #[test]
    fn missing_requirement_info_empty_when_satisfied() {
        let lock = lock_with(vec![make_locked("a/a", "1.0.0")], vec![]);
        let root = root_with_require(&[("a/a", "^1.0")], &[]);
        assert!(lock.get_missing_requirement_info(&root, true).is_empty());
    }

    #[test]
    fn missing_requirement_info_reports_missing_package() {
        let lock = lock_with(vec![], vec![]);
        let root = root_with_require(&[("a/a", "^1.0")], &[]);
        let info = lock.get_missing_requirement_info(&root, true);
        assert_eq!(
            info[0],
            "- Required package \"a/a\" is not present in the lock file."
        );
        assert!(info.iter().any(|m| m.contains("merge conflicts")));
    }

    #[test]
    fn missing_requirement_info_reports_unsatisfied_constraint() {
        let lock = lock_with(vec![make_locked("some/dep", "dev-foo")], vec![]);
        let root = root_with_require(&[("some/dep", "dev-main")], &[]);
        let info = lock.get_missing_requirement_info(&root, true);
        assert_eq!(
            info[0],
            "- Required package \"some/dep\" is in the lock file as \"dev-foo\" but that does not satisfy your constraint \"dev-main\"."
        );
    }

    #[test]
    fn missing_requirement_info_skips_platform_packages() {
        let lock = lock_with(vec![], vec![]);
        let root = root_with_require(&[("php", "^8.0"), ("ext-json", "*")], &[]);
        assert!(lock.get_missing_requirement_info(&root, true).is_empty());
    }

    #[test]
    fn missing_requirement_info_skips_self_version() {
        let lock = lock_with(vec![], vec![]);
        let root = root_with_require(&[("a/a", "self.version")], &[]);
        assert!(lock.get_missing_requirement_info(&root, true).is_empty());
    }

    #[test]
    fn missing_requirement_info_dev_pool_includes_packages_dev() {
        // require-dev "a/a" should be satisfied by an entry in packages-dev.
        let lock = lock_with(vec![], vec![make_locked("a/a", "1.0.0")]);
        let root = root_with_require(&[], &[("a/a", "^1.0")]);
        assert!(lock.get_missing_requirement_info(&root, true).is_empty());
    }

    #[test]
    fn missing_requirement_info_skips_dev_when_include_dev_false() {
        // require-dev errors must NOT appear when include_dev is false (no_dev).
        let lock = lock_with(vec![], vec![]);
        let root = root_with_require(&[], &[("a/a", "^1.0")]);
        assert!(lock.get_missing_requirement_info(&root, false).is_empty());
    }

    #[test]
    fn missing_requirement_info_require_pool_excludes_packages_dev() {
        // A regular require should NOT be satisfied by an entry that lives only
        // in packages-dev.
        let lock = lock_with(vec![], vec![make_locked("a/a", "1.0.0")]);
        let root = root_with_require(&[("a/a", "^1.0")], &[]);
        let info = lock.get_missing_requirement_info(&root, true);
        assert_eq!(
            info[0],
            "- Required package \"a/a\" is not present in the lock file."
        );
    }

    #[test]
    fn missing_requirement_info_reports_multiple_problems() {
        let lock = lock_with(vec![make_locked("some/dep", "dev-foo")], vec![]);
        let root = root_with_require(&[("some/dep", "dev-main"), ("some/dep2", "dev-main")], &[]);
        let info = lock.get_missing_requirement_info(&root, true);
        assert!(
            info.iter()
                .any(|m| m.contains("some/dep") && m.contains("dev-foo") && m.contains("dev-main"))
        );
        assert!(
            info.iter()
                .any(|m| m == "- Required package \"some/dep2\" is not present in the lock file.")
        );
    }

    #[test]
    fn missing_requirement_info_uses_dev_description_label() {
        let lock = lock_with(vec![], vec![]);
        let root = root_with_require(&[], &[("a/a", "^1.0")]);
        let info = lock.get_missing_requirement_info(&root, true);
        assert!(info[0].contains("Required (in require-dev) package \"a/a\""));
    }
}
