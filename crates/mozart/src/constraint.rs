use std::cmp::Ordering;

/// A parsed Composer version (always 4 numeric segments + optional stability suffix).
/// Composer normalizes all versions to `major.minor.patch.build[-stability[N]]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub build: u64,
    /// None = stable, Some("alpha1"), Some("beta2"), Some("RC1"), Some("dev")
    pub pre_release: Option<String>,
    /// true for "dev-master", "dev-feature/foo", etc.
    pub is_dev_branch: bool,
    /// The original branch name for dev branches (e.g. "master", "feature/foo")
    pub dev_branch_name: Option<String>,
}

/// Stability rank for ordering (lower = more stable).
fn stability_rank(pre: &str) -> u8 {
    let lower = pre.to_lowercase();
    if lower.starts_with("dev") {
        50
    } else if lower.starts_with("alpha") || lower.starts_with("a") {
        40
    } else if lower.starts_with("beta") || lower.starts_with("b") {
        30
    } else if lower.starts_with("rc") {
        20
    } else if lower.starts_with("patch") || lower.starts_with("pl") || lower == "p" {
        5
    } else {
        0
    }
}

/// Extract numeric suffix from a pre-release string like "alpha1" → 1, "beta" → 0
fn pre_release_number(pre: &str) -> u64 {
    let digits: String = pre.chars().skip_while(|c| c.is_alphabetic()).collect();
    digits.parse().unwrap_or(0)
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // Dev branches are always lowest
        match (self.is_dev_branch, other.is_dev_branch) {
            (true, true) => {
                // Compare branch names
                return self.dev_branch_name.cmp(&other.dev_branch_name);
            }
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (false, false) => {}
        }

        // Compare numeric segments
        let num_cmp = (self.major, self.minor, self.patch, self.build).cmp(&(
            other.major,
            other.minor,
            other.patch,
            other.build,
        ));
        if num_cmp != Ordering::Equal {
            return num_cmp;
        }

        // Compare pre-release: None (stable) > any pre-release
        match (&self.pre_release, &other.pre_release) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => {
                let rank_a = stability_rank(a);
                let rank_b = stability_rank(b);
                match rank_a.cmp(&rank_b) {
                    Ordering::Equal => {
                        // Same stability: compare numeric suffix
                        pre_release_number(a).cmp(&pre_release_number(b))
                    }
                    // Lower rank = more stable = greater version
                    Ordering::Less => Ordering::Greater,
                    Ordering::Greater => Ordering::Less,
                }
            }
        }
    }
}

impl Version {
    /// Parse a version string into a `Version` struct using Composer normalization rules.
    pub fn parse(input: &str) -> Result<Version, String> {
        let s = input.trim();

        // Strip inline alias: "1.0.x-dev as 1.0.0" → "1.0.x-dev"
        let s = if let Some(pos) = s.find(" as ") {
            &s[..pos]
        } else {
            s
        };

        // Strip stability flag: "@dev", "@alpha", "@beta", "@RC", "@stable"
        let s = if let Some(pos) = s.rfind('@') {
            let after = &s[pos + 1..];
            let known = ["dev", "alpha", "beta", "rc", "stable"];
            if known.iter().any(|k| after.eq_ignore_ascii_case(k)) {
                &s[..pos]
            } else {
                s
            }
        } else {
            s
        };

        // Handle dev-* prefix branches
        if s.to_lowercase().starts_with("dev-") {
            let branch = &s[4..];
            return Ok(Version {
                major: 0,
                minor: 0,
                patch: 0,
                build: 0,
                pre_release: Some("dev".to_string()),
                is_dev_branch: true,
                dev_branch_name: Some(branch.to_string()),
            });
        }

        // Handle *-dev suffix (e.g., "2.1.x-dev" or "2.x-dev")
        let s_lower = s.to_lowercase();
        if s_lower.ends_with("-dev") || s_lower.ends_with(".x-dev") {
            let base = if s_lower.ends_with("-dev") {
                &s[..s.len() - 4]
            } else {
                s
            };
            // Replace any trailing .x with nothing, parse numeric parts
            let base = base.trim_end_matches(".x").trim_end_matches("-dev");
            let parts: Vec<&str> = base.split('.').collect();
            let major = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
            let minor = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
            return Ok(Version {
                major,
                minor,
                patch: 9999999,
                build: 9999999,
                pre_release: Some("dev".to_string()),
                is_dev_branch: true,
                dev_branch_name: None,
            });
        }

        // Strip leading v/V
        let s = s
            .strip_prefix('v')
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s);

