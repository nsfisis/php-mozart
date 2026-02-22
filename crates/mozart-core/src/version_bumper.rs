/// Version constraint bumper.
///
/// Given a constraint string (from composer.json) and the installed version
/// (from composer.lock), computes a new constraint string that raises the
/// lower bound to match the installed version.
///
/// Returns `None` if no change is needed, or `Some(new_constraint)` if the
/// constraint should be updated.
pub fn bump_requirement(
    constraint_str: &str,
    pretty_version: &str,
    version_normalized: Option<&str>,
) -> Option<String> {
    let constraint = constraint_str.trim();

    // Strip and preserve stability flag (@dev, @beta, etc.)
    let (constraint_body, stability_flag) = strip_stability_flag(constraint);

    // Dev constraints (dev-master, dev-main, etc.) are left unchanged
    if constraint_body.trim().starts_with("dev-") {
        return None;
    }

    // Skip dev installed versions that have no alias
    // An alias looks like "dev-master as 1.0.0" — the version string in the lock
    // would be "dev-master" without " as ".
    if pretty_version.starts_with("dev-") && !pretty_version.contains(" as ") {
        return None;
    }
    if let Some(norm) = version_normalized
        && norm.starts_with("dev-")
        && !pretty_version.contains(" as ")
    {
        return None;
    }

    // Resolve the actual version string to use for bumping.
    // If the pretty_version contains an inline alias (e.g. "dev-master as 1.0.0"),
    // take the alias target. Otherwise use pretty_version directly.
    let installed_version = resolve_installed_version(pretty_version, version_normalized);

    // Handle OR constraints (^1.0 || ^2.0)
    if constraint_body.contains("||") {
        return bump_or_constraint(constraint_body, &installed_version, stability_flag);
    }

    // Single constraint
    bump_single(constraint_body.trim(), &installed_version, stability_flag)
}

// ─── OR constraint handling ───────────────────────────────────────────────────

fn bump_or_constraint(
    constraint_body: &str,
    installed_version: &str,
    stability_flag: Option<&str>,
) -> Option<String> {
    let parts: Vec<&str> = constraint_body.split("||").map(str::trim).collect();

    // Determine which major the installed version belongs to
    let installed_major = parse_major(installed_version);

    let mut changed = false;
    let mut new_parts: Vec<String> = Vec::new();

    for part in &parts {
        let part_trimmed = part.trim();
        // Determine the major range this disjunct covers
        let part_major = constraint_major(part_trimmed);

        // Only bump the disjunct whose major matches the installed version's major
        if part_major == installed_major {
            if let Some(bumped) = bump_single(part_trimmed, installed_version, None) {
                new_parts.push(bumped);
                changed = true;
            } else {
                new_parts.push(part_trimmed.to_string());
            }
        } else {
            new_parts.push(part_trimmed.to_string());
        }
    }

    if !changed {
        return None;
    }

    let joined = new_parts.join(" || ");
    let result = append_stability_flag(&joined, stability_flag);
    Some(result)
}

// ─── Single constraint handling ───────────────────────────────────────────────

fn bump_single(
    constraint: &str,
    installed_version: &str,
    stability_flag: Option<&str>,
) -> Option<String> {
    // AND constraints (space-separated like ">=1.0 <2.0" or comma-separated
    // like ">=1.0,<2.0"): split into parts and bump only the lower-bound part.
    let after_op = constraint
        .trim_start_matches('^')
        .trim_start_matches('~')
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches("!=")
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim_start_matches('=');
    if after_op.contains(' ') || after_op.contains(',') {
        return bump_and_constraint(constraint, installed_version, stability_flag);
    }

    // Caret: ^X.Y.Z
    if let Some(rest) = constraint.strip_prefix('^') {
        return bump_caret(rest.trim(), installed_version, stability_flag);
    }

    // Tilde: ~X.Y.Z
    if let Some(rest) = constraint.strip_prefix('~') {
        return bump_tilde(rest.trim(), installed_version, stability_flag);
    }

    // Wildcard: * or X.*
    if constraint == "*" || constraint.ends_with(".*") {
        return bump_wildcard(constraint, installed_version, stability_flag);
    }

    // Greater-or-equal: >=X.Y
    if let Some(rest) = constraint.strip_prefix(">=") {
        return bump_gte(rest.trim(), installed_version, stability_flag);
    }

    // Other operators (exact, <, <=, >, !=, range) — leave unchanged
    None
}

