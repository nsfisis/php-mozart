use crate::composer::Composer;
use clap::Args;
use indexmap::{IndexMap, IndexSet};
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::package;
use mozart_core::platform::is_platform_package;
use mozart_core::repository::lockfile;
use mozart_core::repository::resolver::{
    self, LockedPackageInfo, PlatformConfig, ResolveRequest, ResolvedPackage,
};

#[derive(Args, Default)]
pub struct UpdateArgs {
    /// Package(s) to update
    pub packages: Vec<String>,

    /// Temporary version constraint overrides
    #[arg(long)]
    pub with: Vec<String>,

    /// Forces installation from package sources when possible
    #[arg(long)]
    pub prefer_source: bool,

    /// Forces installation from package dist
    #[arg(long)]
    pub prefer_dist: bool,

    /// Forces usage of a specific install method (dist, source, auto)
    #[arg(long, value_parser = ["source", "dist", "auto"])]
    pub prefer_install: Option<String>,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// [Deprecated] Enables installation of require-dev packages
    #[arg(long)]
    pub dev: bool,

    /// Disables installation of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Only updates the lock file hash
    #[arg(long)]
    pub lock: bool,

    /// Skip the install step
    #[arg(long)]
    pub no_install: bool,

    /// Skip the audit step
    #[arg(long)]
    pub no_audit: bool,

    /// Audit output format
    #[arg(long, value_parser = ["table", "plain", "json", "summary"])]
    pub audit_format: Option<String>,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Skips autoloader generation
    #[arg(long)]
    pub no_autoloader: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Update also dependencies of packages in the argument list
    #[arg(short = 'w', long)]
    pub with_dependencies: bool,

    /// Update also all dependencies including root requirements
    #[arg(short = 'W', long)]
    pub with_all_dependencies: bool,

    /// Optimizes PSR-0 and PSR-4 packages to be loaded with classmaps
    #[arg(short, long)]
    pub optimize_autoloader: bool,

    /// Autoload classes from the classmap only
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,

    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu_autoloader: bool,

    /// Use a custom prefix for the APCu autoloader cache
    #[arg(long)]
    pub apcu_autoloader_prefix: Option<String>,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

    /// Prefer stable versions of dependencies
    #[arg(long)]
    pub prefer_stable: bool,

    /// Prefer lowest versions of dependencies
    #[arg(long)]
    pub prefer_lowest: bool,

    /// Prefer minimal restriction updates
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,

    /// Only allow patch version updates
    #[arg(long)]
    pub patch_only: bool,

    /// Interactive package selection
    #[arg(short, long)]
    pub interactive: bool,

    /// Only update packages that are root requirements
    #[arg(long)]
    pub root_reqs: bool,

    /// Bump version constraints after update (dev, no-dev, all)
    #[arg(long)]
    pub bump_after_update: Option<Option<String>>,
}

/// The kind of change for a package during update.
///
/// Mirrors `Composer\DependencyResolver\Operation\{InstallOperation,UpdateOperation,UninstallOperation}`.
#[derive(Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Install {
        new_version: String,
    },
    Update {
        old_version: String,
        new_version: String,
    },
    Uninstall {
        old_version: String,
    },
}

/// A single package change entry computed during update.
#[derive(Debug)]
pub struct UpdateChange {
    pub name: String,
    pub kind: ChangeKind,
}

/// Parse a minimum-stability string from composer.json into a `Stability` enum value.
///
/// Recognizes "stable", "RC", "beta", "alpha", "dev" (case-insensitive).
/// Defaults to `package::Stability::Stable` for unrecognized values.
/// `update mirrors` post-process: rewrite each new lock package's source/dist
/// reference back to the version recorded in the old lock when the source
/// type (and dist type) match. Mirrors Composer's
/// `LockTransaction::updateMirrorAndUrls`: a pure URL/mirror change pulls
/// the new URL block from the repository but keeps the lock's existing
/// reference, so `composer update mirrors` only rewrites transport metadata
/// — not the package content the user sees as installed. When the source
/// type changed the new entry is left untouched so the install step still
/// emits the Update operation Composer would.
fn apply_mirror_ref_overrides(new_lock: &mut lockfile::LockFile, old: &lockfile::LockFile) {
    let old_pkgs: Vec<&lockfile::LockedPackage> = old
        .packages
        .iter()
        .chain(old.packages_dev.iter().flatten())
        .collect();

    let rewrite = |new_pkg: &mut lockfile::LockedPackage| {
        let Some(old_pkg) = old_pkgs
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&new_pkg.name) && p.version == new_pkg.version)
        else {
            return;
        };
        // source: only override when both sides exist with matching type.
        if let (Some(old_src), Some(new_src)) = (&old_pkg.source, new_pkg.source.as_mut())
            && old_src.source_type == new_src.source_type
            && old_src.reference.is_some()
        {
            new_src.reference = old_src.reference.clone();
        }
        // dist: only override when both sides exist with matching type.
        if let (Some(old_dist), Some(new_dist)) = (&old_pkg.dist, new_pkg.dist.as_mut())
            && old_dist.dist_type == new_dist.dist_type
            && old_dist.reference.is_some()
        {
            new_dist.reference = old_dist.reference.clone();
        }
    };

    for pkg in &mut new_lock.packages {
        rewrite(pkg);
    }
    if let Some(dev) = new_lock.packages_dev.as_mut() {
        for pkg in dev {
            rewrite(pkg);
        }
    }
}

/// Resolve the root composer.json's `extra.branch-alias` against the root's
/// `version` field. Returns the alias target (e.g. `"2.0-dev"`) when both
/// `version` and a matching `branch-alias` entry are present, mirroring
/// Composer's `RootPackageLoader` branch-alias detection on the root package.
/// `None` for projects without a `version` or without a matching alias entry.
fn extract_root_branch_alias(
    composer_json: &mozart_core::package::RawPackageData,
) -> Option<String> {
    let version = composer_json.version.as_deref()?;
    if version.is_empty() {
        return None;
    }
    composer_json
        .extra_fields
        .get("extra")
        .and_then(|extra| extra.get("branch-alias"))
        .and_then(|aliases| aliases.as_object())
        .and_then(|map| map.get(version))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Compare old lock vs new lock to determine installs, updates, removals, and unchanged packages.
///
/// Produces one `UpdateChange` per affected package. Packages that are identical in both
/// lock files are omitted (ChangeKind::Unchanged) from the returned vec — callers that
/// want the full list should filter as needed.
pub fn compute_update_changes(
    old_lock: Option<&lockfile::LockFile>,
    new_lock: &lockfile::LockFile,
    dev_mode: bool,
) -> Vec<UpdateChange> {
    // Build map of old lock packages keyed by lowercase name -> version string
    let mut old_map: IndexMap<String, String> = IndexMap::new();
    if let Some(old) = old_lock {
        for pkg in &old.packages {
            old_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
        }
        if dev_mode && let Some(ref dev_pkgs) = old.packages_dev {
            for pkg in dev_pkgs {
                old_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
            }
        }
    }

    // Build map of new lock packages keyed by lowercase name -> version string
    let mut new_map: IndexMap<String, String> = IndexMap::new();
    for pkg in &new_lock.packages {
        new_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
    }
    if dev_mode && let Some(ref dev_pkgs) = new_lock.packages_dev {
        for pkg in dev_pkgs {
            new_map.insert(pkg.name.to_lowercase(), pkg.version.clone());
        }
    }

    let mut changes: Vec<UpdateChange> = Vec::new();

    // Check all packages in the new lock
    for (name, new_version) in &new_map {
        if let Some(old_version) = old_map.get(name) {
            if old_version != new_version {
                changes.push(UpdateChange {
                    name: name.clone(),
                    kind: ChangeKind::Update {
                        old_version: old_version.clone(),
                        new_version: new_version.clone(),
                    },
                });
            }
        } else {
            changes.push(UpdateChange {
                name: name.clone(),
                kind: ChangeKind::Install {
                    new_version: new_version.clone(),
                },
            });
        }
    }

    // Check packages in the old lock that are missing from the new lock (uninstalls)
    for (name, old_version) in &old_map {
        if !new_map.contains_key(name) {
            changes.push(UpdateChange {
                name: name.clone(),
                kind: ChangeKind::Uninstall {
                    old_version: old_version.clone(),
                },
            });
        }
    }

    changes
}

/// Resolve a `LockedPackage`'s normalized version, falling back to the
/// canonical 4-segment form derived from the pretty version when the lock
/// omits `version_normalized`.
///
/// Lock files written by Composer always include the field, but hand-written
/// fixtures (and `.test` LOCK sections) often only carry `version`. Returning
/// the raw pretty version here would break downstream consumers that compare
/// against `mozart_semver::Version::to_string()` output — most importantly
/// `lockfile::LockFileGenerationRequest::inline_lookup`, which would then miss
/// inline `type: package` entries on partial updates and trigger a Packagist
/// fetch for a package that should never need one.
fn locked_version_normalized(pkg: &lockfile::LockedPackage) -> String {
    pkg.version_normalized.clone().unwrap_or_else(|| {
        mozart_semver::Version::parse(&pkg.version)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| pkg.version.clone())
    })
}

/// True when a locked package's `transport-options.symlink` is explicitly
/// `false`. Composer's `PoolBuilder::buildPool` only treats path repos as
/// "always reload from disk" when symlinks are enabled (the default); a
/// `symlink: false` path repo is copy-mode and gets pinned at its locked
/// version on a partial update so only the explicitly requested packages
/// move. The flag rides on the lock entry under `transport-options`, which
/// `LockedPackage` parks in `extra_fields` since the schema does not call
/// it out by name.
fn is_path_symlink_disabled(pkg: &lockfile::LockedPackage) -> bool {
    pkg.extra_fields
        .get("transport-options")
        .and_then(|v| v.as_object())
        .and_then(|m| m.get("symlink"))
        .and_then(|v| v.as_bool())
        == Some(false)
}