        // Strip build metadata after +
        let s = s.split('+').next().unwrap_or(s);

        // Parse the version using regex-like approach
        parse_classical_version(s)
    }

    /// Create a "dev boundary" version for constraint matching (major.minor.patch.build with dev pre-release).
    pub fn dev_boundary(major: u64, minor: u64, patch: u64, build: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            build,
            pre_release: Some("dev".to_string()),
            is_dev_branch: false,
            dev_branch_name: None,
        }
    }
}

fn parse_classical_version(s: &str) -> Result<Version, String> {
    // Split on '-' to separate version from pre-release
    let (version_part, pre_part) = if let Some(pos) = s.find('-') {
        (&s[..pos], Some(&s[pos + 1..]))
    } else {
        (s, None)
    };

    let segments: Vec<&str> = version_part.split('.').collect();
    if segments.is_empty() || segments[0].is_empty() {
        return Err(format!("Invalid version: {s}"));
    }

    let major: u64 = segments[0]
        .parse()
        .map_err(|_| format!("Invalid major version segment: {}", segments[0]))?;
    let minor: u64 = segments.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch: u64 = segments
        .get(2)
        .and_then(|p| {
            // strip trailing .x
            let p = p.trim_end_matches('x').trim_end_matches('.');
            if p.is_empty() {
                Some(0)
            } else {
                p.parse().ok()
            }
        })
        .unwrap_or(0);
    let build: u64 = segments.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);

    let pre_release = pre_part.map(normalize_pre_release);

    Ok(Version {
        major,
        minor,
        patch,
        build,
        pre_release,
        is_dev_branch: false,
        dev_branch_name: None,
    })
}

fn normalize_pre_release(s: &str) -> String {
    // Normalize aliases: b→beta, a→alpha, rc→RC, p/pl/patch→patch
    let lower = s.to_lowercase();
    // Strip leading non-alpha characters (dots, underscores, dashes used as separators)
    let normalized = lower
        .trim_start_matches(|c: char| !c.is_alphabetic())
        .to_string();

    // Extract the alphabetic prefix (stability name)
    let alpha: String = normalized.chars().take_while(|c| c.is_alphabetic()).collect();
    // Extract only digits from the rest (strip separators like dots)
    let num: String = normalized
        .chars()
        .skip_while(|c| c.is_alphabetic())
        .filter(|c| c.is_ascii_digit())
        .collect();

    if alpha.starts_with("beta") || alpha == "b" {
        format!("beta{num}")
    } else if alpha.starts_with("alpha") || alpha == "a" {
        format!("alpha{num}")
    } else if alpha == "rc" {
        format!("RC{num}")
    } else if alpha == "patch" || alpha == "pl" || alpha == "p" {
        format!("patch{num}")
    } else if alpha == "dev" {
        "dev".to_string()
    } else {
        s.to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Constraint types
// ─────────────────────────────────────────────────────────────────────────────

/// A single atomic constraint.
#[derive(Debug, Clone)]
pub enum Constraint {
    /// Exact version match
    Exact(Version),
    /// Greater than: `> 1.2.3`
    GreaterThan(Version),
    /// Greater than or equal: `>= 1.2.3`
    GreaterThanOrEqual(Version),
    /// Less than: `< 1.2.3`
    LessThan(Version),
    /// Less than or equal: `<= 1.2.3`
    LessThanOrEqual(Version),
    /// Not equal: `!= 1.2.3`
    NotEqual(Version),
    /// Matches any version
    Any,
}

impl Constraint {
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Constraint::Exact(target) => v == target,
            Constraint::GreaterThan(target) => v > target,
            Constraint::GreaterThanOrEqual(target) => v >= target,
            Constraint::LessThan(target) => v < target,
            Constraint::LessThanOrEqual(target) => v <= target,
            Constraint::NotEqual(target) => v != target,
            Constraint::Any => true,
        }
    }
}

/// A compound constraint with AND/OR combinators.
#[derive(Debug, Clone)]
pub enum VersionConstraint {
    /// Single atomic constraint
    Single(Constraint),
    /// All must match (AND — space/comma separated)
    And(Vec<VersionConstraint>),
    /// At least one must match (OR — `||` separated)
    Or(Vec<VersionConstraint>),
}

impl VersionConstraint {
    pub fn matches(&self, version: &Version) -> bool {
        match self {
            VersionConstraint::Single(c) => c.matches(version),
            VersionConstraint::And(cs) => cs.iter().all(|c| c.matches(version)),
            VersionConstraint::Or(cs) => cs.iter().any(|c| c.matches(version)),
        }
    }

