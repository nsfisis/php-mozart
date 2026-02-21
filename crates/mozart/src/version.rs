use crate::package::Stability;
use crate::packagist::PackagistVersion;
use std::cmp::Ordering;

/// Determine the stability of a normalized version string.
pub fn stability_of(version_normalized: &str) -> Stability {
    let v = version_normalized.to_lowercase();
    if v.starts_with("dev-") || v.ends_with("-dev") {
        return Stability::Dev;
    }
    // Check for pre-release suffixes: alpha, beta, RC
    // Normalized versions use formats like "1.0.0.0-alpha1", "1.0.0.0-beta2", "1.0.0.0-RC1"
    if let Some(pos) = v.rfind('-') {
        let suffix = &v[pos + 1..];
        if suffix.starts_with("alpha") {
            return Stability::Alpha;
        }
        if suffix.starts_with("beta") {
            return Stability::Beta;
        }
        if suffix.starts_with("rc") || suffix.starts_with("RC") {
            return Stability::RC;
        }
    }
    Stability::Stable
}

/// Compare two normalized version strings (e.g. "1.2.3.0" vs "1.2.4.0").
///
/// Each version is split into numeric parts. Non-numeric suffixes (like "-beta1")
/// are handled by treating the base parts as numeric and the suffix separately.
pub fn compare_normalized_versions(a: &str, b: &str) -> Ordering {
    let parse = |v: &str| -> (Vec<u64>, Option<String>) {
        // Split off any pre-release suffix
        let (base, suffix) = if let Some(pos) = v.find('-') {
            (&v[..pos], Some(v[pos + 1..].to_string()))
        } else {
            (v, None)
        };
        let parts: Vec<u64> = base.split('.').filter_map(|p| p.parse().ok()).collect();
        (parts, suffix)
    };

    let (a_parts, a_suffix) = parse(a);
    let (b_parts, b_suffix) = parse(b);

    // Compare numeric parts
    let max_len = a_parts.len().max(b_parts.len());
    for i in 0..max_len {
        let a_val = a_parts.get(i).copied().unwrap_or(0);
        let b_val = b_parts.get(i).copied().unwrap_or(0);
        match a_val.cmp(&b_val) {
            Ordering::Equal => continue,
            other => return other,
        }
    }

    // If numeric parts are equal, compare stability
    // A stable version (no suffix) is greater than a pre-release
    match (&a_suffix, &b_suffix) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater, // stable > pre-release
        (Some(_), None) => Ordering::Less,    // pre-release < stable
        (Some(a_s), Some(b_s)) => {
            let stab_a = stability_of(&format!("0.0.0.0-{a_s}"));
            let stab_b = stability_of(&format!("0.0.0.0-{b_s}"));
            // Lower stability value = more stable = greater version
            match stab_a.cmp(&stab_b) {
                Ordering::Equal => a_s.cmp(b_s),
                // Stability enum: Stable(0) < RC(5) < Beta(10) < Alpha(15) < Dev(20)
                // But more stable = higher version, so we reverse
                Ordering::Less => Ordering::Greater,
                Ordering::Greater => Ordering::Less,
            }
        }
    }
}

/// Find the best version candidate given a preferred minimum stability.
///
/// Returns the highest version whose stability is at least as stable as
/// the preferred stability (i.e., stability value <= preferred value).
pub fn find_best_candidate(
    versions: &[PackagistVersion],
    preferred_stability: Stability,
) -> Option<&PackagistVersion> {
    versions
        .iter()
        .filter(|v| stability_of(&v.version_normalized) <= preferred_stability)
        .max_by(|a, b| compare_normalized_versions(&a.version_normalized, &b.version_normalized))
}