/// For a partial update (when specific packages are named on the CLI), swap back
/// the versions of packages that were NOT requested to be updated.
///
/// This implements the simplified approach (plan approach c):
/// - For packages in `update_packages`: use the newly resolved version.
/// - For packages NOT in `update_packages`: if the old lock has them at a different
///   version, keep the old locked version. This prevents unintended upgrades.
///
/// Note: This is a best-effort approach. In some cases the resolver may pick
/// incompatible versions; a future phase can add true pinning via resolver constraints.
pub fn apply_partial_update(
    resolved: Vec<ResolvedPackage>,
    old_lock: &lockfile::LockFile,
    update_packages: &[String],
) -> Vec<ResolvedPackage> {
    // Build a set of normalized package names we want to update
    let update_set: indexmap::IndexSet<String> =
        update_packages.iter().map(|s| s.to_lowercase()).collect();

    // Build a map of old locked packages by name -> (version, version_normalized, is_dev)
    let mut old_pkg_map: IndexMap<String, &lockfile::LockedPackage> = IndexMap::new();
    for pkg in &old_lock.packages {
        old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
    }
    if let Some(ref dev_pkgs) = old_lock.packages_dev {
        for pkg in dev_pkgs {
            old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
        }
    }

    resolved
        .into_iter()
        .map(|mut pkg| {
            let name_lower = pkg.name.to_lowercase();
            // Alias entries already carry their post-swap shape: the resolver
            // picked them from the locked-repo branch-alias surface, which is
            // exactly where the previous lock would have put them. Re-pinning
            // their `version` to the base's locked pretty would collapse the
            // alias label into the base, leaving a self-referential entry in
            // the new lock's `aliases[]` block.
            if pkg.alias_of_normalized.is_some() {
                return pkg;
            }
            // If this package is NOT in the update set and we have an old locked version,
            // swap it back to the old version to prevent unintended changes.
            //
            // Exception: path-repo packages always reload from disk (Composer's
            // PoolBuilder treats them as canonical sources, not lock-bound), so
            // the resolver-picked version must survive the partial-update
            // swap-back. The earlier locked-set construction already excludes
            // them from `locked_packages` for the same reason; mirror it here.
            if !update_set.contains(&name_lower)
                && let Some(old_pkg) = old_pkg_map.get(&name_lower)
                && old_pkg.dist.as_ref().map(|d| d.dist_type.as_str()) != Some("path")
            {
                pkg.version = old_pkg.version.clone();
                pkg.version_normalized = locked_version_normalized(old_pkg);
                pkg.is_dev = false; // preserve existing; lock file doesn't store this flag directly
            }
            pkg
        })
        .collect()
}

/// Match a single package name against a glob pattern.
///
/// Only the `*` wildcard is supported (matches any sequence of non-`/` characters
/// within a segment, or any characters when the pattern contains no `/`).
/// Examples:
///   - `symfony/*`      matches `symfony/console`, `symfony/http-kernel`
///   - `monolog/mono*`  matches `monolog/monolog`
///   - `psr/*`          matches `psr/log`, `psr/container`
fn glob_matches(pattern: &str, name: &str) -> bool {
    // Fast path: no wildcard
    if !pattern.contains('*') {
        return pattern.eq_ignore_ascii_case(name);
    }
    // Split both pattern and name on '/' and match segment-by-segment
    let pat_parts: Vec<&str> = pattern.splitn(2, '/').collect();
    let name_parts: Vec<&str> = name.splitn(2, '/').collect();

    // Both must have the same number of segments (vendor/name vs vendor/name)
    if pat_parts.len() != name_parts.len() {
        return false;
    }
    for (pp, np) in pat_parts.iter().zip(name_parts.iter()) {
        if !glob_segment_matches(pp, np) {
            return false;
        }
    }
    true
}

/// Match a single path segment against a pattern segment (no '/' involved).
/// `*` matches any sequence of characters (including empty).
fn glob_segment_matches(pattern: &str, text: &str) -> bool {
    // Simple recursive matcher
    let pat = pattern.to_lowercase();
    let txt = text.to_lowercase();
    glob_segment_matches_inner(pat.as_bytes(), txt.as_bytes())
}

fn glob_segment_matches_inner(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(&b'*'), _) => {
            // '*' can match zero or more characters
            // Try consuming zero chars, or consuming one char from text
            glob_segment_matches_inner(&pattern[1..], text)
                || (!text.is_empty() && glob_segment_matches_inner(pattern, &text[1..]))
        }
        (Some(p), Some(t)) if p == t => glob_segment_matches_inner(&pattern[1..], &text[1..]),
        _ => false,
    }
}