    /// Parse a constraint string like `^1.2`, `>=1.0 <2.0`, `^1.0 || ^2.0`.
    pub fn parse(input: &str) -> Result<VersionConstraint, String> {
        let input = input.trim();

        // Split on || (OR)
        let or_parts: Vec<&str> = split_or(input);

        if or_parts.len() > 1 {
            let constraints: Result<Vec<_>, _> =
                or_parts.iter().map(|p| parse_and_group(p.trim())).collect();
            let mut cs = constraints?;
            // Flatten single-element groups
            if cs.len() == 1 {
                return Ok(cs.remove(0));
            }
            return Ok(VersionConstraint::Or(cs));
        }

        parse_and_group(input)
    }
}

/// Split on `||` (pipe-OR), but not inside version strings.
fn split_or(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'|' && bytes[i + 1] == b'|' {
            parts.push(s[start..i].trim());
            i += 2;
            start = i;
        } else {
            i += 1;
        }
    }
    parts.push(s[start..].trim());
    parts
}

/// Parse an AND group (space or comma separated constraints).
fn parse_and_group(s: &str) -> Result<VersionConstraint, String> {
    // Detect hyphen range first: "1.0 - 2.0" where both sides start with a digit
    if let Some(idx) = s.find(" - ") {
        let before = s[..idx].trim();
        let after = s[idx + 3..].trim();
        let before_is_version = before
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == 'v' || c == 'V');
        let after_is_version = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == 'v' || c == 'V');
        if before_is_version && after_is_version {
            return parse_hyphen_range(s);
        }
    }

    let parts = split_and(s);

    if parts.is_empty() {
        return Err("Empty constraint".to_string());
    }

    let constraints: Result<Vec<_>, _> = parts.iter().map(|p| parse_single(p.trim())).collect();
    let mut cs = constraints?;

    if cs.len() == 1 {
        return Ok(cs.remove(0));
    }

    // Flatten nested And
    let flat: Vec<VersionConstraint> = cs
        .into_iter()
        .flat_map(|c| match c {
            VersionConstraint::And(inner) => inner,
            other => vec![other],
        })
        .collect();

    Ok(VersionConstraint::And(flat))
}

/// Split on spaces or commas (AND separator), respecting that version strings
/// can contain `-` (pre-release).
fn split_and(s: &str) -> Vec<String> {
    // A constraint "part" is separated by space or comma when not part of
    // operator prefixes like `>=`, `<=`, `!=`, or version like `1.2.3-beta`.
    // Strategy: tokenize by whitespace/comma, then re-join multi-token ranges.
    let tokens: Vec<&str> = s.split([' ', ',']).filter(|t| !t.is_empty()).collect();

    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();

    for token in tokens {
        if current.is_empty() {
            current = token.to_string();
        } else {
            // If the token starts with an operator or a digit/^ ~/>, it's a new constraint
            let starts_new = token.starts_with(|c: char| {
                matches!(c, '>' | '<' | '!' | '=' | '^' | '~' | '*') || c.is_ascii_digit()
            });
            if starts_new {
                parts.push(current.trim().to_string());
                current = token.to_string();
            } else {
                // Continuation (e.g. part of a version string with spaces)
                current.push(' ');
                current.push_str(token);
            }
        }
    }
    if !current.is_empty() {
        parts.push(current.trim().to_string());
    }

    parts
}

/// Parse a single constraint part.
fn parse_single(s: &str) -> Result<VersionConstraint, String> {
    if s == "*" || s.is_empty() {
        return Ok(VersionConstraint::Single(Constraint::Any));
    }

    // Caret: ^1.2.3
    if let Some(rest) = s.strip_prefix('^') {
        return parse_caret(rest);
    }

    // Tilde: ~1.2.3
    if let Some(rest) = s.strip_prefix('~') {
        return parse_tilde(rest);
    }

    // Hyphen range: "1.0 - 2.0" — handled at and-group level, but check here too
    if s.contains(" - ") {
        return parse_hyphen_range(s);
    }

    // Comparison operators
    if let Some(rest) = s.strip_prefix(">=") {
        let v = Version::parse(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::GreaterThanOrEqual(v)));
    }
    if let Some(rest) = s.strip_prefix("<=") {
        let v = Version::parse(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::LessThanOrEqual(v)));
    }
    if let Some(rest) = s.strip_prefix("!=") {
        let v = Version::parse(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::NotEqual(v)));
    }
    if let Some(rest) = s.strip_prefix('>') {
        let v = Version::parse(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::GreaterThan(v)));
    }
    if let Some(rest) = s.strip_prefix('<') {
        let v = Version::parse(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::LessThan(v)));
    }
    if let Some(rest) = s.strip_prefix('=') {
        let v = Version::parse(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::Exact(v)));
    }

    // Wildcard: 1.2.* or 1.*
    if s.ends_with(".*") || s.ends_with(".*.*") || s == "*" {
        return parse_wildcard(s);
    }

    // Exact version
    let v = Version::parse(s)?;
    Ok(VersionConstraint::Single(Constraint::Exact(v)))
}

