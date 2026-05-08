//! Mirrors `Composer\Util\PackageSorter`.
//!
//! Composer's helper takes `PackageInterface[]` and sorts in place by
//! `getName()` (the lowercase normalized name), case-sensitive `<=>`.
//! Mozart commands hold a variety of package representations
//! (`InstalledPackageEntry`, `LockedPackage`, `PackageData`, …); rather
//! than force them all behind one trait, the sorter accepts a key
//! extractor closure and is generic over the slice element type.

/// Mirror of `PackageSorter::sortPackagesAlphabetically`.
///
/// Composer compares with `getName() <=> getName()`. `getName()` returns
/// the normalized (lowercase) `vendor/name`, so the sort is effectively
/// case-insensitive on the original casing but case-sensitive on the
/// already-normalized form. Use a key extractor that returns the
/// normalized name to match.
pub fn sort_packages_alphabetically<T, F>(packages: &mut [T], name: F)
where
    F: Fn(&T) -> &str,
{
    packages.sort_by(|a, b| name(a).cmp(name(b)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct P(&'static str);

    #[test]
    fn sorts_by_name_ascending() {
        let mut v = vec![P("monolog/monolog"), P("psr/log"), P("a/b")];
        sort_packages_alphabetically(&mut v, |p| p.0);
        assert_eq!(v, vec![P("a/b"), P("monolog/monolog"), P("psr/log")]);
    }

    #[test]
    fn empty_slice_is_noop() {
        let mut v: Vec<P> = vec![];
        sort_packages_alphabetically(&mut v, |p| p.0);
        assert!(v.is_empty());
    }

    #[test]
    fn case_sensitive_on_normalized_form() {
        // Already-lowercase names — Composer stores `getName()` lowercased,
        // so we never mix cases here, but verify ordering is plain `<=>`.
        let mut v = vec![P("zzz/x"), P("aaa/x"), P("aaa/y")];
        sort_packages_alphabetically(&mut v, |p| p.0);
        assert_eq!(v, vec![P("aaa/x"), P("aaa/y"), P("zzz/x")]);
    }
}
