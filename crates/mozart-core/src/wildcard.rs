/// Match a package name against a wildcard pattern (case-insensitive).
/// `*` matches any sequence of characters.
pub fn matches_wildcard(name: &str, pattern: &str) -> bool {
    let name_lower = name.to_lowercase();
    let pattern_lower = pattern.to_lowercase();
    let parts: Vec<&str> = pattern_lower.split('*').collect();

    if parts.len() == 1 {
        return name_lower == pattern_lower;
    }

    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match name_lower[pos..].find(*part) {
            Some(found) => {
                if i == 0 && found != 0 {
                    return false; // First segment must match at start
                }
                pos += found + part.len();
            }
            None => return false,
        }
    }

    // If pattern doesn't end with *, name must be fully consumed
    if !pattern_lower.ends_with('*') {
        return pos == name_lower.len();
    }

    true
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_wildcard_exact() {
        assert!(matches_wildcard("psr/log", "psr/log"));
    }

    #[test]
    fn test_matches_wildcard_star_end() {
        assert!(matches_wildcard("psr/log", "psr/*"));
    }

    #[test]
    fn test_matches_wildcard_star_start() {
        assert!(matches_wildcard("psr/log", "*/log"));
    }

    #[test]
    fn test_matches_wildcard_star_middle() {
        assert!(matches_wildcard("monolog/monolog", "mono*/mono*"));
    }

    #[test]
    fn test_matches_wildcard_no_match() {
        assert!(!matches_wildcard("psr/log", "symfony/*"));
    }

    #[test]
    fn test_matches_wildcard_case_insensitive() {
        assert!(matches_wildcard("PSR/Log", "psr/*"));
    }

    #[test]
    fn test_matches_wildcard_star_both_ends() {
        assert!(matches_wildcard("monolog/monolog", "*log*"));
    }

    #[test]
    fn test_matches_wildcard_no_wildcard_mismatch() {
        assert!(!matches_wildcard("psr/log", "psr/log2"));
    }

    #[test]
    fn test_matches_wildcard_trailing_chars_fail() {
        assert!(!matches_wildcard("psr/log", "psr/l"));
    }
}