// ─── Caret bump ───────────────────────────────────────────────────────────────

/// `^X.Y.Z` → bump to installed version if it is greater.
///
/// The caret prefix is preserved; segments from installed version replace
/// those in the constraint (trimming trailing zeros appropriately).
fn bump_caret(rest: &str, installed_version: &str, stability_flag: Option<&str>) -> Option<String> {
    let constraint_segments = parse_version_segments(rest);
    let installed_segments = parse_version_segments(installed_version);

    // The constraint length determines how many segments to compare/output
    let n_constraint = constraint_segments.len().max(1);

    // Compare: if installed <= current lower bound, no change needed
    // We compare as many segments as the installed version has
    let current_lower: Vec<u64> = constraint_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();
    let installed: Vec<u64> = installed_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();

    if installed <= current_lower {
        return None;
    }

    // Build new constraint segments: use installed version, but only up to
    // the number of non-trivial segments needed.
    // We output at least as many segments as the original constraint had,
    // but trim trailing zeros.
    let mut new_segs: Vec<u64> = installed_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(n_constraint.max(installed_segments.len()))
        .collect();

    // Trim trailing zeros (but keep at least n_constraint segments, minimum 1)
    while new_segs.len() > n_constraint && new_segs.last() == Some(&0) {
        new_segs.pop();
    }
    // Also trim trailing zeros beyond 1 segment
    while new_segs.len() > 1 && new_segs.last() == Some(&0) {
        new_segs.pop();
    }

    let version_str = new_segs
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(".");

    let new_constraint = format!("^{version_str}");
    let result = append_stability_flag(&new_constraint, stability_flag);
    Some(result)
}

// ─── Tilde bump ───────────────────────────────────────────────────────────────

/// `~X.Y.Z` (3 segments) → bump patch: `~X.Y.new_patch`
/// `~X.Y` (2 segments) → convert to caret: `^X.Y.new_patch`
fn bump_tilde(rest: &str, installed_version: &str, stability_flag: Option<&str>) -> Option<String> {
    let constraint_segments = parse_version_segments(rest);
    let installed_segments = parse_version_segments(installed_version);

    let current_lower: Vec<u64> = constraint_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();
    let installed: Vec<u64> = installed_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();

    if installed <= current_lower {
        return None;
    }

    let major = installed_segments.first().copied().unwrap_or(0);
    let minor = installed_segments.get(1).copied().unwrap_or(0);
    let patch = installed_segments.get(2).copied().unwrap_or(0);

    let new_constraint = if constraint_segments.len() >= 3 {
        // ~X.Y.Z → keep tilde, bump patch
        if patch == 0 {
            format!("~{major}.{minor}.0")
        } else {
            format!("~{major}.{minor}.{patch}")
        }
    } else {
        // ~X.Y → convert to caret
        if patch == 0 {
            format!("^{major}.{minor}")
        } else {
            format!("^{major}.{minor}.{patch}")
        }
    };

    let result = append_stability_flag(&new_constraint, stability_flag);
    Some(result)
}

// ─── Wildcard bump ────────────────────────────────────────────────────────────