/// Parse `^major.minor.patch` caret constraint.
/// First non-zero segment is the "locked" boundary.
fn parse_caret(s: &str) -> Result<VersionConstraint, String> {
    let parts: Vec<&str> = s.split('.').collect();
    let major: u64 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u64 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch: u64 = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
    let build: u64 = parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);

    let lower = Version::dev_boundary(major, minor, patch, build);

    // Determine upper bound based on first non-zero segment
    let upper = if major > 0 {
        Version::dev_boundary(major + 1, 0, 0, 0)
    } else if minor > 0 {
        Version::dev_boundary(0, minor + 1, 0, 0)
    } else if patch > 0 {
        Version::dev_boundary(0, 0, patch + 1, 0)
    } else {
        Version::dev_boundary(0, 0, 1, 0)
    };

    Ok(VersionConstraint::And(vec![
        VersionConstraint::Single(Constraint::GreaterThanOrEqual(lower)),
        VersionConstraint::Single(Constraint::LessThan(upper)),
    ]))
}

/// Parse `~major.minor.patch` tilde constraint.
fn parse_tilde(s: &str) -> Result<VersionConstraint, String> {
    let parts: Vec<&str> = s.split('.').collect();
    let major: u64 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u64 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch: u64 = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
    let build: u64 = parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);

    let lower = Version::dev_boundary(major, minor, patch, build);

    // ~major.minor.patch → >=major.minor.patch <major.(minor+1).0
    // ~major.minor → >=major.minor.0 <(major+1).0.0
    // ~major → >=major.0.0 <(major+1).0.0
    let upper = if parts.len() >= 3 {
        Version::dev_boundary(major, minor + 1, 0, 0)
    } else {
        Version::dev_boundary(major + 1, 0, 0, 0)
    };

    Ok(VersionConstraint::And(vec![
        VersionConstraint::Single(Constraint::GreaterThanOrEqual(lower)),
        VersionConstraint::Single(Constraint::LessThan(upper)),
    ]))
}

/// Parse `1.2.*` wildcard constraint.
fn parse_wildcard(s: &str) -> Result<VersionConstraint, String> {
    if s == "*" {
        return Ok(VersionConstraint::Single(Constraint::Any));
    }

    // Strip trailing .*
    let base = s.trim_end_matches(".*");
    if base.is_empty() {
        return Ok(VersionConstraint::Single(Constraint::Any));
    }

    let parts: Vec<&str> = base.split('.').collect();
    let major: u64 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u64 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);

    let (lower, upper) = if parts.len() == 1 {
        (
            Version::dev_boundary(major, 0, 0, 0),
            Version::dev_boundary(major + 1, 0, 0, 0),
        )
    } else {
        (
            Version::dev_boundary(major, minor, 0, 0),
            Version::dev_boundary(major, minor + 1, 0, 0),
        )
    };

    Ok(VersionConstraint::And(vec![
        VersionConstraint::Single(Constraint::GreaterThanOrEqual(lower)),
        VersionConstraint::Single(Constraint::LessThan(upper)),
    ]))
}

