//! Mirrors `Composer\Util\PackageInfo`.
//!
//! The PHP class exposes two small static helpers that the `licenses`,
//! `show`, `outdated`, and `funding` commands lean on to produce a
//! "view source" link for a package — preferring the explicit
//! `support.source` URL, falling back to the package's `source` URL,
//! and finally the homepage. Empty strings are normalised to `None`.

/// Minimal contract for the package fields [`view_source_url`] and
/// [`view_source_or_homepage_url`] consult.
pub trait PackageUrls {
    /// `support.source` from `composer.json` (mirrors
    /// `CompletePackageInterface::getSupport()['source']`). Returns
    /// `None` when the support block is absent or the `source` key is
    /// missing.
    fn support_source(&self) -> Option<&str>;
    /// `source.url` (mirrors `PackageInterface::getSourceUrl()`).
    fn source_url(&self) -> Option<&str>;
    /// `homepage` (mirrors `CompletePackageInterface::getHomepage()`).
    fn homepage(&self) -> Option<&str>;
}

/// Mirror of `PackageInfo::getViewSourceUrl`.
///
/// PHP returns the support-source URL when it is set and non-empty,
/// otherwise `getSourceUrl()`. Empty strings are treated as absent.
pub fn view_source_url<P: PackageUrls + ?Sized>(package: &P) -> Option<String> {
    if let Some(s) = package.support_source().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    package
        .source_url()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Mirror of `PackageInfo::getViewSourceOrHomepageUrl`.
///
/// Falls back to the package homepage when no source URL is available.
/// An empty homepage string is normalised to `None`, matching PHP's
/// `if ($url === '') { return null; }` guard.
pub fn view_source_or_homepage_url<P: PackageUrls + ?Sized>(package: &P) -> Option<String> {
    view_source_url(package)
        .or_else(|| package.homepage().map(String::from))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct P {
        support_source: Option<String>,
        source_url: Option<String>,
        homepage: Option<String>,
    }

    impl PackageUrls for P {
        fn support_source(&self) -> Option<&str> {
            self.support_source.as_deref()
        }
        fn source_url(&self) -> Option<&str> {
            self.source_url.as_deref()
        }
        fn homepage(&self) -> Option<&str> {
            self.homepage.as_deref()
        }
    }

    #[test]
    fn prefers_support_source() {
        let p = P {
            support_source: Some("https://github.com/foo/bar".to_string()),
            source_url: Some("https://example.com/repo".to_string()),
            ..Default::default()
        };
        assert_eq!(
            view_source_url(&p).as_deref(),
            Some("https://github.com/foo/bar")
        );
    }

    #[test]
    fn empty_support_source_falls_through_to_source_url() {
        let p = P {
            support_source: Some(String::new()),
            source_url: Some("https://example.com/repo".to_string()),
            ..Default::default()
        };
        assert_eq!(
            view_source_url(&p).as_deref(),
            Some("https://example.com/repo")
        );
    }

    #[test]
    fn falls_back_to_homepage() {
        let p = P {
            homepage: Some("https://example.com/".to_string()),
            ..Default::default()
        };
        assert_eq!(
            view_source_or_homepage_url(&p).as_deref(),
            Some("https://example.com/")
        );
    }

    #[test]
    fn empty_homepage_is_none() {
        let p = P {
            homepage: Some(String::new()),
            ..Default::default()
        };
        assert!(view_source_or_homepage_url(&p).is_none());
    }

    #[test]
    fn no_urls_at_all_returns_none() {
        let p = P::default();
        assert!(view_source_url(&p).is_none());
        assert!(view_source_or_homepage_url(&p).is_none());
    }
}
