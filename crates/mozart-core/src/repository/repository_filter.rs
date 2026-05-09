//! Repository-level package filters (`only`, `exclude`, `canonical`).
//!
//! Mirrors `Composer\Repository\FilterRepository`: a wrapper around an
//! underlying repository that drops packages by name and/or removes the
//! repo's authoritative claim on the names it serves. We model the same
//! semantics for inline `type: package` and local `type: composer`
//! repositories, since the installer fixtures rely on them.

use crate::package::RawRepository;
use regex::Regex;

/// Resolved filter for a single `repositories[]` entry.
pub struct RepositoryFilter {
    only: Option<Regex>,
    exclude: Option<Regex>,
    /// `canonical: true` (default) — packages from this repo claim their
    /// names, suppressing lower-priority repos for the same name.
    /// `canonical: false` — packages enter the pool but lower-priority
    /// repos may also answer.
    pub canonical: bool,
}

impl RepositoryFilter {
    pub fn from_repo(repo: &RawRepository) -> Self {
        Self {
            only: repo.only.as_ref().and_then(|names| build_name_regex(names)),
            exclude: repo
                .exclude
                .as_ref()
                .and_then(|names| build_name_regex(names)),
            canonical: repo.canonical.unwrap_or(true),
        }
    }

    /// `true` if `name` may pass through this filter.
    /// Mirrors `FilterRepository::isAllowed`.
    pub fn is_allowed(&self, name: &str) -> bool {
        if let Some(only) = &self.only {
            return only.is_match(name);
        }
        if let Some(exclude) = &self.exclude {
            return !exclude.is_match(name);
        }
        true
    }
}

/// Build a case-insensitive `^(?:p1|p2|…)$` regex from Composer's pattern
/// list. Mirrors `BasePackage::packageNamesToRegexp` — `*` becomes `.*`,
/// every other regex metacharacter is escaped, and the alternation is
/// anchored to the full string.
fn build_name_regex(patterns: &[String]) -> Option<Regex> {
    if patterns.is_empty() {
        return None;
    }
    let parts: Vec<String> = patterns.iter().map(|p| pattern_to_regex(p)).collect();
    let joined = parts.join("|");
    Regex::new(&format!(r"(?i)^(?:{joined})$")).ok()
}

fn pattern_to_regex(pattern: &str) -> String {
    let escaped = regex::escape(pattern);
    // `*` was escaped to `\*` — turn it into `.*` so glob semantics match
    // Composer.
    escaped.replace(r"\*", ".*")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(
        only: Option<Vec<String>>,
        exclude: Option<Vec<String>>,
        canonical: Option<bool>,
    ) -> RawRepository {
        RawRepository {
            repo_type: "package".to_string(),
            url: None,
            package: None,
            only,
            exclude,
            canonical,
            security_advisories: None,
        }
    }

    #[test]
    fn no_filter_allows_all() {
        let f = RepositoryFilter::from_repo(&repo(None, None, None));
        assert!(f.is_allowed("a/a"));
        assert!(f.is_allowed("foo/bar"));
        assert!(f.canonical);
    }

    #[test]
    fn only_restricts_to_listed_names() {
        let f = RepositoryFilter::from_repo(&repo(Some(vec!["foo/b".to_string()]), None, None));
        assert!(f.is_allowed("foo/b"));
        assert!(!f.is_allowed("foo/a"));
    }

    #[test]
    fn exclude_drops_listed_names() {
        let f = RepositoryFilter::from_repo(&repo(None, Some(vec!["foo/c".to_string()]), None));
        assert!(f.is_allowed("foo/a"));
        assert!(!f.is_allowed("foo/c"));
    }

    #[test]
    fn glob_star_expands() {
        let f = RepositoryFilter::from_repo(&repo(Some(vec!["foo/*".to_string()]), None, None));
        assert!(f.is_allowed("foo/a"));
        assert!(f.is_allowed("foo/anything"));
        assert!(!f.is_allowed("bar/a"));
    }

    #[test]
    fn match_is_case_insensitive() {
        let f = RepositoryFilter::from_repo(&repo(Some(vec!["Foo/Bar".to_string()]), None, None));
        assert!(f.is_allowed("foo/bar"));
        assert!(f.is_allowed("FOO/BAR"));
    }

    #[test]
    fn canonical_default_is_true() {
        let f = RepositoryFilter::from_repo(&repo(None, None, None));
        assert!(f.canonical);
    }

    #[test]
    fn canonical_false_honored() {
        let f = RepositoryFilter::from_repo(&repo(None, None, Some(false)));
        assert!(!f.canonical);
    }
}