/// `*` → `>=installed`
/// `X.*` → `>=installed` (trimming trailing zeros)
fn bump_wildcard(
    constraint: &str,
    installed_version: &str,
    stability_flag: Option<&str>,
) -> Option<String> {
    let installed_segments = parse_version_segments(installed_version);

    // Trim trailing zeros
    let mut segs = installed_segments.clone();
    while segs.len() > 1 && segs.last() == Some(&0) {
        segs.pop();
    }

    let version_str = segs
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(".");

    // For plain wildcard "*", always produce >=installed
    if constraint == "*" {
        let new_constraint = format!(">={version_str}");
        return Some(append_stability_flag(&new_constraint, stability_flag));
    }

    // For "X.*", if installed is at that major, produce >=installed
    let base = constraint.trim_end_matches(".*");
    let base_segs = parse_version_segments(base);
    let current_lower: Vec<u64> = base_segs
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();
    let installed: Vec<u64> = installed_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();

    if installed <= current_lower {
        return None;
    }

    let new_constraint = format!(">={version_str}");
    Some(append_stability_flag(&new_constraint, stability_flag))
}

// ─── GTE bump ─────────────────────────────────────────────────────────────────

/// `>=X.Y` → raise to installed version (trimming trailing zeros)
fn bump_gte(rest: &str, installed_version: &str, stability_flag: Option<&str>) -> Option<String> {
    let constraint_segments = parse_version_segments(rest);
    let installed_segments = parse_version_segments(installed_version);

    let current_lower: Vec<u64> = constraint_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();
    let installed: Vec<u64> = installed_segments
        .iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();

    if installed <= current_lower {
        return None;
    }

    // Trim trailing zeros from installed version
    let mut segs = installed_segments.clone();
    while segs.len() > 1 && segs.last() == Some(&0) {
        segs.pop();
    }

    let version_str = segs
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(".");

    let new_constraint = format!(">={version_str}");
    let result = append_stability_flag(&new_constraint, stability_flag);
    Some(result)
}

// ─── AND constraint bump ──────────────────────────────────────────────────

/// Bump AND constraints like `>=1.0 <2.0` or `>=1.0,<2.0`.
///
/// Only the lower-bound part (>=, ^, ~) is bumped; upper-bound parts
/// (<, <=, !=) are preserved as-is.
fn bump_and_constraint(
    constraint: &str,
    installed_version: &str,
    stability_flag: Option<&str>,
) -> Option<String> {
    // Split on space or comma, preserving the separator style
    let (parts, separator) = split_and_parts(constraint);

    let mut changed = false;
    let mut new_parts: Vec<String> = Vec::new();

    for part in &parts {
        let trimmed = part.trim();
        if is_lower_bound(trimmed) {
            if let Some(bumped) = bump_single(trimmed, installed_version, None) {
                new_parts.push(bumped);
                changed = true;
            } else {
                new_parts.push(trimmed.to_string());
            }
        } else {
            new_parts.push(trimmed.to_string());
        }
    }

    if !changed {
        return None;
    }

    let joined = new_parts.join(separator);
    Some(append_stability_flag(&joined, stability_flag))
}

/// Split an AND constraint into parts, returning the parts and the separator.
fn split_and_parts(constraint: &str) -> (Vec<&str>, &str) {
    if constraint.contains(',') {
        (constraint.split(',').collect(), ",")
    } else {
        // Space-separated: split on spaces that precede an operator character
        let mut parts = Vec::new();
        let mut current_start = 0;
        let bytes = constraint.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if bytes[i] == b' ' {
                // Find next non-space
                let space_start = i;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                // If what follows starts with an operator, split here
                if i < bytes.len()
                    && (bytes[i] == b'>'
                        || bytes[i] == b'<'
                        || bytes[i] == b'!'
                        || bytes[i] == b'='
                        || bytes[i] == b'^'
                        || bytes[i] == b'~')
                {
                    parts.push(&constraint[current_start..space_start]);
                    current_start = i;
                }
            } else {
                i += 1;
            }
        }
        parts.push(&constraint[current_start..]);
        (parts, " ")
    }
}

