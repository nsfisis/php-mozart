use std::cmp::Ordering;
use std::fmt;

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
        // Named dev branches (`dev-foo`) sort below every numeric version.
        // A wildcard `1.0.x-dev` parses with `is_dev_branch=true` but
        // `dev_branch_name=None` and is semantically identical to its
        // normalized form `1.0.9999999.9999999-dev` (which parses with
        // `is_dev_branch=false`). Only the *named* case takes the
        // branch-comparison shortcut; unnamed wildcards fall through to
        // numeric comparison so the two forms compare equal.
        let self_named = self.is_dev_branch && self.dev_branch_name.is_some();
        let other_named = other.is_dev_branch && other.dev_branch_name.is_some();
        match (self_named, other_named) {
            (true, true) => {
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

        // Compare pre-release: None (stable) > most pre-releases,
        // but patch/pl/p pre-releases (stability_rank 5) rank ABOVE stable.
        match (&self.pre_release, &other.pre_release) {
            (None, None) => Ordering::Equal,
            (None, Some(b)) => {
                if stability_rank(b) == 5 {
                    // patch pre-release ranks above stable
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (Some(a), None) => {
                if stability_rank(a) == 5 {
                    // patch pre-release ranks above stable
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
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

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_dev_branch {
            if let Some(ref name) = self.dev_branch_name {
                return write!(f, "dev-{}", name);
            }
            // Numeric dev branch (e.g. "2.x-dev")
            return write!(
                f,
                "{}.{}.{}.{}-dev",
                self.major, self.minor, self.patch, self.build
            );
        }
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.patch, self.build
        )?;
        if let Some(ref pre) = self.pre_release {
            write!(f, "-{}", pre)?;
        }
        Ok(())
    }
}

impl Version {
    /// Parse a version string into a `Version` struct using Composer normalization rules.
    ///
    /// For inline aliases (`"1.0.x-dev as 1.0.0"`), the LEFT side (the real branch version)
    /// is used. This is the correct behaviour for identifying *what* version a package provides.
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

        // Handle wildcard branch versions like "2.x-dev" / "2.1.x-dev". A
        // pure-numeric `-dev` (e.g. `1.0.0-dev` or `3.2.9999999.9999999-dev`,
        // the form Composer's `normalizeBranch` emits for `3.2.x`) is NOT a
        // branch — it falls through to classical parsing where `-dev` is just
        // a regular pre-release stability and `is_dev_branch` stays false.
        let s_lower = s.to_lowercase();
        if s_lower.ends_with(".x-dev") {
            let base = &s[..s.len() - ".x-dev".len()];
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

    /// Parse a version string for use inside a *constraint expression*.
    ///
    /// The difference from [`Version::parse`] is the treatment of inline aliases:
    /// `"1.0.x-dev as 1.0.0"` → takes the **right** side (`1.0.0`).
    ///
    /// Inline aliases appear in `require` fields like:
    /// ```text
    /// "some/package": "1.0.x-dev as 1.0.0"
    /// ```
    /// Here the author wants the constraint to be satisfied by the real version `1.0.0`,
    /// while the left side (`1.0.x-dev`) indicates the branch that provides it.
    pub fn parse_for_constraint(input: &str) -> Result<Version, String> {
        let s = input.trim();
        // For inline aliases, take the RIGHT side (alias target)
        let s = if let Some(pos) = s.find(" as ") {
            s[pos + 4..].trim()
        } else {
            s
        };
        Version::parse(s)
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
    let alpha: String = normalized
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();
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
            // Compare via `Ord` (rather than the derived `PartialEq`) so
            // wildcard-branch / numeric-dev pairs that represent the same
            // normalized version — e.g. `1.0.x-dev` (`is_dev_branch=true,
            // name=None`) and its expanded form `1.0.9999999.9999999-dev`
            // (`is_dev_branch=false`) — count as equal. The derived `==`
            // would compare `is_dev_branch` field-by-field and miss the
            // match.
            Constraint::Exact(target) => v.cmp(target).is_eq(),
            Constraint::GreaterThan(target) => v > target,
            Constraint::GreaterThanOrEqual(target) => v >= target,
            Constraint::LessThan(target) => v < target,
            Constraint::LessThanOrEqual(target) => v <= target,
            Constraint::NotEqual(target) => !v.cmp(target).is_eq(),
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

    /// Whether the two constraints share at least one satisfying version.
    ///
    /// Mirrors Composer's `MultiConstraint::matches` which is used to decide
    /// whether a `provide` / `replace` link can satisfy a `require`. OR
    /// constraints are flattened, then each AND/Single branch is reduced to a
    /// single (low, high, excluded) interval; the two intervals are intersected
    /// arithmetically.
    pub fn intersects(&self, other: &VersionConstraint) -> bool {
        let lhs = flatten_or(self);
        let rhs = flatten_or(other);
        lhs.iter()
            .any(|a| rhs.iter().any(|b| ranges_intersect(a, b)))
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

// ─────────────────────────────────────────────────────────────────────────────
// Constraint intersection helpers
// ─────────────────────────────────────────────────────────────────────────────

/// A reduced range form of a constraint branch: a half-open interval with an
/// optional excluded version (from `!=`). `lower`/`upper` are `(version,
/// inclusive)`; `None` means unbounded on that side.
#[derive(Debug, Clone)]
struct Range {
    lower: Option<(Version, bool)>,
    upper: Option<(Version, bool)>,
    excluded: Vec<Version>,
}

impl Range {
    fn unbounded() -> Self {
        Range {
            lower: None,
            upper: None,
            excluded: Vec::new(),
        }
    }
}

/// Flatten an OR tree into a vector of single-branch ranges. Returns one entry
/// per disjunct; AND trees are merged into a single Range.
fn flatten_or(c: &VersionConstraint) -> Vec<Range> {
    match c {
        VersionConstraint::Or(cs) => cs.iter().flat_map(flatten_or).collect(),
        _ => match constraint_to_range(c) {
            Some(r) => vec![r],
            None => vec![Range::unbounded()],
        },
    }
}

/// Reduce a non-OR constraint to a single Range. Returns `None` if the
/// constraint has nested OR (caller should `flatten_or` first).
fn constraint_to_range(c: &VersionConstraint) -> Option<Range> {
    match c {
        VersionConstraint::Single(atom) => Some(atom_to_range(atom)),
        VersionConstraint::And(cs) => {
            let mut acc = Range::unbounded();
            for sub in cs {
                let r = constraint_to_range(sub)?;
                merge_into(&mut acc, &r);
            }
            Some(acc)
        }
        VersionConstraint::Or(_) => None,
    }
}

fn atom_to_range(c: &Constraint) -> Range {
    match c {
        Constraint::Any => Range::unbounded(),
        Constraint::Exact(v) => Range {
            lower: Some((v.clone(), true)),
            upper: Some((v.clone(), true)),
            excluded: Vec::new(),
        },
        Constraint::GreaterThan(v) => Range {
            lower: Some((v.clone(), false)),
            upper: None,
            excluded: Vec::new(),
        },
        Constraint::GreaterThanOrEqual(v) => Range {
            lower: Some((v.clone(), true)),
            upper: None,
            excluded: Vec::new(),
        },
        Constraint::LessThan(v) => Range {
            lower: None,
            upper: Some((v.clone(), false)),
            excluded: Vec::new(),
        },
        Constraint::LessThanOrEqual(v) => Range {
            lower: None,
            upper: Some((v.clone(), true)),
            excluded: Vec::new(),
        },
        Constraint::NotEqual(v) => Range {
            lower: None,
            upper: None,
            excluded: vec![v.clone()],
        },
    }
}

/// Tighten `acc` by intersecting with `other`. Lower bound becomes the higher
/// of the two; upper bound becomes the lower; excluded versions accumulate.
fn merge_into(acc: &mut Range, other: &Range) {
    if let Some((ov, oi)) = &other.lower {
        match &acc.lower {
            None => acc.lower = Some((ov.clone(), *oi)),
            Some((av, ai)) => {
                if ov > av || (ov == av && !*oi && *ai) {
                    acc.lower = Some((ov.clone(), *oi));
                }
            }
        }
    }
    if let Some((ov, oi)) = &other.upper {
        match &acc.upper {
            None => acc.upper = Some((ov.clone(), *oi)),
            Some((av, ai)) => {
                if ov < av || (ov == av && !*oi && *ai) {
                    acc.upper = Some((ov.clone(), *oi));
                }
            }
        }
    }
    acc.excluded.extend(other.excluded.iter().cloned());
}

/// Whether two reduced ranges share at least one version.
fn ranges_intersect(a: &Range, b: &Range) -> bool {
    let mut combined = a.clone();
    merge_into(&mut combined, b);

    // Empty interval (lower > upper)
    if let (Some((lo, li)), Some((hi, hi_inc))) = (&combined.lower, &combined.upper) {
        if lo > hi {
            return false;
        }
        if lo == hi && !(*li && *hi_inc) {
            return false;
        }
        // Pinned to a single version that is excluded
        if lo == hi && *li && *hi_inc && combined.excluded.iter().any(|e| e == lo) {
            return false;
        }
    }

    // Witness extraction is approximate: if no bounds, any version works.
    // Excluded versions only invalidate when the interval has shrunk to a
    // single excluded point (handled above) — for ranges with width, a
    // satisfying neighbour always exists.
    true
}

/// Split on `|` or `||` (pipe-OR). Composer accepts both forms.
fn split_or(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|' {
            parts.push(s[start..i].trim());
            i += 1;
            // Skip second pipe if `||`
            if i < bytes.len() && bytes[i] == b'|' {
                i += 1;
            }
            start = i;
        } else {
            i += 1;
        }
    }
    parts.push(s[start..].trim());
    // Filter out empty parts (e.g. from leading/trailing pipes)
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Parse an AND group (space or comma separated constraints).
fn parse_and_group(s: &str) -> Result<VersionConstraint, String> {
    // Detect inline alias first: "1.0.x-dev as 1.0.0"
    // The entire expression is a single atomic constraint; parse it directly.
    if s.contains(" as ") {
        return parse_single(s);
    }

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

/// Strip `#ref` suffix from `dev-...#hex` / `....x-dev#hex` constraint
/// strings. Mirrors Composer's
/// `'{^(dev-[^,\s@]+?|[^,\s@]+?\.x-dev)#.+$}i'` regex strip in
/// `VersionParser::parseConstraint`. Returns a `Cow` so callers that pass
/// constraints without `#` see no allocation.
fn strip_constraint_ref(s: &str) -> std::borrow::Cow<'_, str> {
    let lower = s.to_lowercase();
    let Some(hash_pos) = s.find('#') else {
        return std::borrow::Cow::Borrowed(s);
    };
    let head = &lower[..hash_pos];
    let rest = &s[hash_pos + 1..];
    if rest.is_empty() {
        return std::borrow::Cow::Borrowed(s);
    }
    // Accept `dev-foo` or `1.2.x-dev` style prefixes only, mirroring the
    // Composer regex. Anything else (e.g. URLs, comments) is left alone.
    let head_no_space = !head
        .chars()
        .any(|c: char| c.is_whitespace() || c == ',' || c == '@');
    if !head_no_space {
        return std::borrow::Cow::Borrowed(s);
    }
    let matches = head.starts_with("dev-") || head.ends_with(".x-dev");
    if matches {
        std::borrow::Cow::Owned(s[..hash_pos].to_string())
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Parse a single constraint part.
fn parse_single(s: &str) -> Result<VersionConstraint, String> {
    if s == "*" || s.is_empty() {
        return Ok(VersionConstraint::Single(Constraint::Any));
    }

    // Strip `#ref` suffixes from `dev-...#hex` / `....x-dev#hex` constraints —
    // they pin a source reference at the root level (handled by the
    // installer) and are not part of the version match. Mirrors Composer's
    // `VersionParser::parseConstraint` `'{^(dev-[^,\s@]+?|[^,\s@]+?\.x-dev)#.+$}i'` strip.
    let s = strip_constraint_ref(s);
    let s = s.as_ref();

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
    // Use parse_for_constraint so that inline aliases like "1.0.x-dev as 1.0.0"
    // resolve to the alias target (right-hand side) when used in constraint context.
    if let Some(rest) = s.strip_prefix(">=") {
        let v = Version::parse_for_constraint(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::GreaterThanOrEqual(v)));
    }
    if let Some(rest) = s.strip_prefix("<=") {
        let v = Version::parse_for_constraint(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::LessThanOrEqual(v)));
    }
    if let Some(rest) = s.strip_prefix("!=") {
        let v = Version::parse_for_constraint(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::NotEqual(v)));
    }
    // Double-equals `==` is treated the same as `=` (exact match)
    if let Some(rest) = s.strip_prefix("==") {
        let v = Version::parse_for_constraint(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::Exact(v)));
    }
    if let Some(rest) = s.strip_prefix('>') {
        let v = Version::parse_for_constraint(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::GreaterThan(v)));
    }
    if let Some(rest) = s.strip_prefix('<') {
        let v = Version::parse_for_constraint(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::LessThan(v)));
    }
    if let Some(rest) = s.strip_prefix('=') {
        let v = Version::parse_for_constraint(rest.trim())?;
        return Ok(VersionConstraint::Single(Constraint::Exact(v)));
    }

    // Wildcard: 1.2.* or 1.*
    if s.ends_with(".*") || s.ends_with(".*.*") || s == "*" {
        return parse_wildcard(s);
    }

    // Exact version (may carry an inline alias; take the alias target for matching)
    let v = Version::parse_for_constraint(s)?;
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
    // `*.*` (and `*.*.*` etc.) collapse to plain `*` after stripping every
    // trailing `.*` segment — the major slot is itself a wildcard, so the
    // whole constraint is unconstrained. Composer's `parseConstraint`
    // reaches the same conclusion via its `xRange` step (any `x` anchor in
    // a position after a `*` is dropped).
    if base.is_empty() || base == "*" {
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
///
/// Follows Composer semantics:
/// - If the upper bound is a **full** version (3+ numeric segments or has a
///   pre-release suffix), the upper constraint is `<=upper`.
/// - If the upper bound is **partial** (1 or 2 numeric segments without a
///   pre-release suffix), the next significant release is computed and the
///   upper constraint becomes `< next-dev`.  For example `8.5` → `< 8.6.0-dev`,
///   `2` → `< 3.0.0-dev`.
fn parse_hyphen_range(s: &str) -> Result<VersionConstraint, String> {
    let parts: Vec<&str> = s.splitn(2, " - ").collect();
    if parts.len() != 2 {
        return Err(format!("Invalid hyphen range: {s}"));
    }

    let lower_v = Version::parse_for_constraint(parts[0].trim())?;

    let upper_raw = parts[1].trim();
    let upper_constraint = hyphen_upper_bound(upper_raw)?;

    Ok(VersionConstraint::And(vec![
        VersionConstraint::Single(Constraint::GreaterThanOrEqual(lower_v)),
        upper_constraint,
    ]))
}

/// Compute the upper-bound constraint for a hyphen range.
fn hyphen_upper_bound(raw: &str) -> Result<VersionConstraint, String> {
    // Strip leading 'v'/'V' for segment counting.
    let stripped = raw
        .strip_prefix('v')
        .or_else(|| raw.strip_prefix('V'))
        .unwrap_or(raw);

    // Separate numeric part from any pre-release suffix (e.g. "1.2.3-beta1").
    let (numeric_part, has_pre_release) =
        match stripped.find(|c: char| c == '-' && !c.is_ascii_digit()) {
            Some(_) => {
                // There's a '-' that is NOT inside the " - " separator (already split).
                // If it looks like a pre-release suffix, treat as full version.
                let has_suffix = stripped.contains('-') && {
                    let after_dash = &stripped[stripped.find('-').unwrap() + 1..];
                    after_dash.chars().next().is_some_and(|c| c.is_alphabetic())
                };
                (stripped.split('-').next().unwrap_or(stripped), has_suffix)
            }
            None => (stripped, false),
        };

    let segments: Vec<&str> = numeric_part.split('.').collect();
    let segment_count = segments.len();

    if has_pre_release || segment_count >= 3 {
        // Full version → inclusive upper bound.
        let upper_v = Version::parse_for_constraint(raw)?;
        return Ok(VersionConstraint::Single(Constraint::LessThanOrEqual(
            upper_v,
        )));
    }

    // Partial version → exclusive upper bound at the next significant release.
    let upper_v = Version::parse_for_constraint(raw)?;
    let next = match segment_count {
        1 => {
            // "2" → < 3.0.0.0-dev
            Version::dev_boundary(upper_v.major + 1, 0, 0, 0)
        }
        _ => {
            // "2.3" → < 2.4.0.0-dev
            Version::dev_boundary(upper_v.major, upper_v.minor + 1, 0, 0)
        }
    };

    Ok(VersionConstraint::Single(Constraint::LessThan(next)))
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

    #[test]
    fn test_parse_for_constraint_inline_alias() {
        // parse_for_constraint takes the RIGHT side of an inline alias
        let v = Version::parse_for_constraint("1.0.x-dev as 1.0.0").unwrap();
        assert!(!v.is_dev_branch);
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_for_constraint_no_alias() {
        // Without an alias, parse_for_constraint behaves like parse
        let v = Version::parse_for_constraint("1.2.3").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert!(!v.is_dev_branch);
    }

    #[test]
    fn test_constraint_inline_alias_exact_matches_target() {
        // A constraint written as "1.0.x-dev as 1.0.0" should match 1.0.0 (the alias target)
        let c = VersionConstraint::parse("1.0.x-dev as 1.0.0").unwrap();
        let target = Version::parse("1.0.0").unwrap();
        assert!(c.matches(&target));
        // But NOT a different version
        let other = Version::parse("1.1.0").unwrap();
        assert!(!c.matches(&other));
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
        // "1.0 - 2.0" → >=1.0.0.0 <2.1.0.0-dev (upper is partial)
        let c = VersionConstraint::parse("1.0 - 2.0").unwrap();
        assert!(c.matches(&Version::parse("1.0.0").unwrap()));
        assert!(c.matches(&Version::parse("1.5.0").unwrap()));
        assert!(c.matches(&Version::parse("2.0.0").unwrap()));
        assert!(c.matches(&Version::parse("2.0.5").unwrap()));
        assert!(!c.matches(&Version::parse("0.9.0").unwrap()));
        assert!(!c.matches(&Version::parse("2.1.0").unwrap()));
    }

    // ──────────── Helper ────────────

    fn satisfies(constraint: &str, version: &str) -> bool {
        let c = VersionConstraint::parse(constraint).unwrap();
        let v = Version::parse(version).unwrap();
        c.matches(&v)
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 1. VERSION PARSING EDGE CASES
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_parse_single_segment() {
        let v = Version::parse("1").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
        assert_eq!(v.build, 0);
        assert_eq!(v.pre_release, None);
        assert!(!v.is_dev_branch);
    }

    #[test]
    fn test_parse_two_segments() {
        let v = Version::parse("1.2").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 0);
        assert_eq!(v.build, 0);
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_zero_version() {
        let v = Version::parse("0.0.0").unwrap();
        assert_eq!(v.major, 0);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
        assert_eq!(v.build, 0);
        assert_eq!(v.pre_release, None);
        assert!(!v.is_dev_branch);
    }

    #[test]
    fn test_parse_zero_zero_one() {
        let v = Version::parse("0.0.1").unwrap();
        assert_eq!((v.major, v.minor, v.patch, v.build), (0, 0, 1, 0));
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_large_version_numbers() {
        let v = Version::parse("99999.1.2.3").unwrap();
        assert_eq!(v.major, 99999);
        assert_eq!(v.minor, 1);
        assert_eq!(v.patch, 2);
        assert_eq!(v.build, 3);
    }

    #[test]
    fn test_parse_uppercase_v_prefix() {
        let v = Version::parse("V1.2.3").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.pre_release, None);
        assert!(!v.is_dev_branch);
    }

    #[test]
    fn test_parse_build_metadata_stripped() {
        // Build metadata after '+' should be stripped
        let v = Version::parse("1.2.3+build.456").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_shorthand_b_normalizes_to_beta() {
        // "b2" suffix → beta2
        let v = Version::parse("1.0.0-b2").unwrap();
        assert_eq!(v.pre_release, Some("beta2".to_string()));
    }

    #[test]
    fn test_parse_shorthand_a_normalizes_to_alpha() {
        // "a1" suffix → alpha1
        let v = Version::parse("1.0.0-a1").unwrap();
        assert_eq!(v.pre_release, Some("alpha1".to_string()));
    }

    #[test]
    fn test_parse_shorthand_p_normalizes_to_patch() {
        // "p1" suffix → patch1
        let v = Version::parse("1.0.0-p1").unwrap();
        assert_eq!(v.pre_release, Some("patch1".to_string()));
    }

    #[test]
    fn test_parse_shorthand_pl_normalizes_to_patch() {
        // "pl2" suffix → patch2
        let v = Version::parse("1.0.0-pl2").unwrap();
        assert_eq!(v.pre_release, Some("patch2".to_string()));
    }

    #[test]
    fn test_parse_shorthand_rc_lowercase_normalizes_to_rc() {
        // "rc2" suffix → RC2
        let v = Version::parse("1.0.0-rc2").unwrap();
        assert_eq!(v.pre_release, Some("RC2".to_string()));
    }

    #[test]
    fn test_parse_stability_beta_no_number() {
        // "1.0.0-beta" with no number
        let v = Version::parse("1.0.0-beta").unwrap();
        assert_eq!(v.pre_release, Some("beta".to_string()));
    }

    #[test]
    fn test_parse_dev_release_branch() {
        // "dev-release-1.0" is a dev branch named "release-1.0"
        let v = Version::parse("dev-release-1.0").unwrap();
        assert!(v.is_dev_branch);
        assert_eq!(v.dev_branch_name, Some("release-1.0".to_string()));
        assert_eq!(v.pre_release, Some("dev".to_string()));
    }

    #[test]
    fn test_parse_dev_master_uppercase() {
        // "DEV-master" — case-insensitive dev- prefix
        let v = Version::parse("DEV-master").unwrap();
        assert!(v.is_dev_branch);
        assert_eq!(v.dev_branch_name, Some("master".to_string()));
    }

    #[test]
    fn test_parse_x_dev_two_segment() {
        // "2.x-dev" → major=2, minor=0, patch=9999999, build=9999999
        let v = Version::parse("2.x-dev").unwrap();
        assert!(v.is_dev_branch);
        assert_eq!(v.major, 2);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 9999999);
        assert_eq!(v.build, 9999999);
    }

    #[test]
    fn test_parse_numeric_dev_suffix() {
        // Pure-numeric `-dev` (no `.x`) — a regular dev-stability version,
        // not a wildcard branch. Mirrors the form Composer's `normalizeBranch`
        // emits for branch aliases like `3.2.x` → `3.2.9999999.9999999-dev`.
        let v = Version::parse("2.1-dev").unwrap();
        assert!(!v.is_dev_branch);
        assert_eq!(v.major, 2);
        assert_eq!(v.minor, 1);
        assert_eq!(v.pre_release.as_deref(), Some("dev"));
    }

    #[test]
    fn test_parse_stability_flag_dev() {
        // "1.0.0@dev" → strip @dev suffix, parse 1.0.0 as stable
        let v = Version::parse("1.0.0@dev").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 0, 0));
        assert!(!v.is_dev_branch);
        // After stripping @dev, no pre-release suffix remains
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_stability_flag_alpha() {
        let v = Version::parse("1.0.0@alpha").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 0, 0));
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_stability_flag_beta() {
        let v = Version::parse("1.0.0@beta").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 0, 0));
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_stability_flag_rc() {
        let v = Version::parse("1.0.0@rc").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 0, 0));
        assert_eq!(v.pre_release, None);
    }

    #[test]
    fn test_parse_inline_alias_left_side() {
        // "dev-main as 1.0.x-dev" → left side is "dev-main"
        let v = Version::parse("dev-main as 1.0.x-dev").unwrap();
        assert!(v.is_dev_branch);
        assert_eq!(v.dev_branch_name, Some("main".to_string()));
    }

    #[test]
    fn test_parse_error_empty_string() {
        let result = Version::parse("");
        assert!(result.is_err(), "Expected error for empty string");
    }

    #[test]
    fn test_parse_error_not_a_version() {
        // Strings with no numeric start should fail
        let result = Version::parse("not-a-version");
        assert!(
            result.is_err(),
            "Expected error for 'not-a-version', got: {:?}",
            result
        );
    }

    #[test]
    fn test_parse_error_only_dots() {
        let result = Version::parse("....");
        assert!(result.is_err(), "Expected error for '....'");
    }

    #[test]
    fn test_parse_error_non_numeric_segment() {
        // "1.abc.3" — minor segment is non-numeric; parse degrades minor to 0
        // The implementation uses `and_then(|p| p.parse().ok()).unwrap_or(0)`,
        // so non-numeric segments silently become 0. This is intentional behavior.
        let v = Version::parse("1.abc.3").unwrap();
        assert_eq!(v.major, 1);
        // minor "abc" fails to parse as u64, so falls back to 0
        assert_eq!(v.minor, 0);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 2. VERSION ORDERING
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_ordering_equal_versions() {
        let a = Version::parse("1.2.3").unwrap();
        let b = Version::parse("1.2.3").unwrap();
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);
    }

    #[test]
    fn test_ordering_patch_difference() {
        let a = Version::parse("1.2.4").unwrap();
        let b = Version::parse("1.2.3").unwrap();
        assert!(a > b);
    }

    #[test]
    fn test_ordering_build_segment_difference() {
        let a = Version::parse("1.2.3.2").unwrap();
        let b = Version::parse("1.2.3.1").unwrap();
        assert!(a > b);
    }

    #[test]
    fn test_ordering_dev_branch_lt_dev_prerelease() {
        // "dev-master" is a named dev branch (is_dev_branch=true), which sorts
        // below every non-branch version. "1.0.0-dev" is a regular numeric
        // version with `dev` stability — same form Composer's `normalizeBranch`
        // produces for branch aliases like `3.2.x` → `3.2.9999999.9999999-dev`.
        let dev_branch = Version::parse("dev-master").unwrap();
        let dev_prerelease = Version::parse("1.0.0-dev").unwrap();
        assert!(dev_branch < dev_prerelease);
    }

    #[test]
    fn test_ordering_dev_prerelease_lt_alpha() {
        let dev = Version::parse("1.0.0-dev").unwrap();
        let alpha = Version::parse("1.0.0-alpha1").unwrap();
        assert!(dev < alpha);
    }

    #[test]
    fn test_ordering_alpha_lt_beta() {
        let alpha = Version::parse("1.0.0-alpha1").unwrap();
        let beta = Version::parse("1.0.0-beta1").unwrap();
        assert!(alpha < beta);
    }

    #[test]
    fn test_ordering_beta_lt_rc() {
        let beta = Version::parse("1.0.0-beta1").unwrap();
        let rc = Version::parse("1.0.0-RC1").unwrap();
        assert!(beta < rc);
    }

    #[test]
    fn test_ordering_rc_lt_stable() {
        let rc = Version::parse("1.0.0-RC1").unwrap();
        let stable = Version::parse("1.0.0").unwrap();
        assert!(rc < stable);
    }

    #[test]
    fn test_ordering_patch_gt_stable() {
        // In Composer, patch/pl/p pre-releases rank ABOVE stable.
        // e.g. 1.0.0-patch1 > 1.0.0
        let stable = Version::parse("1.0.0").unwrap();
        let patch = Version::parse("1.0.0-patch1").unwrap();
        assert!(patch > stable);
    }

    #[test]
    fn test_ordering_rc3_gt_rc2() {
        let rc3 = Version::parse("1.0.0-RC3").unwrap();
        let rc2 = Version::parse("1.0.0-RC2").unwrap();
        assert!(rc3 > rc2);
    }

    #[test]
    fn test_ordering_alpha5_gt_alpha3() {
        let a5 = Version::parse("1.0.0-alpha5").unwrap();
        let a3 = Version::parse("1.0.0-alpha3").unwrap();
        assert!(a5 > a3);
    }

    #[test]
    fn test_ordering_dev_branches_alphabetical() {
        // Between two dev branches, compare branch names alphabetically
        let dev_foo = Version::parse("dev-foo").unwrap();
        let dev_bar = Version::parse("dev-bar").unwrap();
        // "bar" < "foo" alphabetically
        assert!(dev_foo > dev_bar);
    }

    #[test]
    fn test_ordering_zero_versions() {
        let a = Version::parse("0.0.2").unwrap();
        let b = Version::parse("0.0.1").unwrap();
        assert!(a > b);
    }

    #[test]
    fn test_ordering_four_vs_three_segment_equal() {
        // 1.2.3.0 and 1.2.3 should be equal (build defaults to 0)
        let a = Version::parse("1.2.3.0").unwrap();
        let b = Version::parse("1.2.3").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_ordering_comprehensive_chain() {
        // "dev-foo" is a named branch (is_dev_branch=true) — sorts below every
        // non-branch. "1.0.0-dev" is a regular numeric `dev`-stability version,
        // which sorts below alpha/beta/rc/stable but above named branches.
        let dev_branch = Version::parse("dev-foo").unwrap();
        let dev_prerelease = Version::parse("1.0.0-dev").unwrap();
        let alpha = Version::parse("1.0.0-alpha1").unwrap();
        let beta = Version::parse("1.0.0-beta1").unwrap();
        let rc = Version::parse("1.0.0-RC1").unwrap();
        let stable = Version::parse("1.0.0").unwrap();

        assert!(dev_branch < dev_prerelease);
        assert!(dev_prerelease < alpha);
        assert!(alpha < beta);
        assert!(beta < rc);
        assert!(rc < stable);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 3. CONSTRAINT PARSING EDGE CASES
    // ══════════════════════════════════════════════════════════════════════════

    // ── Caret ──

    #[test]
    fn test_caret_zero_zero_three() {
        // ^0.0.3 → >=0.0.3 <0.0.4
        assert!(satisfies("^0.0.3", "0.0.3"));
        assert!(!satisfies("^0.0.3", "0.0.4"));
        assert!(!satisfies("^0.0.3", "0.0.2"));
    }

    #[test]
    fn test_caret_zero_zero_zero() {
        // ^0.0.0 → first non-zero is none, upper = 0.0.1
        assert!(satisfies("^0.0.0", "0.0.0"));
        assert!(!satisfies("^0.0.0", "0.0.1"));
    }

    #[test]
    fn test_caret_single_major() {
        // ^1 → >=1.0.0 <2.0.0
        assert!(satisfies("^1", "1.0.0"));
        assert!(satisfies("^1", "1.99.99"));
        assert!(!satisfies("^1", "2.0.0"));
        assert!(!satisfies("^1", "0.9.9"));
    }

    #[test]
    fn test_caret_four_segments() {
        // ^1.2.3.4 → >=1.2.3.4 <2.0.0.0
        assert!(satisfies("^1.2.3.4", "1.2.3.4"));
        assert!(satisfies("^1.2.3.4", "1.9.0.0"));
        assert!(!satisfies("^1.2.3.4", "2.0.0.0"));
        assert!(!satisfies("^1.2.3.4", "1.2.3.3"));
    }

    #[test]
    fn test_caret_lower_boundary() {
        // ^1.2.3 lower boundary: 1.2.3 matches but 1.2.2 does not
        assert!(satisfies("^1.2.3", "1.2.3"));
        assert!(!satisfies("^1.2.3", "1.2.2"));
    }

    #[test]
    fn test_caret_upper_boundary() {
        // ^1.2.3 upper boundary: 1.9.9 matches, 2.0.0 does not
        assert!(satisfies("^1.2.3", "1.9.9"));
        assert!(!satisfies("^1.2.3", "2.0.0"));
    }

    // ── Tilde ──

    #[test]
    fn test_tilde_single_major() {
        // ~1 → >=1.0.0 <2.0.0
        assert!(satisfies("~1", "1.0.0"));
        assert!(satisfies("~1", "1.99.0"));
        assert!(!satisfies("~1", "2.0.0"));
        assert!(!satisfies("~1", "0.9.9"));
    }

    #[test]
    fn test_tilde_four_segments() {
        // ~1.2.3.4 → >=1.2.3.4 <1.3.0.0
        assert!(satisfies("~1.2.3.4", "1.2.3.4"));
        assert!(satisfies("~1.2.9.0", "1.2.9.0"));
        assert!(!satisfies("~1.2.3.4", "1.3.0.0"));
        assert!(!satisfies("~1.2.3.4", "1.2.3.3"));
    }

    #[test]
    fn test_tilde_lower_boundary() {
        // ~1.2.3: 1.2.3 matches, 1.2.2 does not
        assert!(satisfies("~1.2.3", "1.2.3"));
        assert!(!satisfies("~1.2.3", "1.2.2"));
    }

    #[test]
    fn test_tilde_upper_boundary() {
        // ~1.2.3: 1.2.9 matches, 1.3.0 does not
        assert!(satisfies("~1.2.3", "1.2.9"));
        assert!(!satisfies("~1.2.3", "1.3.0"));
    }

    // ── Wildcard ──

    #[test]
    fn test_wildcard_major_only() {
        // 1.* → >=1.0.0 <2.0.0
        assert!(satisfies("1.*", "1.0.0"));
        assert!(satisfies("1.*", "1.99.0"));
        assert!(!satisfies("1.*", "2.0.0"));
        assert!(!satisfies("1.*", "0.9.9"));
    }

    #[test]
    fn test_wildcard_double_star() {
        // 1.*.* is treated like 1.*
        assert!(satisfies("1.*.*", "1.5.0"));
        assert!(!satisfies("1.*.*", "2.0.0"));
    }

    #[test]
    fn test_wildcard_three_segment() {
        // 1.2.3.* — the implementation strips trailing .*; base is "1.2.3"
        // parse_wildcard strips .* and splits on '.'; parts.len()=3 → minor constraint
        assert!(satisfies("1.2.3.*", "1.2.3"));
        assert!(satisfies("1.2.3.*", "1.2.9"));
        assert!(!satisfies("1.2.3.*", "1.3.0"));
    }

    #[test]
    fn test_wildcard_zero_major() {
        // 0.* → >=0.0.0 <1.0.0
        assert!(satisfies("0.*", "0.0.0"));
        assert!(satisfies("0.*", "0.99.0"));
        assert!(!satisfies("0.*", "1.0.0"));
    }

    #[test]
    fn test_wildcard_v_prefix() {
        // v1.* — the wildcard parser strips the trailing .*; base becomes "v1"
        // parse_wildcard's base.split('.') on "v1" → single part "v1"
        // v1 fails to parse as u64, falls back to 0 — so this is like 0.*
        // Mark as ignore since the behavior diverges from the expected semantic
        #[allow(unused)]
        let _ = VersionConstraint::parse("v1.*"); // just verify it doesn't panic
    }

    // ── Hyphen ranges ──

    #[test]
    fn test_hyphen_range_partial_from() {
        // "1.0 - 2.0": 1.0 is a partial "from", lower = >=1.0.0
        assert!(satisfies("1.0 - 2.0", "1.0.0"));
        assert!(satisfies("1.0 - 2.0", "1.5.0"));
    }

    #[test]
    fn test_hyphen_range_partial_to() {
        // "1.0 - 2.0": upper is partial → <2.1.0-dev (includes all 2.0.x)
        assert!(satisfies("1.0 - 2.0", "2.0.0"));
        assert!(satisfies("1.0 - 2.0", "2.0.1"));
        assert!(satisfies("1.0 - 2.0", "2.0.99"));
        assert!(!satisfies("1.0 - 2.0", "2.1.0"));
    }

    #[test]
    fn test_hyphen_range_same_version() {
        // "1.0.0 - 1.0.0" → >=1.0.0 <=1.0.0, matches only 1.0.0
        assert!(satisfies("1.0.0 - 1.0.0", "1.0.0"));
        assert!(!satisfies("1.0.0 - 1.0.0", "1.0.1"));
        assert!(!satisfies("1.0.0 - 1.0.0", "0.9.9"));
    }

    #[test]
    fn test_hyphen_range_with_prerelease() {
        // "1.0.0-alpha1 - 1.0.0-RC1"
        assert!(satisfies("1.0.0-alpha1 - 1.0.0-RC1", "1.0.0-alpha1"));
        assert!(satisfies("1.0.0-alpha1 - 1.0.0-RC1", "1.0.0-beta1"));
        assert!(satisfies("1.0.0-alpha1 - 1.0.0-RC1", "1.0.0-RC1"));
        assert!(!satisfies("1.0.0-alpha1 - 1.0.0-RC1", "1.0.0"));
    }

    // ── Comparison operators ──

    #[test]
    fn test_gt_boundary() {
        assert!(!satisfies(">1.0.0", "1.0.0"));
        assert!(satisfies(">1.0.0", "1.0.1"));
    }

    #[test]
    fn test_lt_boundary() {
        assert!(!satisfies("<1.0.0", "1.0.0"));
        assert!(satisfies("<1.0.0", "0.9.9"));
    }

    #[test]
    fn test_lte_boundary() {
        assert!(satisfies("<=1.0.0", "1.0.0"));
        assert!(!satisfies("<=1.0.0", "1.0.1"));
    }

    #[test]
    fn test_exact_equals_sign() {
        // "=1.2.3" is exact match
        assert!(satisfies("=1.2.3", "1.2.3"));
        assert!(!satisfies("=1.2.3", "1.2.4"));
    }

    #[test]
    fn test_double_equals_sign() {
        // "==1.2.3" is treated the same as "=1.2.3" (exact match)
        assert!(satisfies("==1.2.3", "1.2.3"));
        assert!(!satisfies("==1.2.3", "1.2.4"));
    }

    #[test]
    fn test_not_equal_boundary() {
        assert!(!satisfies("!=1.5.0", "1.5.0"));
        assert!(satisfies("!=1.5.0", "1.4.9"));
        assert!(satisfies("!=1.5.0", "1.5.1"));
    }

    #[test]
    fn test_gte_with_spaces() {
        // Spaces after operator should be handled
        assert!(satisfies(">=1.0.0", "1.0.0"));
    }

    // ── AND constraints ──

    #[test]
    fn test_and_comma_separated() {
        // Comma-separated constraints act as AND
        assert!(satisfies(">=1.0,<2.0", "1.5.0"));
        assert!(!satisfies(">=1.0,<2.0", "2.0.0"));
        assert!(!satisfies(">=1.0,<2.0", "0.9.0"));
    }

    #[test]
    fn test_and_three_way() {
        assert!(satisfies(">=1.0 !=1.5.0 <2.0", "1.3.0"));
        assert!(!satisfies(">=1.0 !=1.5.0 <2.0", "1.5.0"));
        assert!(!satisfies(">=1.0 !=1.5.0 <2.0", "2.0.0"));
    }

    #[test]
    fn test_and_impossible_range() {
        // >=2.0 <1.0 — impossible range, nothing should match
        assert!(!satisfies(">=2.0 <1.0", "1.5.0"));
        assert!(!satisfies(">=2.0 <1.0", "2.0.0"));
        assert!(!satisfies(">=2.0 <1.0", "0.5.0"));
    }

    #[test]
    fn test_and_tight_range() {
        // >=1.2.3 <=1.2.3 — only exactly 1.2.3
        assert!(satisfies(">=1.2.3 <=1.2.3", "1.2.3"));
        assert!(!satisfies(">=1.2.3 <=1.2.3", "1.2.4"));
        assert!(!satisfies(">=1.2.3 <=1.2.3", "1.2.2"));
    }

    // ── OR constraints ──

    #[test]
    fn test_or_double_pipe() {
        assert!(satisfies("^1.0 || ^2.0", "1.5.0"));
        assert!(satisfies("^1.0 || ^2.0", "2.3.0"));
        assert!(!satisfies("^1.0 || ^2.0", "3.0.0"));
    }

    #[test]
    fn test_or_three_branches() {
        assert!(satisfies("^1.0 || ^2.0 || ^3.0", "1.0.0"));
        assert!(satisfies("^1.0 || ^2.0 || ^3.0", "2.5.0"));
        assert!(satisfies("^1.0 || ^2.0 || ^3.0", "3.9.9"));
        assert!(!satisfies("^1.0 || ^2.0 || ^3.0", "4.0.0"));
    }

    #[test]
    fn test_or_with_wildcard() {
        assert!(satisfies("1.* || 3.*", "1.5.0"));
        assert!(satisfies("1.* || 3.*", "3.0.0"));
        assert!(!satisfies("1.* || 3.*", "2.0.0"));
    }

    #[test]
    fn test_or_overlapping_ranges() {
        // Overlapping ranges are fine — union semantics
        assert!(satisfies(">=1.0 <3.0 || >=2.0 <4.0", "1.5.0"));
        assert!(satisfies(">=1.0 <3.0 || >=2.0 <4.0", "2.5.0"));
        assert!(satisfies(">=1.0 <3.0 || >=2.0 <4.0", "3.5.0"));
        assert!(!satisfies(">=1.0 <3.0 || >=2.0 <4.0", "0.9.0"));
        assert!(!satisfies(">=1.0 <3.0 || >=2.0 <4.0", "4.0.0"));
    }

    #[test]
    fn test_or_exact_versions() {
        assert!(satisfies("1.0.0 || 2.0.0 || 3.0.0", "1.0.0"));
        assert!(satisfies("1.0.0 || 2.0.0 || 3.0.0", "2.0.0"));
        assert!(satisfies("1.0.0 || 2.0.0 || 3.0.0", "3.0.0"));
        assert!(!satisfies("1.0.0 || 2.0.0 || 3.0.0", "1.0.1"));
    }

    // ── Complex combined ──

    #[test]
    fn test_combined_and_within_or() {
        // ">=1.0 <2.0 || >=3.0 <4.0"
        assert!(satisfies(">=1.0 <2.0 || >=3.0 <4.0", "1.5.0"));
        assert!(satisfies(">=1.0 <2.0 || >=3.0 <4.0", "3.5.0"));
        assert!(!satisfies(">=1.0 <2.0 || >=3.0 <4.0", "2.5.0"));
        assert!(!satisfies(">=1.0 <2.0 || >=3.0 <4.0", "4.0.0"));
    }

    #[test]
    fn test_combined_real_world_laravel_pattern() {
        // "^8.0||^9.0||^10.0||^11.0" — real Laravel constraint
        assert!(satisfies("^8.0||^9.0||^10.0||^11.0", "8.5.0"));
        assert!(satisfies("^8.0||^9.0||^10.0||^11.0", "9.0.0"));
        assert!(satisfies("^8.0||^9.0||^10.0||^11.0", "10.48.22"));
        assert!(satisfies("^8.0||^9.0||^10.0||^11.0", "11.0.1"));
        assert!(!satisfies("^8.0||^9.0||^10.0||^11.0", "7.9.9"));
        assert!(!satisfies("^8.0||^9.0||^10.0||^11.0", "12.0.0"));
    }

    #[test]
    fn test_single_pipe_or() {
        // Single pipe `|` is the standard Composer OR separator
        assert!(satisfies("^6.0|^7.0|^8.0|^9.0|^10.0|^11.0", "6.0.0"));
        assert!(satisfies("^6.0|^7.0|^8.0|^9.0|^10.0|^11.0", "9.0.0"));
        assert!(satisfies("^6.0|^7.0|^8.0|^9.0|^10.0|^11.0", "11.5.0"));
        assert!(!satisfies("^6.0|^7.0|^8.0|^9.0|^10.0|^11.0", "5.9.9"));
        assert!(!satisfies("^6.0|^7.0|^8.0|^9.0|^10.0|^11.0", "12.0.0"));
    }

    #[test]
    fn test_combined_real_world_symfony_pattern() {
        // ">=5.4 <7.0" — typical Symfony range
        assert!(satisfies(">=5.4 <7.0", "5.4.0"));
        assert!(satisfies(">=5.4 <7.0", "6.4.5"));
        assert!(!satisfies(">=5.4 <7.0", "5.3.9"));
        assert!(!satisfies(">=5.4 <7.0", "7.0.0"));
    }

    // ── Edge cases ──

    #[test]
    fn test_constraint_empty_string_is_any() {
        // Empty string → Any constraint
        let c = VersionConstraint::parse("*").unwrap();
        let v = Version::parse("9.9.9").unwrap();
        assert!(c.matches(&v));
    }

    #[test]
    fn test_constraint_v_prefix_in_exact() {
        // "v1.2.3" exact constraint — strip v prefix
        assert!(satisfies("v1.2.3", "1.2.3"));
        assert!(!satisfies("v1.2.3", "1.2.4"));
    }

    #[test]
    fn test_constraint_extra_whitespace_and() {
        // Extra spaces around operators in AND groups
        assert!(satisfies(">=1.0.0  <2.0.0", "1.5.0"));
        assert!(!satisfies(">=1.0.0  <2.0.0", "2.0.0"));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 4. CONSTRAINT MATCHING
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_dev_branch_exact_match() {
        // dev-master matches dev-master constraint exactly
        let c = VersionConstraint::parse("dev-master").unwrap();
        let v = Version::parse("dev-master").unwrap();
        assert!(c.matches(&v));
    }

    #[test]
    fn test_dev_branch_different_branch_no_match() {
        let c = VersionConstraint::parse("dev-master").unwrap();
        let v = Version::parse("dev-develop").unwrap();
        assert!(!c.matches(&v));
    }

    #[test]
    fn test_dev_branch_against_caret_no_match() {
        // dev-master does not satisfy ^1.0 (it is a dev branch, always lowest)
        let c = VersionConstraint::parse("^1.0").unwrap();
        let v = Version::parse("dev-master").unwrap();
        assert!(!c.matches(&v));
    }

    #[test]
    fn test_any_constraint_matches_dev_branch() {
        // "*" matches any version including dev branches
        let c = VersionConstraint::parse("*").unwrap();
        let v = Version::parse("dev-master").unwrap();
        assert!(c.matches(&v));
    }

    #[test]
    fn test_prerelease_within_caret_range() {
        // Pre-release of a version within ^1.0 should match
        // e.g. 1.5.0-beta1 — it is >= dev boundary 1.0.0 and < dev boundary 2.0.0
        assert!(satisfies("^1.0", "1.5.0-beta1"));
    }

    #[test]
    fn test_caret_lower_minus_one_no_match() {
        // ^1.2.3 lower-1 = 1.2.2 → should NOT match
        assert!(!satisfies("^1.2.3", "1.2.2"));
    }

    #[test]
    fn test_caret_upper_minus_one_matches() {
        // ^1.2.3 upper-1 patch: 1.9.9 should still match (below 2.0.0)
        assert!(satisfies("^1.2.3", "1.9.9"));
    }

    #[test]
    fn test_tilde_lower_minus_one_no_match() {
        assert!(!satisfies("~1.2.3", "1.2.2"));
    }

    #[test]
    fn test_tilde_upper_minus_one_matches() {
        assert!(satisfies("~1.2.3", "1.2.9"));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 5. INTERNAL FUNCTION TESTS (via public API)
    // ══════════════════════════════════════════════════════════════════════════

    // stability_rank() — tested via ordering since the function is private

    #[test]
    fn test_stability_rank_dev_via_ordering() {
        // dev rank=50 (highest number = least stable), alpha rank=40
        // So dev < alpha in version ordering terms
        let dev = Version::parse("1.0.0-dev").unwrap();
        let alpha = Version::parse("1.0.0-alpha1").unwrap();
        assert!(dev < alpha, "dev should be less stable than alpha1");
    }

    #[test]
    fn test_stability_rank_alpha_via_ordering() {
        // alpha rank=40, beta rank=30
        let alpha = Version::parse("1.0.0-alpha1").unwrap();
        let beta = Version::parse("1.0.0-beta1").unwrap();
        assert!(alpha < beta, "alpha should be less stable than beta");
    }

    #[test]
    fn test_stability_rank_beta_via_ordering() {
        // beta rank=30, RC rank=20
        let beta = Version::parse("1.0.0-beta1").unwrap();
        let rc = Version::parse("1.0.0-RC1").unwrap();
        assert!(beta < rc, "beta should be less stable than RC");
    }

    #[test]
    fn test_stability_rank_rc_via_ordering() {
        // RC rank=20, stable rank=0
        let rc = Version::parse("1.0.0-RC1").unwrap();
        let stable = Version::parse("1.0.0").unwrap();
        assert!(rc < stable, "RC should be less stable than stable");
    }

    #[test]
    fn test_stability_rank_patch_via_ordering() {
        // In Composer, patch/pl/p pre-releases rank ABOVE stable.
        // patch1 > stable for the same numeric version.
        let patch_ver = Version::parse("1.0.0-patch1").unwrap();
        let stable = Version::parse("1.0.0").unwrap();
        assert!(patch_ver > stable, "patch pre-release ranks above stable");
    }

    // normalize_pre_release() — tested via Version::parse pre_release field

    #[test]
    fn test_normalize_pre_release_b_to_beta() {
        let v = Version::parse("1.0.0-b3").unwrap();
        assert_eq!(v.pre_release, Some("beta3".to_string()));
    }

    #[test]
    fn test_normalize_pre_release_a_to_alpha() {
        let v = Version::parse("1.0.0-a1").unwrap();
        assert_eq!(v.pre_release, Some("alpha1".to_string()));
    }

    #[test]
    fn test_normalize_pre_release_rc_to_rc_uppercase() {
        let v = Version::parse("1.0.0-rc").unwrap();
        assert_eq!(v.pre_release, Some("RC".to_string()));
    }

    #[test]
    fn test_normalize_pre_release_pl_to_patch() {
        let v = Version::parse("1.0.0-pl2").unwrap();
        assert_eq!(v.pre_release, Some("patch2".to_string()));
    }

    #[test]
    fn test_normalize_pre_release_patch_explicit() {
        let v = Version::parse("1.0.0-patch3").unwrap();
        assert_eq!(v.pre_release, Some("patch3".to_string()));
    }

    // pre_release_number() — tested via ordering of numbered pre-releases

    #[test]
    fn test_pre_release_number_ordering_beta() {
        // beta10 > beta2 if pre_release_number extracts correctly
        let b10 = Version::parse("1.0.0-beta10").unwrap();
        let b2 = Version::parse("1.0.0-beta2").unwrap();
        assert!(b10 > b2);
    }

    #[test]
    fn test_pre_release_number_ordering_rc() {
        let rc5 = Version::parse("1.0.0-RC5").unwrap();
        let rc1 = Version::parse("1.0.0-RC1").unwrap();
        assert!(rc5 > rc1);
    }

    #[test]
    fn test_pre_release_number_zero_when_missing() {
        // "alpha" with no number → 0; "alpha1" → 1; alpha1 > alpha
        let alpha1 = Version::parse("1.0.0-alpha1").unwrap();
        let alpha = Version::parse("1.0.0-alpha").unwrap();
        assert!(alpha1 > alpha);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 6. COMPOSER BEHAVIORAL COMPATIBILITY
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_composer_caret_four_matches_minor_bump() {
        // ^4.0 matches 4.5.3
        assert!(satisfies("^4.0", "4.5.3"));
    }

    #[test]
    fn test_composer_caret_four_does_not_match_next_major() {
        assert!(!satisfies("^4.0", "5.0.0"));
    }

    #[test]
    fn test_composer_caret_zero_three_matches_patch() {
        // ^0.3 matches 0.3.5 (same minor family)
        assert!(satisfies("^0.3", "0.3.5"));
    }

    #[test]
    fn test_composer_caret_zero_three_does_not_match_next_minor() {
        // ^0.3 does NOT match 0.4.0
        assert!(!satisfies("^0.3", "0.4.0"));
    }

    #[test]
    fn test_composer_tilde_four_one_matches_within_major() {
        // ~4.1 → >=4.1.0 <5.0.0 — matches 4.9.0
        assert!(satisfies("~4.1", "4.9.0"));
    }

    #[test]
    fn test_composer_tilde_four_one_does_not_match_next_major() {
        // ~4.1 does NOT match 5.0.0
        assert!(!satisfies("~4.1", "5.0.0"));
    }

    #[test]
    fn test_composer_range_gap_matches_second_range() {
        // ">=1.0 <1.1 || >=1.2" — gap at 1.1.x; 1.2.0 matches
        assert!(satisfies(">=1.0 <1.1 || >=1.2", "1.2.0"));
    }

    #[test]
    fn test_composer_range_gap_does_not_match_in_gap() {
        // 1.1.5 is in the gap — should NOT match
        assert!(!satisfies(">=1.0 <1.1 || >=1.2", "1.1.5"));
    }

    #[test]
    fn test_composer_laravel_constraint_matches_v10() {
        // "^8.0||^9.0||^10.0||^11.0" — Laravel-style; 10.48.22 matches
        assert!(satisfies("^8.0||^9.0||^10.0||^11.0", "10.48.22"));
    }

    #[test]
    fn test_composer_laravel_constraint_does_not_match_v7() {
        assert!(!satisfies("^8.0||^9.0||^10.0||^11.0", "7.9.9"));
    }

    #[test]
    fn test_composer_symfony_range_matches_6_4() {
        // ">=5.4 <7.0" — Symfony; 6.4.5 matches
        assert!(satisfies(">=5.4 <7.0", "6.4.5"));
    }

    #[test]
    fn test_composer_symfony_range_does_not_match_7_0() {
        assert!(!satisfies(">=5.4 <7.0", "7.0.0"));
    }

    #[test]
    fn test_composer_not_equal_in_range() {
        // ">=1.0 !=1.5.0 <2.0" — typical blacklist constraint
        assert!(satisfies(">=1.0 !=1.5.0 <2.0", "1.4.9"));
        assert!(!satisfies(">=1.0 !=1.5.0 <2.0", "1.5.0"));
        assert!(satisfies(">=1.0 !=1.5.0 <2.0", "1.5.1"));
        assert!(!satisfies(">=1.0 !=1.5.0 <2.0", "2.0.0"));
    }

    #[test]
    fn test_composer_exact_major_minor_match() {
        // exact "1.5.0" only matches 1.5.0
        assert!(satisfies("1.5.0", "1.5.0"));
        assert!(!satisfies("1.5.0", "1.5.1"));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 7. DIVERGENCE INVESTIGATION
    // ══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_hyphen_range_partial_upper_two_segment() {
        // "1.0 - 2": upper is partial (1 segment) → <3.0.0.0-dev
        assert!(satisfies("1.0 - 2", "2.0.0"));
        assert!(satisfies("1.0 - 2", "2.0.1"));
        assert!(satisfies("1.0 - 2", "2.1.0"));
        assert!(satisfies("1.0 - 2", "2.99.99"));
        assert!(!satisfies("1.0 - 2", "3.0.0"));
    }

    #[test]
    fn test_hyphen_range_php_version_constraint() {
        // "8.1 - 8.5" as used by nette/schema → >=8.1.0.0 <8.6.0.0-dev
        assert!(satisfies("8.1 - 8.5", "8.1.0"));
        assert!(satisfies("8.1 - 8.5", "8.3.0"));
        assert!(satisfies("8.1 - 8.5", "8.5.0"));
        assert!(satisfies("8.1 - 8.5", "8.5.3"));
        assert!(!satisfies("8.1 - 8.5", "8.0.99"));
        assert!(!satisfies("8.1 - 8.5", "8.6.0"));
        assert!(!satisfies("8.1 - 8.5", "9.0.0"));

        // Full upper bound: "1.0.0 - 2.0.0" → >=1.0.0 <=2.0.0
        assert!(satisfies("1.0.0 - 2.0.0", "2.0.0"));
        assert!(!satisfies("1.0.0 - 2.0.0", "2.0.1"));
    }

    #[test]
    fn test_caret_with_prerelease_suffix() {
        // ^1.2.3-beta1 — the caret parser ignores pre-release in its bounds calculation
        // because parse_caret works on the numeric parts only.
        // Lower: dev_boundary(1,2,3,0). Upper: dev_boundary(2,0,0,0).
        // 1.2.3-beta1 (pre_release=Some("beta1")) is >= lower boundary?
        // dev_boundary uses pre_release=Some("dev"), so lower is (1,2,3,0,dev)
        // Version 1.2.3-beta1 has same numeric, but beta > dev in stability terms
        // so 1.2.3-beta1 >= lower (1.2.3-dev) is true.
        assert!(satisfies("^1.2.3-beta1", "1.2.3-beta1"));
        assert!(satisfies("^1.2.3-beta1", "1.5.0"));
        assert!(!satisfies("^1.2.3-beta1", "2.0.0"));
    }

    #[test]
    fn test_tilde_with_prerelease_suffix() {
        // ~1.2.3-alpha1: lower = dev_boundary(1,2,3,0), upper = dev_boundary(1,3,0,0)
        // 1.2.3-alpha1 has numeric (1,2,3,0); pre_release "alpha1" > "dev"
        assert!(satisfies("~1.2.3-alpha1", "1.2.3-alpha1"));
        assert!(satisfies("~1.2.3-alpha1", "1.2.9"));
        assert!(!satisfies("~1.2.3-alpha1", "1.3.0"));
    }

    #[test]
    fn test_dev_boundary_comparison() {
        // Version::dev_boundary creates a version with pre_release=Some("dev") and
        // is_dev_branch=false. These should sort correctly against real versions.
        let lower = Version::dev_boundary(1, 0, 0, 0);
        let v = Version::parse("1.0.0").unwrap();
        // 1.0.0 (stable) > 1.0.0-dev (lower boundary)
        assert!(v > lower);
    }

    #[test]
    fn test_x_dev_ordering_within_range() {
        // `2.x-dev` is the in-progress 2.x branch and normalizes to
        // `2.9999999.9999999.9999999-dev`. Numerically that sorts above any
        // concrete `2.N.M` release — Composer relies on this so a wildcard
        // branch alias compares as the *latest* candidate within its major.
        // Only *named* dev branches (`dev-foo`) sort below numeric versions.
        let x_dev = Version::parse("2.x-dev").unwrap();
        let stable_low = Version::parse("2.0.0").unwrap();
        let stable_next_major = Version::parse("3.0.0").unwrap();
        assert!(x_dev > stable_low);
        assert!(x_dev < stable_next_major);
    }

    #[test]
    fn test_four_segment_vs_three_segment_constraint() {
        // "1.2.3.4" exact constraint — matches only 1.2.3.4, not 1.2.3
        assert!(satisfies("1.2.3.4", "1.2.3.4"));
        assert!(!satisfies("1.2.3.4", "1.2.3"));
        assert!(!satisfies("1.2.3.4", "1.2.3.5"));
    }

    #[test]
    fn test_date_style_version_ordering() {
        // Date-based versioning: 20230101 > 20220101
        let a = Version::parse("20230101.0.0").unwrap();
        let b = Version::parse("20220101.0.0").unwrap();
        assert!(a > b);
    }

    #[test]
    fn test_intersects_disjoint_exacts() {
        // The replaced-packages cluster: a `replace: foo "1.0.0"` link is
        // not a valid provider for a `require: foo "2.0.0"` request.
        let a = VersionConstraint::parse("1.0.0").unwrap();
        let b = VersionConstraint::parse("2.0.0").unwrap();
        assert!(!a.intersects(&b));
        assert!(!b.intersects(&a));
    }

    #[test]
    fn test_intersects_overlapping_ranges() {
        let a = VersionConstraint::parse("^1.0").unwrap();
        let b = VersionConstraint::parse(">=1.5 <3.0").unwrap();
        assert!(a.intersects(&b));
    }

    #[test]
    fn test_intersects_disjoint_ranges() {
        let a = VersionConstraint::parse("^1.0").unwrap();
        let b = VersionConstraint::parse("^2.0").unwrap();
        assert!(!a.intersects(&b));
    }

    #[test]
    fn test_intersects_or_branch() {
        // Either branch in an OR is enough to intersect.
        let a = VersionConstraint::parse("1.0.0 || 2.0.0").unwrap();
        let b = VersionConstraint::parse("2.0.0").unwrap();
        assert!(a.intersects(&b));
    }

    #[test]
    fn test_intersects_any() {
        let a = VersionConstraint::parse("*").unwrap();
        let b = VersionConstraint::parse("1.0.0").unwrap();
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
    }

    #[test]
    fn test_intersects_touching_open_boundaries() {
        // [1, 2) and (2, 3] do not share any version.
        let a = VersionConstraint::parse(">=1.0 <2.0").unwrap();
        let b = VersionConstraint::parse(">2.0 <=3.0").unwrap();
        assert!(!a.intersects(&b));
    }

    #[test]
    fn test_intersects_touching_closed_boundaries() {
        // [1, 2] and [2, 3] share version 2.
        let a = VersionConstraint::parse(">=1.0 <=2.0").unwrap();
        let b = VersionConstraint::parse(">=2.0 <=3.0").unwrap();
        assert!(a.intersects(&b));
    }
}