/// Generate a recommended version constraint string from a concrete version.
///
/// Examples:
/// - `"1.2.1"` (stable) → `"^1.2"`
/// - `"0.3.5"` (stable) → `"^0.3"`
/// - `"2.0.0-beta.1"` (beta) → `"^2.0@beta"`
/// - `"dev-master"` (dev) → `"dev-master"`
pub fn find_recommended_require_version(
    version: &str,
    version_normalized: &str,
    stability: Stability,
) -> String {
    // dev branches are returned as-is
    if stability == Stability::Dev {
        return version.to_string();
    }

    // Extract major.minor from the normalized version (e.g. "1.2.3.0" → "1.2")
    let base = if let Some(pos) = version_normalized.find('-') {
        &version_normalized[..pos]
    } else {
        version_normalized
    };

    let parts: Vec<&str> = base.split('.').collect();
    let major = parts.first().copied().unwrap_or("0");
    let minor = parts.get(1).copied().unwrap_or("0");

    let constraint = format!("^{major}.{minor}");

    match stability {
        Stability::Stable => constraint,
        Stability::RC => format!("{constraint}@RC"),
        Stability::Beta => format!("{constraint}@beta"),
        Stability::Alpha => format!("{constraint}@alpha"),
        Stability::Dev => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stability_of() {
        assert_eq!(stability_of("1.0.0.0"), Stability::Stable);
        assert_eq!(stability_of("2.3.1.0"), Stability::Stable);
        assert_eq!(stability_of("1.0.0.0-alpha1"), Stability::Alpha);
        assert_eq!(stability_of("1.0.0.0-beta2"), Stability::Beta);
        assert_eq!(stability_of("1.0.0.0-RC1"), Stability::RC);
        assert_eq!(stability_of("dev-master"), Stability::Dev);
        assert_eq!(stability_of("dev-feature/foo"), Stability::Dev);
        assert_eq!(stability_of("1.0.0.0-dev"), Stability::Dev);
    }

    #[test]
    fn test_compare_normalized_versions() {
        assert_eq!(
            compare_normalized_versions("1.0.0.0", "1.0.0.0"),
            Ordering::Equal
        );
        assert_eq!(
            compare_normalized_versions("2.0.0.0", "1.0.0.0"),
            Ordering::Greater
        );
        assert_eq!(
            compare_normalized_versions("1.0.0.0", "2.0.0.0"),
            Ordering::Less
        );
        assert_eq!(
            compare_normalized_versions("1.2.0.0", "1.1.0.0"),
            Ordering::Greater
        );
        assert_eq!(
            compare_normalized_versions("1.0.0.0", "1.0.0.0-beta1"),
            Ordering::Greater
        );
        assert_eq!(
            compare_normalized_versions("1.0.0.0-RC1", "1.0.0.0-beta1"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_find_best_candidate_stable() {
        let versions = vec![
            PackagistVersion {
                version: "dev-master".to_string(),
                version_normalized: "dev-master".to_string(),
                require: Default::default(),
                dist: None,
                source: None,
            },
            PackagistVersion {
                version: "2.0.0-beta.1".to_string(),
                version_normalized: "2.0.0.0-beta1".to_string(),
                require: Default::default(),
                dist: None,
                source: None,
            },
            PackagistVersion {
                version: "1.5.0".to_string(),
                version_normalized: "1.5.0.0".to_string(),
                require: Default::default(),
                dist: None,
                source: None,
            },
            PackagistVersion {
                version: "1.4.0".to_string(),
                version_normalized: "1.4.0.0".to_string(),
                require: Default::default(),
                dist: None,
                source: None,
            },
        ];

        let best = find_best_candidate(&versions, Stability::Stable).unwrap();
        assert_eq!(best.version, "1.5.0");
    }

    #[test]
    fn test_find_best_candidate_beta() {
        let versions = vec![
            PackagistVersion {
                version: "dev-master".to_string(),
                version_normalized: "dev-master".to_string(),
                require: Default::default(),
                dist: None,
                source: None,
            },
            PackagistVersion {
                version: "2.0.0-beta.1".to_string(),
                version_normalized: "2.0.0.0-beta1".to_string(),
                require: Default::default(),
                dist: None,
                source: None,
            },
            PackagistVersion {
                version: "1.5.0".to_string(),
                version_normalized: "1.5.0.0".to_string(),
                require: Default::default(),
                dist: None,
                source: None,
            },
        ];

        let best = find_best_candidate(&versions, Stability::Beta).unwrap();
        assert_eq!(best.version, "2.0.0-beta.1");
    }

    #[test]
    fn test_find_best_candidate_no_match() {
        let versions = vec![PackagistVersion {
            version: "dev-master".to_string(),
            version_normalized: "dev-master".to_string(),
            require: Default::default(),
            dist: None,
            source: None,
        }];

        let best = find_best_candidate(&versions, Stability::Stable);
        assert!(best.is_none());
    }

    #[test]
    fn test_find_recommended_require_version() {
        // Stable
        assert_eq!(
            find_recommended_require_version("1.2.1", "1.2.1.0", Stability::Stable),
            "^1.2"
        );
        assert_eq!(
            find_recommended_require_version("0.3.5", "0.3.5.0", Stability::Stable),
            "^0.3"
        );

        // Beta
        assert_eq!(
            find_recommended_require_version("2.0.0-beta.1", "2.0.0.0-beta1", Stability::Beta),
            "^2.0@beta"
        );

        // RC
        assert_eq!(
            find_recommended_require_version("3.0.0-RC1", "3.0.0.0-RC1", Stability::RC),
            "^3.0@RC"
        );

        // Dev
        assert_eq!(
            find_recommended_require_version("dev-master", "dev-master", Stability::Dev),
            "dev-master"
        );
    }
}