/// Check if a constraint part is a lower bound (can be bumped).
fn is_lower_bound(part: &str) -> bool {
    part.starts_with(">=") || part.starts_with('^') || part.starts_with('~')
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Strip a trailing `@stability` flag from a constraint string.
/// Returns (body, flag) where flag is the `@...` suffix (without the `@`).
fn strip_stability_flag(constraint: &str) -> (&str, Option<&str>) {
    let known = ["@dev", "@alpha", "@beta", "@RC", "@rc", "@stable"];
    for flag in &known {
        if let Some(body) = constraint.strip_suffix(flag) {
            let flag_str = &constraint[body.len()..];
            return (body.trim_end(), Some(flag_str));
        }
    }
    (constraint, None)
}

/// Append an optional stability flag to a constraint string.
fn append_stability_flag(constraint: &str, flag: Option<&str>) -> String {
    match flag {
        Some(f) => format!("{constraint}{f}"),
        None => constraint.to_string(),
    }
}

/// Parse a version string into numeric segments.
/// Handles "1.2.3", "1.2", "1", etc.
/// Stops at any non-numeric/non-dot character.
fn parse_version_segments(version: &str) -> Vec<u64> {
    // Strip inline alias: "dev-master as 1.0.0" → "1.0.0"
    let version = if let Some(pos) = version.find(" as ") {
        &version[pos + 4..]
    } else {
        version
    };

    // Strip leading v/V
    let version = version
        .strip_prefix('v')
        .or_else(|| version.strip_prefix('V'))
        .unwrap_or(version);

    // Take up to any pre-release suffix (first '-' or '+')
    let version = version.split(['-', '+']).next().unwrap_or(version);

    version
        .split('.')
        .filter_map(|s| s.parse::<u64>().ok())
        .collect()
}

/// Parse the major version number from a version string.
fn parse_major(version: &str) -> Option<u64> {
    parse_version_segments(version).into_iter().next()
}

/// Determine the major version that a single disjunct constraint covers.
/// For `^1.2`, returns `Some(1)`. For `^0.3`, returns `Some(0)`.
fn constraint_major(constraint: &str) -> Option<u64> {
    if let Some(rest) = constraint.strip_prefix('^') {
        return parse_version_segments(rest).into_iter().next();
    }
    if let Some(rest) = constraint.strip_prefix('~') {
        return parse_version_segments(rest).into_iter().next();
    }
    if let Some(rest) = constraint.strip_prefix(">=") {
        return parse_version_segments(rest).into_iter().next();
    }
    // Try as plain version
    parse_version_segments(constraint).into_iter().next()
}

/// Resolve the installed version string to use for comparison.
/// Handles inline aliases (e.g., "dev-main as 2.1.0" → "2.1.0").
fn resolve_installed_version<'a>(
    pretty_version: &'a str,
    _version_normalized: Option<&'a str>,
) -> String {
    // If pretty_version contains an inline alias, use the alias target
    if let Some(pos) = pretty_version.find(" as ") {
        return pretty_version[pos + 4..].trim().to_string();
    }

    // If version_normalized is available and not a dev branch, prefer it
    // for more precise comparison, but use pretty_version for output
    // Actually we use pretty_version for building constraint strings
    // since normalized versions have extra .0 suffixes

    // Use pretty_version as-is (strip leading 'v' for normalization)
    pretty_version
        .strip_prefix('v')
        .unwrap_or(pretty_version)
        .to_string()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Caret bumps ───────────────────────────────────────────────────────────

    #[test]
    fn test_caret_bump_basic() {
        // ^1.0 + 1.2.1 → ^1.2.1
        let result = bump_requirement("^1.0", "1.2.1", Some("1.2.1.0"));
        assert_eq!(result, Some("^1.2.1".to_string()));
    }

    #[test]
    fn test_caret_no_change_at_lower_bound() {
        // ^1.2 + 1.2.0 → None (already at lower bound)
        let result = bump_requirement("^1.2", "1.2.0", Some("1.2.0.0"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_caret_no_change_exact_match() {
        // ^1.2.1 + 1.2.1 → None
        let result = bump_requirement("^1.2.1", "1.2.1", Some("1.2.1.0"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_caret_bump_zero_major() {
        // ^0.3 + 0.3.5 → ^0.3.5
        let result = bump_requirement("^0.3", "0.3.5", Some("0.3.5.0"));
        assert_eq!(result, Some("^0.3.5".to_string()));
    }

    #[test]
    fn test_caret_bump_three_segments() {
        // ^1.0.0 + 1.2.1 → ^1.2.1
        let result = bump_requirement("^1.0.0", "1.2.1", Some("1.2.1.0"));
        assert_eq!(result, Some("^1.2.1".to_string()));
    }

    #[test]
    fn test_caret_bump_minor_only() {
        // ^1.2 + 1.5.0 → ^1.5 (trailing zero trimmed)
        let result = bump_requirement("^1.2", "1.5.0", Some("1.5.0.0"));
        assert_eq!(result, Some("^1.5".to_string()));
    }

    // ── Tilde bumps ───────────────────────────────────────────────────────────

    #[test]
    fn test_tilde_three_segment_bump() {
        // ~2.0.0 + 2.0.3 → ~2.0.3
        let result = bump_requirement("~2.0.0", "2.0.3", Some("2.0.3.0"));
        assert_eq!(result, Some("~2.0.3".to_string()));
    }

    #[test]
    fn test_tilde_two_segment_becomes_caret() {
        // ~2.0 + 2.0.3 → ^2.0.3
        let result = bump_requirement("~2.0", "2.0.3", Some("2.0.3.0"));
        assert_eq!(result, Some("^2.0.3".to_string()));
    }

    #[test]
    fn test_tilde_no_change() {
        // ~2.0.3 + 2.0.3 → None
        let result = bump_requirement("~2.0.3", "2.0.3", Some("2.0.3.0"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_tilde_two_segment_no_patch() {
        // ~2.3 + 2.5.0 → ^2.5 (patch is 0, trimmed)
        let result = bump_requirement("~2.3", "2.5.0", Some("2.5.0.0"));
        assert_eq!(result, Some("^2.5".to_string()));
    }

    // ── Wildcard bumps ────────────────────────────────────────────────────────

    #[test]
    fn test_wildcard_star() {
        // * + 1.2.3 → >=1.2.3
        let result = bump_requirement("*", "1.2.3", Some("1.2.3.0"));
        assert_eq!(result, Some(">=1.2.3".to_string()));
    }

    #[test]
    fn test_wildcard_major_star() {
        // 2.* + 2.5.0 → >=2.5
        let result = bump_requirement("2.*", "2.5.0", Some("2.5.0.0"));
        assert_eq!(result, Some(">=2.5".to_string()));
    }

    #[test]
    fn test_wildcard_no_change() {
        // 2.* + 2.0.0 → None (installed is at lower bound)
        let result = bump_requirement("2.*", "2.0.0", Some("2.0.0.0"));
        assert_eq!(result, None);
    }

    // ── GTE bumps ─────────────────────────────────────────────────────────────

    #[test]
    fn test_gte_bump() {
        // >=1.2 + 1.5.0 → >=1.5
        let result = bump_requirement(">=1.2", "1.5.0", Some("1.5.0.0"));
        assert_eq!(result, Some(">=1.5".to_string()));
    }

    #[test]
    fn test_gte_no_change() {
        // >=1.5 + 1.5.0 → None
        let result = bump_requirement(">=1.5", "1.5.0", Some("1.5.0.0"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_gte_with_patch() {
        // >=1.2.0 + 1.5.3 → >=1.5.3
        let result = bump_requirement(">=1.2.0", "1.5.3", Some("1.5.3.0"));
        assert_eq!(result, Some(">=1.5.3".to_string()));
    }

    // ── OR constraints ────────────────────────────────────────────────────────

    #[test]
    fn test_or_constraint_bumps_matching_major() {
        // ^1.2 || ^2.3 + 1.3.0 → ^1.3 || ^2.3
        let result = bump_requirement("^1.2 || ^2.3", "1.3.0", Some("1.3.0.0"));
        assert_eq!(result, Some("^1.3 || ^2.3".to_string()));
    }

    #[test]
    fn test_or_constraint_bumps_second_major() {
        // ^1.2 || ^2.3 + 2.5.0 → ^1.2 || ^2.5
        let result = bump_requirement("^1.2 || ^2.3", "2.5.0", Some("2.5.0.0"));
        assert_eq!(result, Some("^1.2 || ^2.5".to_string()));
    }

    #[test]
    fn test_or_constraint_no_change() {
        // ^1.2 || ^2.3 + 1.2.0 → None
        let result = bump_requirement("^1.2 || ^2.3", "1.2.0", Some("1.2.0.0"));
        assert_eq!(result, None);
    }

    // ── Dev constraints ───────────────────────────────────────────────────────

    #[test]
    fn test_dev_constraint_unchanged() {
        // dev-master → None
        let result = bump_requirement("dev-master", "dev-master", None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_dev_installed_no_alias_unchanged() {
        // Installed is dev-main without alias → None
        let result = bump_requirement("^1.0", "dev-main", None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_dev_installed_with_alias() {
        // Installed is "dev-main as 1.2.0" → bump based on alias
        let result = bump_requirement("^1.0", "dev-main as 1.2.0", None);
        assert_eq!(result, Some("^1.2".to_string()));
    }

    // ── Stability flags ───────────────────────────────────────────────────────

    #[test]
    fn test_stability_flag_preserved() {
        // ^1.0@dev + 1.2.0 → ^1.2@dev
        let result = bump_requirement("^1.0@dev", "1.2.0", Some("1.2.0.0"));
        assert_eq!(result, Some("^1.2@dev".to_string()));
    }

    #[test]
    fn test_stability_flag_beta_preserved() {
        // ^1.0@beta + 1.2.1 → ^1.2.1@beta
        let result = bump_requirement("^1.0@beta", "1.2.1", Some("1.2.1.0"));
        assert_eq!(result, Some("^1.2.1@beta".to_string()));
    }

    // ── Edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn test_exact_constraint_no_bump() {
        // 1.2.3 → None (exact version, not bumped)
        let result = bump_requirement("1.2.3", "1.3.0", Some("1.3.0.0"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_and_constraint_gte_lt_space() {
        // >=1.0 <2.0 + 1.5.0 → >=1.5 <2.0
        let result = bump_requirement(">=1.0 <2.0", "1.5.0", Some("1.5.0.0"));
        assert_eq!(result, Some(">=1.5 <2.0".to_string()));
    }

    #[test]
    fn test_and_constraint_gte_lt_comma() {
        // >=1.0,<2.0 + 1.5.0 → >=1.5,<2.0
        let result = bump_requirement(">=1.0,<2.0", "1.5.0", Some("1.5.0.0"));
        assert_eq!(result, Some(">=1.5,<2.0".to_string()));
    }

    #[test]
    fn test_and_constraint_no_change() {
        // >=1.5 <2.0 + 1.5.0 → None (already at lower bound)
        let result = bump_requirement(">=1.5 <2.0", "1.5.0", Some("1.5.0.0"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_and_constraint_with_stability() {
        // >=1.0 <2.0@dev + 1.5.0 → >=1.5 <2.0@dev
        let result = bump_requirement(">=1.0 <2.0@dev", "1.5.0", Some("1.5.0.0"));
        assert_eq!(result, Some(">=1.5 <2.0@dev".to_string()));
    }

    #[test]
    fn test_parse_version_segments_basic() {
        assert_eq!(parse_version_segments("1.2.3"), vec![1, 2, 3]);
        assert_eq!(parse_version_segments("1.2"), vec![1, 2]);
        assert_eq!(parse_version_segments("1"), vec![1]);
    }

    #[test]
    fn test_parse_version_segments_with_prerelease() {
        assert_eq!(parse_version_segments("1.2.3-beta1"), vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_version_segments_with_v_prefix() {
        assert_eq!(parse_version_segments("v1.2.3"), vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_version_segments_alias() {
        // "dev-master as 1.0.0" → segments of "1.0.0"
        assert_eq!(parse_version_segments("dev-master as 1.0.0"), vec![1, 0, 0]);
    }
}