/// Parse `1.0 - 2.0` hyphen range.
fn parse_hyphen_range(s: &str) -> Result<VersionConstraint, String> {
    let parts: Vec<&str> = s.splitn(2, " - ").collect();
    if parts.len() != 2 {
        return Err(format!("Invalid hyphen range: {s}"));
    }

    let lower_v = Version::parse(parts[0].trim())?;
    let upper_v = Version::parse(parts[1].trim())?;

    Ok(VersionConstraint::And(vec![
        VersionConstraint::Single(Constraint::GreaterThanOrEqual(lower_v)),
        VersionConstraint::Single(Constraint::LessThanOrEqual(upper_v)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ──────────── Version parsing ────────────

    #[test]
    fn test_parse_simple() {
        let v = Version::parse("1.2.3").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.build, 0);
        assert_eq!(v.pre_release, None);
        assert!(!v.is_dev_branch);
    }

    #[test]
    fn test_parse_with_v_prefix() {
        let v = Version::parse("v1.2").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 0);
        assert_eq!(v.build, 0);
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_four_segments() {
        let v = Version::parse("1.2.3.4").unwrap();
        assert_eq!((v.major, v.minor, v.patch, v.build), (1, 2, 3, 4));
    }

    #[test]
    fn test_parse_beta() {
        let v = Version::parse("1.0.0-beta.1").unwrap();
        assert_eq!(v.major, 1);
        // "beta.1" normalizes to "beta1" (dot is stripped)
        assert_eq!(v.pre_release, Some("beta1".to_string()));
    }

    #[test]
    fn test_parse_beta1() {
        let v = Version::parse("1.0.0-beta1").unwrap();
        assert_eq!(v.pre_release, Some("beta1".to_string()));
    }

    #[test]
    fn test_parse_rc() {
        let v = Version::parse("1.0.0-RC1").unwrap();
        assert_eq!(v.pre_release, Some("RC1".to_string()));
    }

    #[test]
    fn test_parse_alpha() {
        let v = Version::parse("2.0.0-alpha3").unwrap();
        assert_eq!(v.pre_release, Some("alpha3".to_string()));
    }

    #[test]
    fn test_parse_dev_master() {
        let v = Version::parse("dev-master").unwrap();
        assert!(v.is_dev_branch);
        assert_eq!(v.dev_branch_name, Some("master".to_string()));
        assert_eq!(v.pre_release, Some("dev".to_string()));
    }

    #[test]
    fn test_parse_dev_feature() {
        let v = Version::parse("dev-feature/foo").unwrap();
        assert!(v.is_dev_branch);
        assert_eq!(v.dev_branch_name, Some("feature/foo".to_string()));
    }

    #[test]
    fn test_parse_x_dev() {
        let v = Version::parse("2.1.x-dev").unwrap();
        assert!(v.is_dev_branch);
        assert_eq!(v.major, 2);
        assert_eq!(v.minor, 1);
        assert_eq!(v.patch, 9999999);
        assert_eq!(v.build, 9999999);
    }

    #[test]
    fn test_parse_strip_at_stability() {
        let v = Version::parse("1.2.3@stable").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_inline_alias() {
        let v = Version::parse("1.0.x-dev as 1.0.0").unwrap();
        // Takes left side: 1.0.x-dev
        assert!(v.is_dev_branch);
    }

    // ──────────── Version ordering ────────────

    #[test]
    fn test_ordering_major() {
        let a = Version::parse("2.0.0").unwrap();
        let b = Version::parse("1.0.0").unwrap();
        assert!(a > b);
    }

    #[test]
    fn test_ordering_minor() {
        let a = Version::parse("1.2.0").unwrap();
        let b = Version::parse("1.1.0").unwrap();
        assert!(a > b);
    }

    #[test]
    fn test_ordering_stable_gt_rc() {
        let stable = Version::parse("1.0.0").unwrap();
        let rc = Version::parse("1.0.0-RC1").unwrap();
        assert!(stable > rc);
    }

    #[test]
    fn test_ordering_rc_gt_beta() {
        let rc = Version::parse("1.0.0-RC1").unwrap();
        let beta = Version::parse("1.0.0-beta1").unwrap();
        assert!(rc > beta);
    }

    #[test]
    fn test_ordering_beta_gt_alpha() {
        let beta = Version::parse("1.0.0-beta1").unwrap();
        let alpha = Version::parse("1.0.0-alpha1").unwrap();
        assert!(beta > alpha);
    }

    #[test]
    fn test_ordering_alpha_gt_dev_branch() {
        let alpha = Version::parse("1.0.0-alpha1").unwrap();
        let dev = Version::parse("dev-master").unwrap();
        assert!(alpha > dev);
    }

    #[test]
    fn test_ordering_pre_release_numbers() {
        let beta2 = Version::parse("1.0.0-beta2").unwrap();
        let beta1 = Version::parse("1.0.0-beta1").unwrap();
        assert!(beta2 > beta1);
    }

    // ──────────── Constraint parsing ────────────

    #[test]
    fn test_parse_any() {
        let c = VersionConstraint::parse("*").unwrap();
        let v = Version::parse("1.2.3").unwrap();
        assert!(c.matches(&v));
    }

    #[test]
    fn test_parse_exact() {
        let c = VersionConstraint::parse("1.2.3").unwrap();
        let v = Version::parse("1.2.3").unwrap();
        assert!(c.matches(&v));
        let v2 = Version::parse("1.2.4").unwrap();
        assert!(!c.matches(&v2));
    }

    #[test]
    fn test_parse_gte() {
        let c = VersionConstraint::parse(">=1.0.0").unwrap();
        assert!(c.matches(&Version::parse("1.0.0").unwrap()));
        assert!(c.matches(&Version::parse("2.0.0").unwrap()));
        assert!(!c.matches(&Version::parse("0.9.0").unwrap()));
    }

    #[test]
    fn test_parse_caret_major() {
        let c = VersionConstraint::parse("^1.2").unwrap();
        assert!(c.matches(&Version::parse("1.2.0").unwrap()));
        assert!(c.matches(&Version::parse("1.3.0").unwrap()));
        assert!(c.matches(&Version::parse("1.9.9").unwrap()));
        assert!(!c.matches(&Version::parse("2.0.0").unwrap()));
        assert!(!c.matches(&Version::parse("1.1.0").unwrap()));
    }

    #[test]
    fn test_parse_caret_zero_minor() {
        // ^0.2.3 → >=0.2.3 <0.3.0
        let c = VersionConstraint::parse("^0.2.3").unwrap();
        assert!(c.matches(&Version::parse("0.2.3").unwrap()));
        assert!(c.matches(&Version::parse("0.2.9").unwrap()));
        assert!(!c.matches(&Version::parse("0.3.0").unwrap()));
        assert!(!c.matches(&Version::parse("1.0.0").unwrap()));
    }

    #[test]
    fn test_parse_tilde_three_parts() {
        // ~1.2.3 → >=1.2.3 <1.3.0
        let c = VersionConstraint::parse("~1.2.3").unwrap();
        assert!(c.matches(&Version::parse("1.2.3").unwrap()));
        assert!(c.matches(&Version::parse("1.2.9").unwrap()));
        assert!(!c.matches(&Version::parse("1.3.0").unwrap()));
    }

    #[test]
    fn test_parse_tilde_two_parts() {
        // ~1.2 → >=1.2.0 <2.0.0
        let c = VersionConstraint::parse("~1.2").unwrap();
        assert!(c.matches(&Version::parse("1.2.0").unwrap()));
        assert!(c.matches(&Version::parse("1.9.0").unwrap()));
        assert!(!c.matches(&Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn test_parse_wildcard() {
        let c = VersionConstraint::parse("1.2.*").unwrap();
        assert!(c.matches(&Version::parse("1.2.0").unwrap()));
        assert!(c.matches(&Version::parse("1.2.9").unwrap()));
        assert!(!c.matches(&Version::parse("1.3.0").unwrap()));
    }

    #[test]
    fn test_parse_and() {
        let c = VersionConstraint::parse(">=1.0 <2.0").unwrap();
        assert!(c.matches(&Version::parse("1.0.0").unwrap()));
        assert!(c.matches(&Version::parse("1.9.9").unwrap()));
        assert!(!c.matches(&Version::parse("2.0.0").unwrap()));
        assert!(!c.matches(&Version::parse("0.9.9").unwrap()));
    }

    #[test]
    fn test_parse_or() {
        let c = VersionConstraint::parse("^1.0 || ^2.0").unwrap();
        assert!(c.matches(&Version::parse("1.5.0").unwrap()));
        assert!(c.matches(&Version::parse("2.3.0").unwrap()));
        assert!(!c.matches(&Version::parse("3.0.0").unwrap()));
    }

    #[test]
    fn test_parse_not_equal() {
        let c = VersionConstraint::parse("!=1.5.0").unwrap();
        assert!(c.matches(&Version::parse("1.4.0").unwrap()));
        assert!(!c.matches(&Version::parse("1.5.0").unwrap()));
        assert!(c.matches(&Version::parse("1.6.0").unwrap()));
    }

    #[test]
    fn test_parse_hyphen_range() {
        let c = VersionConstraint::parse("1.0 - 2.0").unwrap();
        assert!(c.matches(&Version::parse("1.0.0").unwrap()));
        assert!(c.matches(&Version::parse("1.5.0").unwrap()));
        assert!(c.matches(&Version::parse("2.0.0").unwrap()));
        assert!(!c.matches(&Version::parse("0.9.0").unwrap()));
        assert!(!c.matches(&Version::parse("2.1.0").unwrap()));
    }
}
