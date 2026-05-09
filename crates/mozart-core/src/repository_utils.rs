//! Mirrors `Composer\Repository\RepositoryUtils`.
//!
//! Currently ports `filterRequiredPackages` only; `flattenRepositories`
//! has no Mozart equivalent yet because Mozart does not model nested
//! `CompositeRepository`/`FilterRepository` structures.

use std::collections::BTreeSet;

/// Minimal contract for a package that can participate in the require
/// closure walk performed by [`filter_required_packages`].
///
/// `package_name` is the normalized `vendor/name`. `requires` returns
/// the require map (`name → constraint`); only the keys are consulted.
/// `package_names` returns every name the package answers for —
/// typically just `package_name`, but Composer's `PackageInterface::getNames()`
/// also includes `provide`/`replace` targets. Implementations may
/// return `None` when those auxiliary names are not yet modelled in
/// Mozart's data layer; the walk falls back to matching on
/// `package_name` only in that case.
pub trait Required {
    fn package_name(&self) -> &str;
    fn requires(&self) -> &std::collections::BTreeMap<String, String>;
    fn package_names(&self) -> Option<Vec<&str>> {
        None
    }
}

/// Mirror of `RepositoryUtils::filterRequiredPackages`.
///
/// Walks the require closure of `requirer_requires` against `packages`,
/// collecting (in input order) every package that is reachable.
/// `requirer_dev_requires`, when `Some`, is merged into the initial
/// require set — matching the `$includeRequireDev` flag, which Composer
/// only honours for the *initial* requirer (transitive walks always
/// look at `getRequires()` only).
///
/// The returned vector preserves the order in which packages were
/// discovered, matching PHP's `$bucket[] = $candidate;` push pattern.
pub fn filter_required_packages<P, V>(
    packages: &[P],
    requirer_requires: &std::collections::BTreeMap<String, V>,
    requirer_dev_requires: Option<&std::collections::BTreeMap<String, V>>,
) -> Vec<usize>
where
    P: Required,
{
    let mut initial: BTreeSet<&str> = requirer_requires.keys().map(String::as_str).collect();
    if let Some(dev) = requirer_dev_requires {
        initial.extend(dev.keys().map(String::as_str));
    }

    let mut bucket: Vec<usize> = Vec::new();
    walk(packages, &initial, &mut bucket);
    bucket
}

fn walk<P>(packages: &[P], requires: &BTreeSet<&str>, bucket: &mut Vec<usize>)
where
    P: Required,
{
    for (idx, candidate) in packages.iter().enumerate() {
        let names: Vec<&str> = candidate
            .package_names()
            .unwrap_or_else(|| vec![candidate.package_name()]);
        let matches = names.iter().any(|n| requires.contains(n));
        if !matches {
            continue;
        }
        if bucket.contains(&idx) {
            continue;
        }
        bucket.push(idx);
        let next: BTreeSet<&str> = candidate.requires().keys().map(String::as_str).collect();
        walk(packages, &next, bucket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Pkg {
        name: String,
        requires: BTreeMap<String, String>,
    }

    impl Required for Pkg {
        fn package_name(&self) -> &str {
            &self.name
        }
        fn requires(&self) -> &BTreeMap<String, String> {
            &self.requires
        }
    }

    fn pkg(name: &str, requires: &[&str]) -> Pkg {
        let mut r = BTreeMap::new();
        for n in requires {
            r.insert(n.to_string(), "*".to_string());
        }
        Pkg {
            name: name.to_string(),
            requires: r,
        }
    }

    fn root_requires(names: &[&str]) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for n in names {
            m.insert(n.to_string(), "*".to_string());
        }
        m
    }

    #[test]
    fn filters_to_root_requires_only() {
        let packages = vec![
            pkg("a/a", &[]),
            pkg("b/b", &[]),
            pkg("c/c", &[]), // not required
        ];
        let root = root_requires(&["a/a", "b/b"]);
        let kept = filter_required_packages(&packages, &root, None);
        let names: Vec<&str> = kept.iter().map(|&i| packages[i].name.as_str()).collect();
        assert_eq!(names, vec!["a/a", "b/b"]);
    }

    #[test]
    fn walks_transitive_requires() {
        let packages = vec![
            pkg("a/a", &["b/b"]),
            pkg("b/b", &["c/c"]),
            pkg("c/c", &[]),
            pkg("d/d", &[]), // unreachable
        ];
        let root = root_requires(&["a/a"]);
        let kept = filter_required_packages(&packages, &root, None);
        let names: Vec<&str> = kept.iter().map(|&i| packages[i].name.as_str()).collect();
        assert_eq!(names, vec!["a/a", "b/b", "c/c"]);
    }

    #[test]
    fn dev_requires_only_apply_at_root() {
        let packages = vec![
            pkg("a/a", &[]),
            pkg("b/b", &["c/c"]),
            pkg("c/c", &[]), // only reachable via a's dev-requires (no dev requires here)
            pkg("d/d", &[]),
        ];
        let root = root_requires(&["a/a"]);
        let dev = root_requires(&["b/b"]);
        let kept = filter_required_packages(&packages, &root, Some(&dev));
        let names: Vec<&str> = kept.iter().map(|&i| packages[i].name.as_str()).collect();
        assert_eq!(names, vec!["a/a", "b/b", "c/c"]);
    }

    #[test]
    fn handles_circular_requires() {
        let packages = vec![pkg("a/a", &["b/b"]), pkg("b/b", &["a/a"])];
        let root = root_requires(&["a/a"]);
        let kept = filter_required_packages(&packages, &root, None);
        let names: Vec<&str> = kept.iter().map(|&i| packages[i].name.as_str()).collect();
        assert_eq!(names, vec!["a/a", "b/b"]);
    }

    #[test]
    fn empty_requires_yields_nothing() {
        let packages = vec![pkg("a/a", &[]), pkg("b/b", &[])];
        let root: BTreeMap<String, String> = BTreeMap::new();
        let kept = filter_required_packages(&packages, &root, None);
        assert!(kept.is_empty());
    }
}
