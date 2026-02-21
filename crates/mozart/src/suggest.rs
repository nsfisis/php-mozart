//! Fuzzy package name suggestions using Levenshtein distance.
//!
//! Used to provide "Did you mean ...?" hints when a user types a package name
//! that does not exist in the installed packages or in the require/require-dev
//! sections of composer.json.

/// Compute the Levenshtein edit distance between two strings.
///
/// This is a standard dynamic-programming implementation that runs in O(m*n)
/// time and O(min(m,n)) space.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();

    let m = a.len();
    let n = b.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    // Use two alternating rows to save memory.
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr: Vec<usize> = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1) // deletion
                .min(curr[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

/// Maximum edit distance for a suggestion to be considered "similar".
///
/// Packages with Levenshtein distance greater than this threshold are not
/// returned as suggestions.
const MAX_DISTANCE: usize = 5;

/// Find package names from `candidates` that are similar to `query`.
///
/// Returns a list of `(distance, name)` pairs sorted by ascending distance,
/// then ascending name for stability.  Only candidates with a Levenshtein
/// distance <= [`MAX_DISTANCE`] are returned.
pub fn find_similar<'a>(
    query: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Vec<(usize, &'a str)> {
    let query_lower = query.to_lowercase();
    let mut results: Vec<(usize, &'a str)> = candidates
        .filter_map(|name| {
            let dist = levenshtein(&query_lower, &name.to_lowercase());
            if dist <= MAX_DISTANCE && dist > 0 {
                Some((dist, name))
            } else {
                None
            }
        })
        .collect();

    results.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    results
}

/// Format a "Did you mean ...?" message from a list of suggestions.
///
/// Returns `None` when `suggestions` is empty.
///
/// # Examples
///
/// ```
/// use mozart::suggest::format_did_you_mean;
/// let msg = format_did_you_mean(&["psr/log", "psr/cache"]);
/// assert!(msg.unwrap().contains("Did you mean"));
/// ```
pub fn format_did_you_mean(suggestions: &[&str]) -> Option<String> {
    if suggestions.is_empty() {
        return None;
    }

    let formatted = suggestions
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(" or ");

    Some(format!("Did you mean {}?", formatted))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── levenshtein ───────────────────────────────────────────────────────────

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("psr/log", "psr/log"), 0);
    }

    #[test]
    fn test_levenshtein_empty_left() {
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn test_levenshtein_empty_right() {
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn test_levenshtein_both_empty() {
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn test_levenshtein_single_insertion() {
        assert_eq!(levenshtein("psr/log", "psr/logs"), 1);
    }

    #[test]
    fn test_levenshtein_single_deletion() {
        assert_eq!(levenshtein("psr/logs", "psr/log"), 1);
    }

    #[test]
    fn test_levenshtein_single_substitution() {
        assert_eq!(levenshtein("psr/log", "psr/lag"), 1);
    }

    #[test]
    fn test_levenshtein_completely_different() {
        assert_eq!(levenshtein("abc", "xyz"), 3);
    }

    #[test]
    fn test_levenshtein_package_names() {
        // "monolog/monolog" vs "monolong/monolog" — 1 insertion
        assert_eq!(levenshtein("monolog/monolog", "monolong/monolog"), 1);
    }

    // ── find_similar ──────────────────────────────────────────────────────────

    #[test]
    fn test_find_similar_returns_close_matches() {
        let candidates = ["psr/log", "psr/cache", "monolog/monolog", "symfony/console"];
        let results = find_similar("psr/lod", candidates.iter().copied());
        assert!(!results.is_empty());
        // "psr/log" has distance 1 from "psr/lod"
        assert_eq!(results[0].1, "psr/log");
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn test_find_similar_excludes_exact_match() {
        let candidates = ["psr/log", "psr/cache"];
        // Exact match should not appear (distance == 0)
        let results = find_similar("psr/log", candidates.iter().copied());
        assert!(!results.iter().any(|(_, name)| *name == "psr/log"));
    }

    #[test]
    fn test_find_similar_excludes_too_distant() {
        let candidates = ["completely/different", "another/package"];
        let results = find_similar("psr/log", candidates.iter().copied());
        // All candidates are more than MAX_DISTANCE away
        assert!(results.is_empty());
    }

    #[test]
    fn test_find_similar_sorted_by_distance() {
        let candidates = ["psr/log", "psr/logs", "psr/logsx"];
        // "psr/lod" -> "psr/log" distance 1, "psr/logs" distance 2, "psr/logsx" distance 3
        let results = find_similar("psr/lod", candidates.iter().copied());
        if results.len() >= 2 {
            assert!(results[0].0 <= results[1].0);
        }
    }

    #[test]
    fn test_find_similar_case_insensitive() {
        let candidates = ["PSR/Log"];
        let results = find_similar("psr/log", candidates.iter().copied());
        // "psr/log" vs "psr/log" (both lowercased) = distance 0, so excluded
        assert!(results.is_empty());
    }

    // ── format_did_you_mean ───────────────────────────────────────────────────

    #[test]
    fn test_format_did_you_mean_empty() {
        assert!(format_did_you_mean(&[]).is_none());
    }

    #[test]
    fn test_format_did_you_mean_single() {
        let msg = format_did_you_mean(&["psr/log"]).unwrap();
        assert_eq!(msg, "Did you mean \"psr/log\"?");
    }

    #[test]
    fn test_format_did_you_mean_multiple() {
        let msg = format_did_you_mean(&["psr/log", "psr/cache"]).unwrap();
        assert!(msg.contains("Did you mean"));
        assert!(msg.contains("\"psr/log\""));
        assert!(msg.contains("\"psr/cache\""));
        assert!(msg.contains(" or "));
    }
}