/// Expand a list of package specifiers (which may include wildcards) against
/// all packages in the lock file, returning the resolved concrete package names.
///
/// Non-wildcard specifiers are passed through unchanged (even if not in the lock,
/// so the resolver can report the error naturally).
pub fn expand_wildcards(
    specifiers: &[String],
    lock: &lockfile::LockFile,
    root_requires: &IndexSet<String>,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> Vec<String> {
    // Collect all locked package names (prod + dev) plus the current root
    // require names. Mirrors Composer's
    // `PoolBuilder::warnAboutNonMatchingUpdateAllowList`, which accepts a
    // pattern as soon as it matches *either* a locked package or a root
    // require (so `update new/pkg` works even when `new/pkg` was just
    // added to composer.json and isn't in the lock yet). Names appear in
    // declaration order; deduplication happens implicitly via `seen`
    // below.
    let mut all_names: Vec<String> = lock
        .packages
        .iter()
        .map(|p| p.name.to_lowercase())
        .chain(
            lock.packages_dev
                .iter()
                .flatten()
                .map(|p| p.name.to_lowercase()),
        )
        .collect();
    for name in root_requires {
        let lower = name.to_lowercase();
        if !all_names.contains(&lower) {
            all_names.push(lower);
        }
    }

    let mut result: Vec<String> = Vec::new();
    let mut seen: IndexSet<String> = IndexSet::new();

    for spec in specifiers {
        // Mirror Composer's `BasePackage::packageNameToRegexp` + the
        // `isUpdateAllowed` walk over locked packages: the pattern is
        // matched case-insensitively against each locked name, with `*`
        // expanded to `.*` and every other character treated literally.
        // Specs that match no locked package are warned about and dropped
        // — for a non-wildcard spec like `notexact/Test` that's typo'd
        // against `notexact/testpackage`, this prevents Mozart from
        // forwarding the bogus name into the resolver (which would then
        // fail looking it up). Genuinely new packages are still picked up
        // by the resolver via `composer.json` root requires regardless of
        // whether they appear in `update_packages`.
        let mut matched = false;
        for name in &all_names {
            if glob_matches(spec, name) && seen.insert(name.clone()) {
                result.push(name.clone());
                matched = true;
            }
        }
        if !matched {
            io.lock().unwrap().info(&console_format!(
                "<warning>Package '{}' listed for update is not in the lock file. Specifier will be ignored.</warning>",
                spec
            ));
        }
    }

    result
}

/// Build a lookup map from package name (lowercase) to its LockedPackage.
fn build_lock_map(lock: &lockfile::LockFile) -> IndexMap<String, &lockfile::LockedPackage> {
    let mut map = IndexMap::new();
    for pkg in &lock.packages {
        map.insert(pkg.name.to_lowercase(), pkg);
    }
    if let Some(ref dev_pkgs) = lock.packages_dev {
        for pkg in dev_pkgs {
            map.insert(pkg.name.to_lowercase(), pkg);
        }
    }
    map
}

/// Build a `name → union of require keys` lookup from inline `type: package`
/// and `type: composer` repository entries declared in `composer.json`.
///
/// Used by `expand_with_direct_dependencies` / `expand_with_all_dependencies`
/// to walk the require list of allow-listed packages that are NOT yet in the
/// lock (e.g. a newly added root require). Mirrors the dynamic unlock side
/// effect Composer's `PoolBuilder::loadPackage` produces when it loads a
/// not-yet-locked package — every require that currently sits in
/// `skippedLoad` becomes a candidate for unlocking. We approximate that here
/// by unioning the require *names* across every available version, since at
/// allow-list expansion time we don't yet know which version the resolver
/// will pick.
pub fn collect_repo_requires(
    repositories: &[mozart_core::package::RawRepository],
) -> IndexMap<String, IndexSet<String>> {
    let mut out: IndexMap<String, IndexSet<String>> = IndexMap::new();
    for ipkg in mozart_core::repository::inline_package::collect_inline_packages(repositories) {
        let entry = out.entry(ipkg.name.to_lowercase()).or_default();
        for req in ipkg.version.require.keys() {
            entry.insert(req.to_lowercase());
        }
    }
    for cpkg in mozart_core::repository::composer_repo::collect_composer_packages(repositories) {
        let entry = out.entry(cpkg.name.to_lowercase()).or_default();
        for req in cpkg.version.require.keys() {
            entry.insert(req.to_lowercase());
        }
    }
    out
}

/// Look up the require-list for `name`, unioning the lock entry's
/// requires with every available version's requires from inline /
/// composer-repo entries. Lowercase names returned, deduped.
///
/// Composer's `PoolBuilder::loadPackage` dynamically unlocks any locked
/// dependency referenced by an allow-listed package's *new* (about-to-be-
/// resolved) version. Mozart pre-computes the unlock set, so it has to
/// consider not only the lock-pinned version's requires but also every
/// candidate version the resolver might pick — otherwise upgrading a
/// locked package whose new version added a requirement on another
/// locked package leaves that other package pinned, and the resolver
/// silently keeps the old version.
fn requires_for_name(
    name: &str,
    lock_map: &IndexMap<String, &lockfile::LockedPackage>,
    repo_requires: &IndexMap<String, IndexSet<String>>,
) -> Option<Vec<String>> {
    let mut deps: IndexSet<String> = IndexSet::new();
    let mut seen = false;
    if let Some(pkg) = lock_map.get(name) {
        seen = true;
        for k in pkg.require.keys() {
            deps.insert(k.to_lowercase());
        }
    }
    if let Some(set) = repo_requires.get(name) {
        seen = true;
        for k in set {
            deps.insert(k.clone());
        }
    }
    if seen {
        Some(deps.into_iter().collect())
    } else {
        None
    }
}

/// Expand the allow-list with transitive `require` dependencies, stopping at
/// any dependency that is also a root requirement.
///
/// Mirrors Composer's `--with-dependencies` (UPDATE_LISTED_WITH_TRANSITIVE_DEPS_NO_ROOT_REQUIRE)
/// behaviour: in `PoolBuilder::loadPackage`, when a propagated package's
/// require points at a still-skipped (locked) dependency, the dependency is
/// unlocked and re-loaded — but only when it is NOT itself a root require.
/// A root require hit acts as a barrier: the locked version stays, a
/// warning is issued, and the cascade through that node stops.
///
/// `repo_requires` supplies the require list for allow-listed packages that
/// are not yet in the lock (e.g. a freshly added root require). It is built
/// via `collect_repo_requires` from the inline / composer-repo entries in
/// `composer.json`.
pub fn expand_with_direct_dependencies(
    packages: Vec<String>,
    lock: &lockfile::LockFile,
    root_requires: &IndexSet<String>,
    repo_requires: &IndexMap<String, IndexSet<String>>,
) -> Vec<String> {
    let lock_map = build_lock_map(lock);
    let replace_map = build_lock_replace_map(lock);
    let mut result_set: IndexSet<String> = packages.iter().cloned().collect();
    let mut queue: Vec<String> = packages.clone();
    let mut result: Vec<String> = packages;

    while let Some(name) = queue.pop() {
        let Some(deps) = requires_for_name(&name, &lock_map, repo_requires) else {
            continue;
        };
        for dep_name in deps {
            if is_platform_package(&dep_name) {
                continue;
            }
            // Root-require barrier: don't unlock and don't recurse.
            if root_requires.contains(&dep_name) {
                continue;
            }
            for actual in resolve_dep_via_replace(&dep_name, &lock_map, &replace_map) {
                if result_set.insert(actual.clone()) {
                    result.push(actual.clone());
                    queue.push(actual);
                }
            }
        }
    }

    result
}

/// Given a set of package names, recursively expand their full transitive
/// `require` dependency tree from the lock file (and from inline /
/// composer-repo entries for packages not yet in the lock).
pub fn expand_with_all_dependencies(
    packages: Vec<String>,
    lock: &lockfile::LockFile,
    repo_requires: &IndexMap<String, IndexSet<String>>,
) -> Vec<String> {
    let lock_map = build_lock_map(lock);
    let replace_map = build_lock_replace_map(lock);
    let mut result_set: IndexSet<String> = packages.iter().cloned().collect();
    let mut queue: Vec<String> = packages.clone();
    let mut result: Vec<String> = packages;

    while let Some(name) = queue.pop() {
        let Some(deps) = requires_for_name(&name, &lock_map, repo_requires) else {
            continue;
        };
        for dep_name in deps {
            if is_platform_package(&dep_name) {
                continue;
            }
            for actual in resolve_dep_via_replace(&dep_name, &lock_map, &replace_map) {
                if result_set.insert(actual.clone()) {
                    result.push(actual.clone());
                    queue.push(actual);
                }
            }
        }
    }

    result
}

/// Build a `replaced_name → list of replacing package names` index over the
/// lock, so a dependency on a virtual / replaced name reaches the actual
/// locked package that owns it. Mirrors the replace branch of Composer's
/// `PoolBuilder::loadPackage`: a partial update with `--with-dependencies`
/// must unlock the replacer when a transitive require points at the
/// replaced name, otherwise the resolver leaves the replacer pinned at
/// its lock version and silently fails to upgrade.
fn build_lock_replace_map(lock: &lockfile::LockFile) -> IndexMap<String, Vec<String>> {
    let mut map: IndexMap<String, Vec<String>> = IndexMap::new();
    for pkg in lock
        .packages
        .iter()
        .chain(lock.packages_dev.iter().flatten())
    {
        for replaced in pkg.replace.keys() {
            map.entry(replaced.to_lowercase())
                .or_default()
                .push(pkg.name.to_lowercase());
        }
    }
    map
}

/// Translate a dependency name into the list of locked package names that
/// effectively own it: either the package directly named (the common case)
/// or, when the name is virtual / replaced, every locked package whose
/// `replace` map covers it. The result is what should enter the unlock set
/// during `--with-(all-)dependencies` expansion.
fn resolve_dep_via_replace(
    dep_name: &str,
    lock_map: &IndexMap<String, &lockfile::LockedPackage>,
    replace_map: &IndexMap<String, Vec<String>>,
) -> Vec<String> {
    if lock_map.contains_key(dep_name) {
        vec![dep_name.to_string()]
    } else if let Some(replacers) = replace_map.get(dep_name) {
        replacers.clone()
    } else {
        vec![dep_name.to_string()]
    }
}

/// Expand the package list applying wildcard matching and optional dependency expansion.
///
/// Returns the final list of package names to update (concrete, lowercase, deduplicated).
pub fn expand_packages(
    specifiers: &[String],
    lock: Option<&lockfile::LockFile>,
    with_dependencies: bool,
    with_all_dependencies: bool,
    root_requires: &IndexSet<String>,
    repo_requires: &IndexMap<String, IndexSet<String>>,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> Vec<String> {
    let mut packages: Vec<String> = if let Some(lock) = lock {
        expand_wildcards(specifiers, lock, root_requires, io)
    } else {
        // No lock file: pass through as-is (no wildcards can be resolved)
        specifiers.iter().map(|s| s.to_lowercase()).collect()
    };

    // Then expand dependencies if requested
    if let Some(lock) = lock {
        if with_all_dependencies {
            packages = expand_with_all_dependencies(packages, lock, repo_requires);
        } else if with_dependencies {
            packages =
                expand_with_direct_dependencies(packages, lock, root_requires, repo_requires);
        }
    }

    packages
}

/// Interactively prompt the user to select which packages to update.
///
/// For each package in `packages`, prints a y/n prompt and collects the
/// user's response.  Returns only the packages the user confirmed.
///
/// When stdin is not a TTY (e.g. in CI or piped input), emits a warning and
/// returns the full package list unchanged.
pub fn interactive_select_packages(
    packages: Vec<String>,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> Vec<String> {
    use std::io::{self, BufRead, IsTerminal, Write};

    let stdin = io::stdin();
    if !stdin.is_terminal() {
        io.lock().unwrap().info(&console_format!(
            "<warning>Interactive mode requires a TTY. Running non-interactively with all packages.</warning>"
        ));
        return packages;
    }

    io.lock()
        .unwrap()
        .info("Select packages to update (y/n for each):");

    let mut selected = Vec::new();
    let stdin_locked = stdin.lock();
    let mut lines = stdin_locked.lines();

    for pkg in &packages {
        loop {
            eprint!("  Update {}? [y/n] ", pkg);
            let _ = io::stderr().flush();

            match lines.next() {
                Some(Ok(line)) => {
                    let answer = line.trim().to_lowercase();
                    match answer.as_str() {
                        "y" | "yes" => {
                            selected.push(pkg.clone());
                            break;
                        }
                        "n" | "no" => {
                            break;
                        }
                        _ => {
                            io.lock().unwrap().info("  Please answer y or n.");
                        }
                    }
                }
                _ => {
                    // EOF or error: treat as "no"
                    break;
                }
            }
        }
    }

    selected
}

/// For `--minimal-changes` mode: when no specific packages are named, pin all
/// packages to their current locked version UNLESS the current locked version
/// no longer satisfies the root constraint.  This prevents pulling in newer
/// versions of packages that don't need updating.
///
/// When specific packages ARE named, `apply_partial_update` already handles
/// pinning non-requested packages, so this function is a no-op in that case.
///
/// Implementation: We add the locked version back for every package that is NOT
/// newly required (i.e., already exists in the lock with a version that is still
/// satisfiable). In practice this is expressed by running `apply_partial_update`
/// with an empty update set — which pins *everything* — and then releasing the
/// pins for packages whose constraints have changed or that are new.
///
/// For the initial implementation we take a simpler approach: we call
/// `apply_partial_update` with an empty update list so that all packages are
/// pinned to their old locked versions.  The resolver will still produce a valid
/// solution; we then override with locked versions for packages not explicitly
/// listed.
pub fn apply_minimal_changes(
    resolved: Vec<ResolvedPackage>,
    old_lock: &lockfile::LockFile,
) -> Vec<ResolvedPackage> {
    // Pin every package to its old locked version (full pin, no updates)
    apply_partial_update(resolved, old_lock, &[])
}

/// Filter resolved packages to only allow patch-level version changes.
///
/// For each resolved package, if the old lock has a version with the same
/// major.minor, the upgrade is allowed. Otherwise the package is pinned
/// back to its old locked version.
pub fn apply_patch_only(
    resolved: Vec<ResolvedPackage>,
    old_lock: &lockfile::LockFile,
) -> Vec<ResolvedPackage> {
    let mut old_pkg_map: IndexMap<String, &lockfile::LockedPackage> = IndexMap::new();
    for pkg in &old_lock.packages {
        old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
    }
    if let Some(ref dev_pkgs) = old_lock.packages_dev {
        for pkg in dev_pkgs {
            old_pkg_map.insert(pkg.name.to_lowercase(), pkg);
        }
    }

    resolved
        .into_iter()
        .map(|mut pkg| {
            let name_lower = pkg.name.to_lowercase();
            if let Some(old_pkg) = old_pkg_map.get(&name_lower) {
                let old_norm = locked_version_normalized(old_pkg);
                let new_norm = &pkg.version_normalized;

                // Compare major.minor: if they differ, pin to old version
                let old_mm = major_minor(&old_norm);
                let new_mm = major_minor(new_norm);
                if old_mm != new_mm {
                    pkg.version = old_pkg.version.clone();
                    pkg.version_normalized = old_norm;
                }
            }
            pkg
        })
        .collect()
}

/// Determine whether a version change is a downgrade.
///
/// Parses both versions and compares them; returns true if `new_version` is
/// lower than `old_version`.
fn is_downgrade(old_version: &str, new_version: &str) -> bool {
    use mozart_semver::Version;
    match (Version::parse(old_version), Version::parse(new_version)) {
        (Ok(old), Ok(new)) => new < old,
        _ => false,
    }
}

/// Extract (major, minor) from a normalized version string.
fn major_minor(version: &str) -> (u64, u64) {
    let parts: Vec<&str> = version.split('.').collect();
    let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

/// CLI entry point. Builds production [`RepositorySet`] (Packagist) and
/// [`FilesystemExecutor`] from `cli`, then dispatches to [`run`].
pub async fn execute(
    args: &UpdateArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let repositories = std::sync::Arc::new(
        mozart_core::repository::repository::RepositorySet::with_packagist(
            mozart_core::repository::cache::Cache::repo(&cache_config),
        ),
    );
    let mut executor = mozart_core::repository::installer_executor::FilesystemExecutor::new(
        mozart_core::repository::cache::Cache::files(&cache_config),
    );
    let working_dir = cli.working_dir()?;
    run(
        &working_dir,
        None,
        args,
        io.clone(),
        repositories,
        &mut executor,
    )
    .await
}

/// Library entry point — pure logic, no CLI / Cli access.
///
/// In-process tests construct a `RepositorySet` without `PackagistRepository`
/// (Composer's `'packagist' => false` test config) and a tracing
/// `InstallerExecutor`, then call this function directly to exercise the
/// update flow without spawning the binary.
///
/// `path_repo_base_override` is for the in-process test harness only:
/// Composer's PHP test suite `chdir(__DIR__)` so that `type: path` repo URLs
/// like `Fixtures/.../pkg` resolve against the test directory, but the
/// Rust harness writes `composer.json` into a per-test tempdir, so we need a
/// way to anchor relative path-repo URLs somewhere other than `working_dir`.
/// Production callers pass `None` to use `working_dir`, matching Composer's
/// "resolve relative to cwd" behaviour.
pub async fn run(
    working_dir: &std::path::Path,
    path_repo_base_override: Option<&std::path::Path>,
    args: &UpdateArgs,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    repositories: std::sync::Arc<mozart_core::repository::repository::RepositorySet>,
    executor: &mut dyn mozart_core::repository::installer_executor::InstallerExecutor,
) -> anyhow::Result<()> {
    // Step 2: Handle deprecated flags
    if args.dev {
        io.lock().unwrap().info(&console_format!(
            "<warning>The --dev option is deprecated. Dev packages are updated by default.</warning>"
        ));
    }
    if args.no_suggest {
        io.lock().unwrap().info(&console_format!(
            "<warning>You are using the deprecated option \"--no-suggest\". It has no effect and will break in Composer 3.</warning>"
        ));
    }

    // --root-reqs: if no packages specified, auto-populate with root requirements
    if args.root_reqs && args.packages.is_empty() {
        io.lock()
            .unwrap()
            .info("Using root requirements as the update list (--root-reqs).");
    }

    // Step 3: Read composer.json
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::GENERAL_ERROR,
            format!(
                "Composer could not find a composer.json file in {}",
                working_dir.display()
            ),
        ));
    }
    let composer_json = package::read_from_file(&composer_json_path)?;
    composer_json.validate_root_does_not_self_require()?;
    let composer_json_content = std::fs::read_to_string(&composer_json_path)?;

    // Expand `type: path` repos into synthetic `type: package` entries so the
    // resolver and lockfile see them as ordinary inline packages. The
    // original `composer_json.repositories` is preserved for writeback paths
    // (e.g. `--bump-after-update` rewrites composer.json) — only the cloned
    // `composer_json_expanded` carries the synthetic entries.
    let path_repo_base = path_repo_base_override.unwrap_or(working_dir);
    let composer_json_expanded = {
        let mut clone = composer_json.clone();
        clone.repositories = mozart_core::repository::path_repository::expand_path_repositories(
            &clone.repositories,
            path_repo_base,
        );
        clone
    };

    let lock_path = working_dir.join("composer.lock");
    let vendor_dir = working_dir.join("vendor");

    let dev_mode = !args.no_dev;

    // Build the set of root require names (lowercase, excluding platform
    // packages). Used as the barrier for `--with-dependencies` transitive
    // expansion: Composer's UPDATE_LISTED_WITH_TRANSITIVE_DEPS_NO_ROOT_REQUIRE
    // mode (Request.php) leaves root requires locked even when they are
    // depended on by an allow-listed package, emitting a warning instead.
    let root_requires: IndexSet<String> = {
        let mut s: IndexSet<String> = composer_json
            .require
            .keys()
            .filter(|k| !is_platform_package(k))
            .map(|k| k.to_lowercase())
            .collect();
        if dev_mode {
            s.extend(
                composer_json
                    .require_dev
                    .keys()
                    .filter(|k| !is_platform_package(k))
                    .map(|k| k.to_lowercase()),
            );
        }
        s
    };

    // Fix 1C + Fix 2: Parse --with constraints and inline constraint shorthand.
    let mut temporary_constraints: IndexMap<String, String> = IndexMap::new();

    // Parse --with constraints (format: "vendor/package:constraint")
    for with_entry in &args.with {
        if let Some((name, constraint)) = with_entry.split_once(':') {
            let name = name.trim().to_lowercase();
            let constraint = constraint.trim().to_string();
            if !name.is_empty() && !constraint.is_empty() {
                temporary_constraints.insert(name, constraint);
            }
        }
    }

    // Fix 2: Parse inline constraint shorthand from package arguments
    // (e.g. "vendor/package:1.0.*" -> name="vendor/package", constraint="1.0.*")
    let mut raw_packages: Vec<String> = Vec::new();
    for pkg in &args.packages {
        if let Some((name, constraint)) = pkg.split_once(':') {
            let name = name.trim().to_string();
            let constraint = constraint.trim().to_string();
            if !name.is_empty() && !constraint.is_empty() {
                temporary_constraints.insert(name.to_lowercase(), constraint);
                raw_packages.push(name);
            } else {
                raw_packages.push(pkg.clone());
            }
        } else {
            raw_packages.push(pkg.clone());
        }
    }

    // Filter magic keywords (`lock`, `nothing`, `mirrors`) from the package list.
    // Mirrors Composer's UpdateCommand::execute 214–226:
    //   $filteredPackages = array_filter($packages, fn($p) => !in_array($p, ['lock','nothing','mirrors']));
    //   $updateMirrors = --lock || count($filteredPackages) !== count($packages);
    //   if ($updateMirrors && count($filteredPackages) > 0) → error
    let packages_before_filter_len = raw_packages.len();
    let raw_packages: Vec<String> = raw_packages
        .into_iter()
        .filter(|p| !matches!(p.to_lowercase().as_str(), "lock" | "nothing" | "mirrors"))
        .collect();
    let update_mirrors = args.lock || raw_packages.len() != packages_before_filter_len;

    // Mirrors+packages mutex: cannot simultaneously update a selection and regenerate
    // lock metadata. Composer returns -1 here; Mozart uses exit 1 (no -1 on Unix).
    if update_mirrors && !raw_packages.is_empty() {
        anyhow::bail!(
            "You cannot simultaneously update only a selection of packages and regenerate the lock file metadata."
        );
    }

    // --patch-only requires a lock file: fail fast before the solve.
    // Mirrors Composer's UpdateCommand::execute 177–178 which throws
    // InvalidArgumentException when no lock exists.
    if args.patch_only && !lock_path.exists() {
        return Err(mozart_core::exit_code::bail(
            mozart_core::exit_code::GENERAL_ERROR,
            "The --patch-only option requires a lock file to be present.",
        ));
    }

    // --patch-only PRE-SOLVE constraint injection: for each locked package
    // whose pretty version starts with M.N.P, inject a `~M.N.P` temporary
    // constraint so the resolver itself only allows patch-level moves.
    // Mirrors Composer's UpdateCommand::execute 177–195. Packages that
    // already have a user-supplied temporary constraint are skipped (the
    // user's explicit `--with foo:^2` takes precedence).
    if args.patch_only
        && lock_path.exists()
        && let Ok(lock) = lockfile::LockFile::read_from_file(&lock_path)
    {
        for pkg in lock
            .packages
            .iter()
            .chain(lock.packages_dev.iter().flatten())
        {
            let name_lower = pkg.name.to_lowercase();
            if temporary_constraints.contains_key(&name_lower) {
                continue;
            }
            // Only apply to SemVer-like versions starting with M.N.P.
            // Mirrors Composer's UpdateCommand::execute preg_match('{^\d+\.\d+\.\d+}').
            let parts: Vec<&str> = pkg.version.splitn(4, '.').collect();
            if parts.len() >= 3 {
                let patch_raw = parts[2].split(['-', '+']).next().unwrap_or("0");
                if let (Ok(major), Ok(minor), Ok(patch)) = (
                    parts[0].parse::<u64>(),
                    parts[1].parse::<u64>(),
                    patch_raw.parse::<u64>(),
                ) {
                    // >=M.N.P.0, <M.(N+1).0.0 — mirrors Composer's MultiConstraint
                    let constraint = format!(
                        ">={}.{}.{}.0, <{}.{}.0.0",
                        major,
                        minor,
                        patch,
                        major,
                        minor + 1
                    );
                    temporary_constraints.insert(name_lower, constraint);
                }
            }
        }
    }

    // For partial updates (specific package names given), eagerly read the
    // lock file to gather both the names that stay pinned across this
    // resolve *and* the full package data needed to seed fixed pool
    // entries for those names. The resolver uses the names to skip
    // materializing root `as` aliases (Composer's `propagateUpdate=false`
    // branch in `PoolBuilder::loadPackage`) and the full data to add a
    // fixed entry per locked package — without the fixed entry the SAT
    // solver may pick a different version of a locked package, including
    // one that `replace`s an allow-listed dependency, silently dropping
    // it from the install plan.
    //
    // The full lock is re-read below for change reporting and
    // `apply_partial_update` post-processing. Reading it twice is fine:
    // it's a small JSON file. Errors here fall back to empty
    // collections (treat as full update); the later read surfaces the
    // failure to the user.
    let (locked_package_names, locked_packages): (IndexSet<String>, Vec<LockedPackageInfo>) =
        if !raw_packages.is_empty() && lock_path.exists() {
            match lockfile::LockFile::read_from_file(&lock_path) {
                Ok(l) => {
                    // Apply `--with-dependencies` / `--with-all-dependencies`
                    // expansion so transitive deps of allow-listed packages
                    // are not held back at their locked version. Mirrors
                    // Composer's `PoolBuilder::loadPackage` unlock cascade
                    // (line 524: when a propagated package's `require`
                    // points at a `skippedLoad` entry, the dep is unlocked
                    // and re-loaded).
                    let repo_requires = collect_repo_requires(&composer_json_expanded.repositories);
                    let updated: IndexSet<String> = expand_packages(
                        &raw_packages,
                        Some(&l),
                        args.with_dependencies,
                        args.with_all_dependencies,
                        &root_requires,
                        &repo_requires,
                        io.clone(),
                    )
                    .into_iter()
                    .collect();
                    let mut names: IndexSet<String> = IndexSet::new();
                    let mut infos: Vec<LockedPackageInfo> = Vec::new();
                    for p in l.packages.iter().chain(l.packages_dev.iter().flatten()) {
                        let name_lower = p.name.to_lowercase();
                        if updated.contains(&name_lower) {
                            continue;
                        }
                        // Path-repo packages backed by a symlink are always
                        // reloaded from disk by Composer (PoolBuilder treats
                        // them as canonical sources, not lock-bound). Skip
                        // them so the pool sees the live on-disk version,
                        // matching Composer's "path repos always update"
                        // behaviour. The exception — and the reason for the
                        // explicit `transport-options.symlink == false`
                        // check — is a copy-mode path repo: with symlinks
                        // disabled, Composer keeps the locked entry pinned
                        // so a partial update only refreshes the named
                        // package(s), not every path-repo dep on disk.
                        // Mirrors `PoolBuilder::buildPool` lines 230-235.
                        if p.dist.as_ref().map(|d| d.dist_type.as_str()) == Some("path")
                            && !is_path_symlink_disabled(p)
                        {
                            continue;
                        }
                        names.insert(name_lower.clone());
                        let branch_aliases = lockfile::locked_package_branch_aliases(p)
                            .into_iter()
                            .map(|a| (a.alias, a.alias_normalized))
                            .collect();
                        infos.push(LockedPackageInfo {
                            name: name_lower,
                            pretty_version: p.version.clone(),
                            version_normalized: locked_version_normalized(p),
                            requires: p
                                .require
                                .iter()
                                .map(|(k, v)| (k.to_lowercase(), v.clone()))
                                .collect(),
                            replaces: p
                                .replace
                                .iter()
                                .map(|(k, v)| (k.to_lowercase(), v.clone()))
                                .collect(),
                            provides: p
                                .provide
                                .iter()
                                .map(|(k, v)| (k.to_lowercase(), v.clone()))
                                .collect(),
                            conflicts: p
                                .conflict
                                .iter()
                                .map(|(k, v)| (k.to_lowercase(), v.clone()))
                                .collect(),
                            branch_aliases,
                        });
                    }
                    (names, infos)
                }
                Err(_) => (IndexSet::new(), Vec::new()),
            }
        } else {
            (IndexSet::new(), Vec::new())
        };

    // Step 5: Build the resolve request from composer.json. In `mirrors`
    // mode, swap the root requires for `==<lock-version>` pins on every
    // locked package, mirroring `Composer\Installer::requirePackagesForUpdate`
    // when `updateMirrors` is true: locked versions are preserved, while
    // source/dist metadata is reloaded fresh from the repository (so a
    // VCS-type / URL flip on disk shows up in the new lock and trace).
    // Filter out platform packages from require list for the resolver
    // (they're handled separately).
    let (require, require_dev) = if update_mirrors {
        let mut req: Vec<(String, String)> = Vec::new();
        let mut req_dev: Vec<(String, String)> = Vec::new();
        if let Ok(lock) = lockfile::LockFile::read_from_file(&lock_path) {
            // Re-attach any `as <alias>` clause the lock recorded for this
            // package so the resolver materializes the same alias entry it
            // would on a fresh install. Without this, mirrors mode would
            // pin `c/aliased ==1.0.0` while a transitive dep requires
            // `c/aliased 2.0.0`, with no alias bridging the two — and the
            // solver fails despite the lock being internally consistent.
            // Mirrors Composer's `Locker::getLockedRepository` pulling lock
            // aliases into the solver's pool.
            let alias_for = |name: &str| -> Option<String> {
                lock.aliases
                    .iter()
                    .find(|a| a.package.eq_ignore_ascii_case(name))
                    .map(|a| a.alias.clone())
            };
            // The alias-bearing form uses the bare `<version>` instead of
            // `==<version>` because the resolver's alias extractor only
            // accepts a parsable LEFT atom; `==1.0.0` would fail
            // `VersionParser::normalize` and the alias pair would be
            // dropped silently. A bare `1.0.0` constraint matches the same
            // exact version as `==1.0.0`, so the lock pin is preserved.
            let pin_with_alias = |name: &str, version: &str| -> String {
                match alias_for(name) {
                    Some(alias) => format!("{version} as {alias}"),
                    None => format!("=={version}"),
                }
            };
            for pkg in &lock.packages {
                req.push((pkg.name.clone(), pin_with_alias(&pkg.name, &pkg.version)));
            }
            for pkg in lock.packages_dev.iter().flatten() {
                req_dev.push((pkg.name.clone(), pin_with_alias(&pkg.name, &pkg.version)));
            }
        }
        (req, req_dev)
    } else {
        (
            composer_json
                .require
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            composer_json
                .require_dev
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    };

    // Parse minimum-stability from composer.json (defaults to "stable")
    let minimum_stability_str = composer_json
        .minimum_stability
        .as_deref()
        .unwrap_or("stable");
    let minimum_stability = package::Stability::parse(minimum_stability_str);

    // Determine prefer-stable: CLI flag OR composer.json field
    let composer_prefer_stable = composer_json
        .extra_fields
        .get("prefer-stable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let prefer_stable = args.prefer_stable || composer_prefer_stable;

    let mut platform = PlatformConfig::new();
    if let Some(overrides) = composer_json
        .extra_fields
        .get("config")
        .and_then(|c| c.get("platform"))
    {
        platform.apply_overrides(overrides);
    }

    // Mirrors `Composer\Advisory\AuditConfig::fromConfig`: read
    // `config.audit.block-abandoned` straight off composer.json. Defaults to
    // false; when true the resolver drops abandoned packages from the pool.
    let block_abandoned = composer_json
        .extra_fields
        .get("config")
        .and_then(|c| c.get("audit"))
        .and_then(|a| a.get("block-abandoned"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Mirrors `Composer\Advisory\AuditConfig::fromConfig`: `block-insecure`
    // turns the security-advisory data into a hard filter — affected
    // versions are dropped from the pool, so a root require with no
    // unaffected candidates fails resolution before any side effects.
    let block_insecure = composer_json
        .extra_fields
        .get("config")
        .and_then(|c| c.get("audit"))
        .and_then(|a| a.get("block-insecure"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // For `--minimal-changes`, feed the lock's pinned versions into the
    // resolver as preferred-version overrides. The packages the user
    // explicitly named on the CLI are excluded — they're being asked to
    // move, so the policy should pick the regular highest/lowest version
    // for them. Mirrors Composer's
    // `Installer::createPolicy(forUpdate=true, minimalUpdate=true)` branch,
    // which loops over the locked repository and skips any
    // `updateAllowList` entry. Transitive deps pulled in by
    // `--with-(all-)dependencies` stay in the map so they only move when a
    // constraint actually forces a different version.
    let preferred_versions: IndexMap<String, String> = if args.minimal_changes && lock_path.exists()
    {
        match lockfile::LockFile::read_from_file(&lock_path) {
            Ok(lock) => {
                let allow_set: IndexSet<String> =
                    raw_packages.iter().map(|s| s.to_lowercase()).collect();
                let mut map = IndexMap::new();
                for pkg in lock
                    .packages
                    .iter()
                    .chain(lock.packages_dev.iter().flatten())
                {
                    let name_lower = pkg.name.to_lowercase();
                    if allow_set.contains(&name_lower) {
                        continue;
                    }
                    map.insert(name_lower, locked_version_normalized(pkg));
                }
                map
            }
            Err(_) => IndexMap::new(),
        }
    } else {
        IndexMap::new()
    };

    let request = ResolveRequest {
        root_name: composer_json.name.clone(),
        root_version: composer_json.version.clone(),
        require,
        require_dev,
        // Mirrors `Composer\Installer::doUpdate` line 498:
        // `requirePackagesForUpdate($request, $lockedRepo, true)` —
        // require-dev is always part of the first solve, regardless of
        // --no-dev. The flag only affects what gets installed and the
        // packages-dev split in the lock file.
        include_dev: true,
        minimum_stability,
        stability_flags: IndexMap::new(),
        prefer_stable,
        prefer_lowest: args.prefer_lowest,
        platform,
        ignore_platform_reqs: args.ignore_platform_reqs,
        ignore_platform_req_list: args.ignore_platform_req.clone(),
        repositories: repositories.clone(),
        temporary_constraints,
        raw_repositories: composer_json_expanded.repositories.clone(),
        root_provide: composer_json
            .provide
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_replace: composer_json
            .replace
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        root_conflict: composer_json
            .conflict
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        locked_package_names,
        locked_packages,
        block_abandoned,
        root_branch_alias: extract_root_branch_alias(&composer_json),
        preferred_versions,
        block_insecure,
    };

    // Step 6: Print header and run resolver
    io.lock()
        .unwrap()
        .info("Loading composer repositories with package information");
    if dev_mode {
        io.lock()
            .unwrap()
            .info("Updating dependencies (including require-dev)");
    } else {
        io.lock().unwrap().info("Updating dependencies");
    }
    let mut resolved = match resolver::resolve(&request).await {
        Ok(packages) => packages,
        Err(e) => {
            return Err(mozart_core::exit_code::bail(
                mozart_core::exit_code::DEPENDENCY_RESOLUTION_FAILED,
                e.to_string(),
            ));
        }
    };

    // Step 7: Read old lock file (for change reporting and partial update)
    let old_lock = if lock_path.exists() {
        match lockfile::LockFile::read_from_file(&lock_path) {
            Ok(l) => Some(l),
            Err(e) => {
                io.lock().unwrap().info(&console_format!(
                    "<warning>Could not read existing composer.lock: {}. Treating as a fresh install.</warning>",
                    e
                ));
                None
            }
        }
    } else {
        None
    };

    // Step 8: Expand package list (wildcards + dependency expansion) and handle
    //         interactive selection, then apply partial update logic.
    //
    // Note: wildcard expansion and dependency traversal both require a lock file.
    // If --minimal-changes is requested without specific packages, we pin all packages.
    // Save raw_packages for the --bump-after-update delegate before it is moved.
    let raw_packages_for_bump = raw_packages.clone();
    // --root-reqs: treat root requirements as the package list
    let effective_packages: Vec<String> = if args.root_reqs && raw_packages.is_empty() {
        let mut root_pkgs: Vec<String> = composer_json
            .require
            .keys()
            .filter(|k| !is_platform_package(k))
            .map(|k| k.to_lowercase())
            .collect();
        if dev_mode {
            root_pkgs.extend(
                composer_json
                    .require_dev
                    .keys()
                    .filter(|k| !is_platform_package(k))
                    .map(|k| k.to_lowercase()),
            );
        }
        root_pkgs
    } else {
        raw_packages
    };

    let update_packages: Vec<String> = if !effective_packages.is_empty() {
        match &old_lock {
            None => {
                return Err(mozart_core::exit_code::bail(
                    mozart_core::exit_code::NO_LOCK_FILE_FOR_PARTIAL_UPDATE,
                    "No lock file found. Cannot perform partial update. Run `mozart update` first.",
                ));
            }
            Some(lock) => {
                // 1. Expand wildcards
                let repo_requires = collect_repo_requires(&composer_json_expanded.repositories);
                let mut expanded = expand_packages(
                    &effective_packages,
                    Some(lock),
                    args.with_dependencies,
                    args.with_all_dependencies,
                    &root_requires,
                    &repo_requires,
                    io.clone(),
                );

                // 2. Interactive selection (filter the expanded list)
                if args.interactive {
                    expanded = interactive_select_packages(expanded, io.clone());
                }

                expanded
            }
        }
    } else {
        // No specific packages: full update mode
        // If --interactive, show all locked packages and let user select
        if args.interactive {
            match &old_lock {
                None => {
                    io.lock().unwrap().info(&console_format!(
                        "<warning>No lock file found. --interactive mode skipped.</warning>"
                    ));
                    vec![]
                }
                Some(lock) => {
                    let all_names: Vec<String> = lock
                        .packages
                        .iter()
                        .map(|p| p.name.to_lowercase())
                        .chain(
                            lock.packages_dev
                                .iter()
                                .flatten()
                                .map(|p| p.name.to_lowercase()),
                        )
                        .collect();
                    interactive_select_packages(all_names, io.clone())
                }
            }
        } else {
            vec![]
        }
    };

    // Apply partial update (pin non-requested packages) when a subset was named
    if !update_packages.is_empty() {
        match &old_lock {
            None => {
                return Err(mozart_core::exit_code::bail(
                    mozart_core::exit_code::NO_LOCK_FILE_FOR_PARTIAL_UPDATE,
                    "No lock file found. Cannot perform partial update. Run `mozart update` first.",
                ));
            }
            Some(lock) => {
                resolved = apply_partial_update(resolved, lock, &update_packages);
            }
        }
    } else if args.minimal_changes && update_packages.is_empty() && old_lock.is_some() {
        io.lock()
            .unwrap()
            .info("Minimal changes mode: preserving locked versions where possible.");
    }

    // Apply --patch-only filter: restrict updates to patch-level changes only
    if args.patch_only
        && let Some(ref lock) = old_lock
    {
        io.lock()
            .unwrap()
            .info("Patch-only mode: restricting updates to patch-level changes.");
        resolved = apply_patch_only(resolved, lock);
    }

    // Step 9: Generate new lock file. `include_dev: true` matches Composer:
    // `update --no-dev` still writes a complete lock file with packages-dev
    // populated, so a later `install` (with dev_mode) sees them.
    //
    // For partial updates, names NOT in the CLI allow list keep their
    // locked-repo metadata (source/dist references in particular). Computed
    // here from the same `update_packages` list `apply_partial_update` used
    // to swap the resolved versions back. Empty for full updates.
    let lock_pinned_names: IndexSet<String> = if update_packages.is_empty() {
        IndexSet::new()
    } else if let Some(lock) = &old_lock {
        let update_set: IndexSet<String> =
            update_packages.iter().map(|s| s.to_lowercase()).collect();
        lock.packages
            .iter()
            .chain(lock.packages_dev.iter().flatten())
            .map(|p| p.name.to_lowercase())
            .filter(|n| !update_set.contains(n))
            .collect()
    } else {
        IndexSet::new()
    };
    let mut new_lock = lockfile::generate_lock_file(&lockfile::LockFileGenerationRequest {
        resolved_packages: resolved,
        composer_json_content: composer_json_content.clone(),
        composer_json: composer_json_expanded.clone(),
        include_dev: true,
        repositories: repositories.clone(),
        previous_lock: old_lock.clone(),
        lock_pinned_names,
    })
    .await?;

    // In `update mirrors` mode, walk each new lock entry and reset its
    // source/dist references to the old lock's values when the source/dist
    // *types* haven't changed. Mirrors Composer's
    // `LockTransaction::updateMirrorAndUrls`: the URL / mirror block flips
    // to whatever the repository now advertises, but the reference sticks
    // to what was already locked, so a pure URL move (e.g. a repo rename)
    // doesn't masquerade as a content update. When the source or dist type
    // changed (`hg` → `git`, etc.), the new entry is left as-is so the
    // change still emits the install-step Update operation.
    if update_mirrors && let Some(old) = &old_lock {
        apply_mirror_ref_overrides(&mut new_lock, old);
    }

    // Step 10: Compute and print change report
    let changes = compute_update_changes(old_lock.as_ref(), &new_lock, dev_mode);

    let installs: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Install { .. }))
        .collect();
    let updates: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Update { .. }))
        .collect();
    let removals: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Uninstall { .. }))
        .collect();

    io.lock().unwrap().info(&console_format!(
        "<info>Lock file operations: {} install{}, {} update{}, {} removal{}</info>",
        installs.len(),
        if installs.len() == 1 { "" } else { "s" },
        updates.len(),
        if updates.len() == 1 { "" } else { "s" },
        removals.len(),
        if removals.len() == 1 { "" } else { "s" },
    ));

    // Print individual change lines
    for change in &changes {
        match &change.kind {
            ChangeKind::Uninstall { old_version } => {
                if args.dry_run {
                    io.lock().unwrap().info(&console_format!(
                        "  - Would remove <info>{}</info> (<comment>{}</comment>)",
                        change.name,
                        old_version
                    ));
                } else {
                    io.lock().unwrap().info(&console_format!(
                        "  - Removing <info>{}</info> (<comment>{}</comment>)",
                        change.name,
                        old_version
                    ));
                }
            }
            ChangeKind::Install { new_version } => {
                if args.dry_run {
                    io.lock().unwrap().info(&console_format!(
                        "  - Would lock <info>{}</info> (<comment>{}</comment>)",
                        change.name,
                        new_version
                    ));
                } else {
                    io.lock().unwrap().info(&console_format!(
                        "  - Locking <info>{}</info> (<comment>{}</comment>)",
                        change.name,
                        new_version
                    ));
                }
            }
            ChangeKind::Update {
                old_version,
                new_version,
            } => {
                let direction = if is_downgrade(old_version, new_version) {
                    if args.dry_run {
                        "Would downgrade"
                    } else {
                        "Downgrading"
                    }
                } else if args.dry_run {
                    "Would upgrade"
                } else {
                    "Upgrading"
                };
                io.lock().unwrap().info(&console_format!(
                    "  - {} <info>{}</info> (<comment>{}</comment> => <comment>{}</comment>)",
                    direction,
                    change.name,
                    old_version,
                    new_version
                ));
            }
        }
    }

    // Step 11: Write lock file (unless --dry-run)
    if !args.dry_run {
        io.lock()
            .unwrap()
            .info(&console_format!("<info>Writing lock file</info>"));
        new_lock.write_to_file(&lock_path)?;
    }

    // Step 11b: Bump composer.json constraints if --bump-after-update.
    // Mirrors Composer's UpdateCommand::execute 280–299: delegate to BumpCommand::doBump.
    // Only runs when result == 0 (we're here) AND --lock was not set.
    if let Some(ref bump_mode) = args.bump_after_update
        && !args.dry_run
        && !args.lock
    {
        let mode = bump_mode.as_deref().unwrap_or("all");
        let dev_only = mode == "dev";
        let no_dev_only = mode == "no-dev";
        let bump_composer = Composer::require(working_dir)?;
        let bump_exit = super::bump::do_bump(
            io.clone(),
            &bump_composer,
            dev_only,
            no_dev_only,
            false,
            &raw_packages_for_bump,
            "--bump-after-update=dev",
        )
        .await?;
        if bump_exit != 0 {
            return Err(mozart_core::exit_code::bail_silent(bump_exit));
        }
    }

    // Step 12: Install packages (unless --no-install or --dry-run)
    if !args.no_install && !args.dry_run {
        // Determine if prefer-source is enabled
        let prefer_source = args.prefer_source
            || args
                .prefer_install
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("source"))
                .unwrap_or(false);

        super::install::install_from_lock(
            &new_lock,
            working_dir,
            &vendor_dir,
            &super::install::InstallConfig {
                dev_mode,
                dry_run: false, // dry_run already checked above
                no_autoloader: args.no_autoloader,
                no_progress: args.no_progress,
                ignore_platform_reqs: args.ignore_platform_reqs,
                ignore_platform_req: args.ignore_platform_req.clone(),
                optimize_autoloader: args.optimize_autoloader,
                classmap_authoritative: args.classmap_authoritative,
                apcu_autoloader: args.apcu_autoloader || args.apcu_autoloader_prefix.is_some(),
                apcu_autoloader_prefix: args.apcu_autoloader_prefix.clone(),
                download_only: false,
                prefer_source,
            },
            io.clone(),
            executor,
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_locked_package(name: &str, version: &str) -> lockfile::LockedPackage {
        lockfile::LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: Some(format!("{}.0", version)),
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

    fn minimal_lock(packages: Vec<lockfile::LockedPackage>) -> lockfile::LockFile {
        lockfile::LockFile {
            readme: lockfile::LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages,
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

    fn make_resolved_package(name: &str, version: &str) -> ResolvedPackage {
        ResolvedPackage {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: format!("{}.0", version),
            is_dev: false,
            alias_of_normalized: None,
        }
    }

    fn test_console() -> std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>> {
        std::sync::Arc::new(std::sync::Mutex::new(
            Box::new(mozart_core::console::Console::new(
                0, false, false, false, false,
            )) as Box<dyn IoInterface>,
        ))
    }

    #[test]
    fn test_parse_minimum_stability_stable() {
        assert_eq!(
            package::Stability::parse("stable"),
            package::Stability::Stable
        );
        assert_eq!(
            package::Stability::parse("STABLE"),
            package::Stability::Stable
        );
        assert_eq!(
            package::Stability::parse("Stable"),
            package::Stability::Stable
        );
    }

    #[test]
    fn test_parse_minimum_stability_rc() {
        assert_eq!(package::Stability::parse("RC"), package::Stability::RC);
        assert_eq!(package::Stability::parse("rc"), package::Stability::RC);
    }

    #[test]
    fn test_parse_minimum_stability_beta() {
        assert_eq!(package::Stability::parse("beta"), package::Stability::Beta);
        assert_eq!(package::Stability::parse("BETA"), package::Stability::Beta);
    }

    #[test]
    fn test_parse_minimum_stability_alpha() {
        assert_eq!(
            package::Stability::parse("alpha"),
            package::Stability::Alpha
        );
        assert_eq!(
            package::Stability::parse("ALPHA"),
            package::Stability::Alpha
        );
    }

    #[test]
    fn test_parse_minimum_stability_dev() {
        assert_eq!(package::Stability::parse("dev"), package::Stability::Dev);
        assert_eq!(package::Stability::parse("DEV"), package::Stability::Dev);
    }

    #[test]
    fn test_parse_minimum_stability_unknown_defaults_to_stable() {
        assert_eq!(
            package::Stability::parse("unknown"),
            package::Stability::Stable
        );
        assert_eq!(package::Stability::parse(""), package::Stability::Stable);
    }

    #[test]
    fn test_compute_update_changes_all_new() {
        // No old lock: all packages in new lock should be Install
        let new_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);

        let changes = compute_update_changes(None, &new_lock, false);

        assert_eq!(changes.len(), 2);
        for change in &changes {
            assert!(
                matches!(change.kind, ChangeKind::Install { .. }),
                "Expected Install, got {:?} for {}",
                change.kind,
                change.name
            );
        }
    }

    #[test]
    fn test_compute_update_changes_update() {
        // Old lock has psr/log at 3.0.0; new lock has it at 3.0.1 -> Update
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.1")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "psr/log");
        assert!(matches!(
            &changes[0].kind,
            ChangeKind::Update {
                old_version,
                new_version
            } if old_version == "3.0.0" && new_version == "3.0.1"
        ));
    }

    #[test]
    fn test_compute_update_changes_remove() {
        // Old lock has monolog; new lock doesn't -> Remove
        let old_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);
        let new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "monolog/monolog");
        assert!(matches!(
            &changes[0].kind,
            ChangeKind::Uninstall { old_version } if old_version == "3.8.0"
        ));
    }

    #[test]
    fn test_compute_update_changes_unchanged_not_in_result() {
        // Same version in both locks -> no changes
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        assert!(
            changes.is_empty(),
            "Unchanged packages should not appear in changes list"
        );
    }

    #[test]
    fn test_compute_update_changes_mixed() {
        // Mixed scenario: install, update, remove, unchanged
        let old_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),         // unchanged
            make_locked_package("monolog/monolog", "3.7.0"), // will be updated
            make_locked_package("old/package", "1.0.0"),     // will be removed
        ]);
        let new_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),         // unchanged
            make_locked_package("monolog/monolog", "3.8.0"), // updated
            make_locked_package("new/package", "2.0.0"),     // installed
        ]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        // 3 changes: update monolog, remove old/package, install new/package
        assert_eq!(changes.len(), 3);

        let monolog = changes
            .iter()
            .find(|c| c.name == "monolog/monolog")
            .unwrap();
        assert!(matches!(
            &monolog.kind,
            ChangeKind::Update { old_version, new_version }
            if old_version == "3.7.0" && new_version == "3.8.0"
        ));

        let removed = changes.iter().find(|c| c.name == "old/package").unwrap();
        assert!(matches!(&removed.kind, ChangeKind::Uninstall { .. }));

        let installed = changes.iter().find(|c| c.name == "new/package").unwrap();
        assert!(matches!(&installed.kind, ChangeKind::Install { .. }));
    }

    #[test]
    fn test_compute_update_changes_dev_packages_included() {
        // dev_mode=true: dev packages are also compared
        let mut old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        old_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "10.0.0")]);

        let mut new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        new_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "11.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, true);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "phpunit/phpunit");
        assert!(matches!(&changes[0].kind, ChangeKind::Update { .. }));
    }

    #[test]
    fn test_compute_update_changes_dev_packages_excluded_when_no_dev() {
        // dev_mode=false: dev packages are ignored
        let mut old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        old_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "10.0.0")]);

        let mut new_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        new_lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "11.0.0")]);

        let changes = compute_update_changes(Some(&old_lock), &new_lock, false);

        // No changes because we're not including dev packages
        assert!(
            changes.is_empty(),
            "Dev packages should not appear in changes when dev_mode=false"
        );
    }

    #[test]
    fn test_apply_partial_update_keeps_non_specified_packages() {
        // old lock has psr/log 3.0.0 and monolog 3.7.0
        // resolver found psr/log 3.0.1 and monolog 3.8.0
        // we only want to update monolog
        // expected: psr/log stays at 3.0.0, monolog becomes 3.8.0

        let old_lock = minimal_lock(vec![
            make_locked_package("psr/log", "3.0.0"),
            make_locked_package("monolog/monolog", "3.7.0"),
        ]);

        let resolved = vec![
            make_resolved_package("psr/log", "3.0.1"),
            make_resolved_package("monolog/monolog", "3.8.0"),
        ];

        let update_packages = vec!["monolog/monolog".to_string()];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(
            psr.version, "3.0.0",
            "psr/log should be kept at old version"
        );

        let monolog = result.iter().find(|p| p.name == "monolog/monolog").unwrap();
        assert_eq!(
            monolog.version, "3.8.0",
            "monolog/monolog should use new version"
        );
    }

    #[test]
    fn test_apply_partial_update_case_insensitive() {
        // update_packages uses mixed case, package names may be lowercase
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![make_resolved_package("psr/log", "3.0.1")];

        // Not updating psr/log (not in update list); should revert to 3.0.0
        let update_packages = vec!["MonoLog/Monolog".to_string()];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(psr.version, "3.0.0");
    }

    #[test]
    fn test_apply_partial_update_new_package_in_update_list() {
        // A brand new package resolved that is in the update list should use the new version
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![
            make_resolved_package("psr/log", "3.0.0"),
            make_resolved_package("new/package", "1.0.0"),
        ];

        let update_packages = vec!["new/package".to_string()];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        let new_pkg = result.iter().find(|p| p.name == "new/package").unwrap();
        assert_eq!(new_pkg.version, "1.0.0");
    }

    #[test]
    fn test_apply_partial_update_full_update_mode() {
        // If update_packages is empty, it should behave like full update (no swapping)
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![make_resolved_package("psr/log", "3.0.1")];

        // Empty update list means... everything is not in the update set,
        // so old versions are preserved. This is the expected behavior for partial mode
        // when packages is empty (which shouldn't happen - full update is separate path).
        let update_packages: Vec<String> = vec![];
        let result = apply_partial_update(resolved, &old_lock, &update_packages);

        // When update_packages is empty, nothing is in the update set, so old versions revert
        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(psr.version, "3.0.0");
    }

    #[test]
    fn test_glob_matches_exact() {
        assert!(glob_matches("monolog/monolog", "monolog/monolog"));
        assert!(!glob_matches("monolog/monolog", "monolog/logger"));
    }

    #[test]
    fn test_glob_matches_case_insensitive() {
        assert!(glob_matches("Monolog/Monolog", "monolog/monolog"));
        assert!(glob_matches("symfony/*", "Symfony/Console"));
    }

    #[test]
    fn test_glob_matches_vendor_wildcard() {
        assert!(glob_matches("symfony/*", "symfony/console"));
        assert!(glob_matches("symfony/*", "symfony/http-kernel"));
        assert!(!glob_matches("symfony/*", "monolog/monolog"));
    }

    #[test]
    fn test_glob_matches_wildcard_in_name() {
        assert!(glob_matches("monolog/mono*", "monolog/monolog"));
        assert!(!glob_matches("monolog/mono*", "monolog/logger"));
    }

    #[test]
    fn test_glob_matches_wildcard_no_slash() {
        // Without a '/' the pattern still works as a full name match
        assert!(!glob_matches("symfony/*", "monolog/monolog"));
    }

    #[test]
    fn test_glob_matches_different_segment_count() {
        // "vendor/*" has 2 segments; "monolog" has only 1: no match
        assert!(!glob_matches("vendor/*", "monolog"));
        // Pattern with 1 segment vs name with 2 segments: no match
        assert!(!glob_matches("monolog", "monolog/monolog"));
    }

    #[test]
    fn test_expand_wildcards_no_wildcard_passthrough() {
        let lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let root_requires: IndexSet<String> = ["psr/log", "nonexistent/pkg"]
            .into_iter()
            .map(String::from)
            .collect();
        let specs = vec!["psr/log".to_string(), "nonexistent/pkg".to_string()];
        let result = expand_wildcards(&specs, &lock, &root_requires, test_console());
        assert_eq!(result, vec!["psr/log", "nonexistent/pkg"]);
    }

    #[test]
    fn test_expand_wildcards_vendor_star() {
        let lock = minimal_lock(vec![
            make_locked_package("symfony/console", "7.0.0"),
            make_locked_package("symfony/http-kernel", "7.0.0"),
            make_locked_package("monolog/monolog", "3.8.0"),
        ]);
        let specs = vec!["symfony/*".to_string()];
        let root_requires: IndexSet<String> = IndexSet::new();
        let mut result = expand_wildcards(&specs, &lock, &root_requires, test_console());
        result.sort();
        assert_eq!(result, vec!["symfony/console", "symfony/http-kernel"]);
    }

    #[test]
    fn test_expand_wildcards_no_match_emits_warning() {
        let lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let specs = vec!["unknown/*".to_string()];
        let root_requires: IndexSet<String> = IndexSet::new();
        // Should return empty (no match), no panic
        let result = expand_wildcards(&specs, &lock, &root_requires, test_console());
        assert!(result.is_empty());
    }

    #[test]
    fn test_expand_wildcards_deduplication() {
        let lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let specs = vec!["psr/log".to_string(), "psr/log".to_string()];
        let root_requires: IndexSet<String> = IndexSet::new();
        let result = expand_wildcards(&specs, &lock, &root_requires, test_console());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "psr/log");
    }

    #[test]
    fn test_expand_wildcards_also_checks_dev() {
        let mut lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        lock.packages_dev = Some(vec![make_locked_package("phpunit/phpunit", "11.0.0")]);
        let specs = vec!["phpunit/*".to_string()];
        let root_requires: IndexSet<String> = IndexSet::new();
        let result = expand_wildcards(&specs, &lock, &root_requires, test_console());
        assert_eq!(result, vec!["phpunit/phpunit"]);
    }

    #[test]
    fn test_expand_with_direct_deps_adds_require() {
        // monolog/monolog requires psr/log
        let mut pkg = make_locked_package("monolog/monolog", "3.8.0");
        pkg.require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![pkg, make_locked_package("psr/log", "3.0.0")]);

        let result = expand_with_direct_dependencies(
            vec!["monolog/monolog".to_string()],
            &lock,
            &IndexSet::new(),
            &IndexMap::new(),
        );
        let mut result_sorted = result.clone();
        result_sorted.sort();
        assert!(result_sorted.contains(&"monolog/monolog".to_string()));
        assert!(result_sorted.contains(&"psr/log".to_string()));
    }

    #[test]
    fn test_expand_with_direct_deps_skips_platform() {
        let mut pkg = make_locked_package("monolog/monolog", "3.8.0");
        pkg.require.insert("php".to_string(), ">=8.1".to_string());
        pkg.require.insert("ext-json".to_string(), "*".to_string());
        pkg.require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![pkg, make_locked_package("psr/log", "3.0.0")]);

        let result = expand_with_direct_dependencies(
            vec!["monolog/monolog".to_string()],
            &lock,
            &IndexSet::new(),
            &IndexMap::new(),
        );
        // Should NOT include php or ext-json
        assert!(!result.contains(&"php".to_string()));
        assert!(!result.contains(&"ext-json".to_string()));
        assert!(result.contains(&"psr/log".to_string()));
    }

    #[test]
    fn test_expand_with_direct_deps_no_duplicates() {
        // Both packages in the list require psr/log
        let mut pkg_a = make_locked_package("foo/a", "1.0.0");
        pkg_a
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());
        let mut pkg_b = make_locked_package("foo/b", "1.0.0");
        pkg_b
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![pkg_a, pkg_b, make_locked_package("psr/log", "3.0.0")]);

        let result = expand_with_direct_dependencies(
            vec!["foo/a".to_string(), "foo/b".to_string()],
            &lock,
            &IndexSet::new(),
            &IndexMap::new(),
        );
        let psr_count = result.iter().filter(|s| s.as_str() == "psr/log").count();
        assert_eq!(psr_count, 1, "psr/log should appear only once");
    }

    #[test]
    fn test_expand_all_deps_transitive() {
        // a -> b -> c
        let mut pkg_a = make_locked_package("foo/a", "1.0.0");
        pkg_a
            .require
            .insert("foo/b".to_string(), "^1.0".to_string());
        let mut pkg_b = make_locked_package("foo/b", "1.0.0");
        pkg_b
            .require
            .insert("foo/c".to_string(), "^1.0".to_string());
        let pkg_c = make_locked_package("foo/c", "1.0.0");

        let lock = minimal_lock(vec![pkg_a, pkg_b, pkg_c]);

        let result =
            expand_with_all_dependencies(vec!["foo/a".to_string()], &lock, &IndexMap::new());
        assert!(result.contains(&"foo/a".to_string()));
        assert!(result.contains(&"foo/b".to_string()));
        assert!(result.contains(&"foo/c".to_string()));
    }

    #[test]
    fn test_expand_all_deps_no_infinite_loop() {
        // Circular reference: a -> b -> a
        let mut pkg_a = make_locked_package("foo/a", "1.0.0");
        pkg_a
            .require
            .insert("foo/b".to_string(), "^1.0".to_string());
        let mut pkg_b = make_locked_package("foo/b", "1.0.0");
        pkg_b
            .require
            .insert("foo/a".to_string(), "^1.0".to_string());

        let lock = minimal_lock(vec![pkg_a, pkg_b]);

        // Must not loop infinitely
        let result =
            expand_with_all_dependencies(vec!["foo/a".to_string()], &lock, &IndexMap::new());
        assert!(result.contains(&"foo/a".to_string()));
        assert!(result.contains(&"foo/b".to_string()));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_expand_packages_wildcard_with_direct_deps() {
        // symfony/* expands to symfony/console; symfony/console requires psr/log
        let mut console_pkg = make_locked_package("symfony/console", "7.0.0");
        console_pkg
            .require
            .insert("psr/log".to_string(), "^3.0".to_string());

        let lock = minimal_lock(vec![console_pkg, make_locked_package("psr/log", "3.0.0")]);

        let result = expand_packages(
            &["symfony/*".to_string()],
            Some(&lock),
            true,  // with_dependencies
            false, // with_all_dependencies
            &IndexSet::new(),
            &IndexMap::new(),
            test_console(),
        );

        assert!(result.contains(&"symfony/console".to_string()));
        assert!(result.contains(&"psr/log".to_string()));
    }

    #[test]
    fn test_apply_minimal_changes_pins_all() {
        // Resolver found psr/log 3.0.1, but old lock has 3.0.0
        // apply_minimal_changes should pin back to 3.0.0
        let old_lock = minimal_lock(vec![make_locked_package("psr/log", "3.0.0")]);
        let resolved = vec![make_resolved_package("psr/log", "3.0.1")];

        let result = apply_minimal_changes(resolved, &old_lock);
        let psr = result.iter().find(|p| p.name == "psr/log").unwrap();
        assert_eq!(
            psr.version, "3.0.0",
            "minimal-changes should pin to locked version"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_update_full_e2e() {
        use mozart_core::package::RawPackageData;
        use mozart_core::repository::lockfile::{LockFileGenerationRequest, generate_lock_file};
        use mozart_core::repository::resolver::{ResolveRequest, resolve};

        let composer_json_content =
            r#"{"name": "test/project", "require": {"monolog/monolog": "^3.0"}}"#;
        let composer_json: RawPackageData = serde_json::from_str(composer_json_content).unwrap();

        let request = ResolveRequest {
            root_name: String::new(),
            root_version: None,
            require: vec![("monolog/monolog".to_string(), "^3.0".to_string())],
            require_dev: vec![],
            include_dev: false,
            minimum_stability: package::Stability::Stable,
            stability_flags: IndexMap::new(),
            prefer_stable: true,
            prefer_lowest: false,
            platform: PlatformConfig::new(),
            ignore_platform_reqs: false,
            ignore_platform_req_list: vec![],
            repositories: std::sync::Arc::new(
                mozart_core::repository::repository::RepositorySet::with_packagist(
                    mozart_core::repository::cache::Cache::new(
                        std::env::temp_dir().join("mozart-test-cache"),
                        false,
                    ),
                ),
            ),
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
            block_insecure: false,
        };

        let resolved = resolve(&request).await.expect("Resolution should succeed");
        assert!(!resolved.is_empty());
        assert!(resolved.iter().any(|p| p.name == "monolog/monolog"));

        let lock = generate_lock_file(&LockFileGenerationRequest {
            resolved_packages: resolved,
            composer_json_content: composer_json_content.to_string(),
            composer_json,
            include_dev: false,
            repositories: std::sync::Arc::new(
                mozart_core::repository::repository::RepositorySet::with_packagist(
                    mozart_core::repository::cache::Cache::new(
                        std::env::temp_dir().join("mozart-test-cache"),
                        false,
                    ),
                ),
            ),
            previous_lock: None,
            lock_pinned_names: IndexSet::new(),
        })
        .await
        .expect("Lock file generation should succeed");

        assert!(!lock.content_hash.is_empty());
        assert!(!lock.packages.is_empty());
        assert!(lock.packages.iter().any(|p| p.name == "monolog/monolog"));
    }
}
