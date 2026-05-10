use crate::package::version::VersionParser;
use crate::vcs::process::ProcessExecutor;
use mozart_semver::{Version, normalize_branch};
use regex::Regex;
use serde_json::Value;
use std::path::Path;
use std::sync::LazyLock;

const DEFAULT_BRANCH_ALIAS: &str = "9999999-dev";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuessedVersion {
    pub version: String,
    pub commit: Option<String>,
    pub pretty_version: Option<String>,
    pub feature_version: Option<String>,
    pub feature_pretty_version: Option<String>,
}

pub struct VersionGuesser {
    process: ProcessExecutor,
}

impl Default for VersionGuesser {
    fn default() -> Self {
        Self::new(VersionParser::new())
    }
}

impl VersionGuesser {
    /// Mirrors `Composer\Package\Version\VersionGuesser::__construct`.
    /// `_version_parser` is accepted for API parity but unused — Rust relies
    /// on `mozart_semver` directly.
    pub fn new(_version_parser: VersionParser) -> Self {
        Self {
            process: ProcessExecutor::new(),
        }
    }

    /// `Composer\Package\Version\VersionGuesser::guessVersion`.
    pub fn guess_version(&self, package_config: &Value, path: &Path) -> Option<GuessedVersion> {
        if let Some(v) = self.guess_git_version(package_config, path) {
            return Some(postprocess(v));
        }
        if let Some(v) = self.guess_hg_version(package_config, path) {
            return Some(postprocess(v));
        }
        if let Some(v) = self.guess_svn_version(package_config, path) {
            return Some(postprocess(v));
        }
        None
    }

    fn guess_git_version(&self, package_config: &Value, path: &Path) -> Option<GuessedVersion> {
        let mut commit: Option<String> = None;
        let mut version: Option<String> = None;
        let mut pretty_version: Option<String> = None;
        let mut feature_version: Option<String> = None;
        let mut feature_pretty_version: Option<String> = None;
        let mut is_detached = false;

        let branch_out = self
            .process
            .execute(
                &["git", "branch", "-a", "--no-color", "--no-abbrev", "-v"],
                Some(path),
            )
            .ok()?;
        if branch_out.status != 0 {
            return None;
        }

        let mut branches: Vec<String> = Vec::new();
        let mut is_feature_branch = false;

        for line in branch_out.stdout.lines() {
            if line.is_empty() {
                continue;
            }
            if let Some(caps) = CURRENT_BRANCH_RE.captures(line) {
                let name = caps.get(1).map_or("", |m| m.as_str());
                let hash = caps.get(2).map_or("", |m| m.as_str());
                if name == "(no branch)"
                    || name.starts_with("(detached ")
                    || name.starts_with("(HEAD detached at")
                {
                    let v = format!("dev-{hash}");
                    version = Some(v.clone());
                    pretty_version = Some(v);
                    is_feature_branch = true;
                    is_detached = true;
                } else {
                    version = Some(normalize_branch(name));
                    pretty_version = Some(format!("dev-{name}"));
                    is_feature_branch = is_feature_branch_name(package_config, name);
                }
                commit = Some(hash.to_string());
            }

            if !REMOTE_HEAD_RE.is_match(line)
                && let Some(caps) = ANY_BRANCH_RE.captures(line)
                && let Some(m) = caps.get(1)
            {
                branches.push(m.as_str().to_string());
            }
        }

        if is_feature_branch {
            feature_version = version.clone();
            feature_pretty_version = pretty_version.clone();
            let result = self.guess_feature_version(
                package_config,
                version.as_deref(),
                &branches,
                &["git", "rev-list", "%candidate%..%branch%"],
                path,
            );
            version = result.0;
            pretty_version = result.1;
        }

        if (version.is_none() || is_detached)
            && let Some((tag_v, tag_pretty)) = self.version_from_git_tags(path)
        {
            version = Some(tag_v);
            pretty_version = Some(tag_pretty);
            feature_version = None;
            feature_pretty_version = None;
        }

        if commit.is_none()
            && let Ok(out) = self
                .process
                .execute(&["git", "rev-parse", "HEAD"], Some(path))
            && out.status == 0
        {
            let trimmed = out.stdout.trim();
            if !trimmed.is_empty() {
                commit = Some(trimmed.to_string());
            }
        }

        version.as_ref()?;
        Some(GuessedVersion {
            version: version.unwrap(),
            commit,
            pretty_version,
            feature_version,
            feature_pretty_version,
        })
    }

    fn version_from_git_tags(&self, path: &Path) -> Option<(String, String)> {
        let out = self
            .process
            .execute(&["git", "describe", "--exact-match", "--tags"], Some(path))
            .ok()?;
        if out.status != 0 {
            return None;
        }
        let pretty = out.stdout.trim().to_string();
        if pretty.is_empty() {
            return None;
        }
        let normalized = Version::parse(&pretty).ok()?;
        Some((normalized.to_string(), pretty))
    }

    fn guess_hg_version(&self, package_config: &Value, path: &Path) -> Option<GuessedVersion> {
        let out = self.process.execute(&["hg", "branch"], Some(path)).ok()?;
        if out.status != 0 {
            return None;
        }
        let branch = out.stdout.trim().to_string();
        if branch.is_empty() {
            return None;
        }
        let version = normalize_branch(&branch);
        let is_feature = version.starts_with("dev-");

        if version == DEFAULT_BRANCH_ALIAS {
            return Some(GuessedVersion {
                version,
                commit: None,
                pretty_version: Some(format!("dev-{branch}")),
                feature_version: None,
                feature_pretty_version: None,
            });
        }

        if !is_feature {
            return Some(GuessedVersion {
                version: version.clone(),
                commit: None,
                pretty_version: Some(version),
                feature_version: None,
                feature_pretty_version: None,
            });
        }

        // List branches via `hg branches` (first whitespace-separated token per line).
        let branches_out = self.process.execute(&["hg", "branches"], Some(path)).ok()?;
        let branches: Vec<String> = if branches_out.status == 0 {
            branches_out
                .stdout
                .lines()
                .filter_map(|l| l.split_whitespace().next().map(str::to_string))
                .collect()
        } else {
            Vec::new()
        };

        let (out_version, out_pretty) = self.guess_feature_version(
            package_config,
            Some(&version),
            &branches,
            &[
                "hg",
                "log",
                "-r",
                "not ancestors('%candidate%') and ancestors('%branch%')",
                "--template",
                "\"{node}\\n\"",
            ],
            path,
        );

        Some(GuessedVersion {
            version: out_version.unwrap_or(version.clone()),
            commit: Some(String::new()),
            pretty_version: out_pretty,
            feature_version: Some(version.clone()),
            feature_pretty_version: Some(version),
        })
    }

    fn guess_svn_version(&self, package_config: &Value, path: &Path) -> Option<GuessedVersion> {
        let out = self
            .process
            .execute(&["svn", "info", "--xml"], Some(path))
            .ok()?;
        if out.status != 0 {
            return None;
        }

        let trunk = package_config
            .get("trunk-path")
            .and_then(Value::as_str)
            .unwrap_or("trunk");
        let branches = package_config
            .get("branches-path")
            .and_then(Value::as_str)
            .unwrap_or("branches");
        let tags = package_config
            .get("tags-path")
            .and_then(Value::as_str)
            .unwrap_or("tags");

        let pattern = format!(
            r"<url>.*/({trunk}|({branches}|{tags})/(.*))</url>",
            trunk = regex::escape(trunk),
            branches = regex::escape(branches),
            tags = regex::escape(tags),
        );
        let re = Regex::new(&pattern).ok()?;
        let caps = re.captures(&out.stdout)?;

        let kind = caps.get(2).map(|m| m.as_str().to_string());
        let inner = caps.get(3).map(|m| m.as_str().to_string());

        if let (Some(kind), Some(inner)) = (kind, inner)
            && (kind == branches || kind == tags)
        {
            let pretty = format!("dev-{inner}");
            return Some(GuessedVersion {
                version: normalize_branch(&inner),
                commit: Some(String::new()),
                pretty_version: Some(pretty),
                feature_version: None,
                feature_pretty_version: None,
            });
        }

        let trunk_match = caps.get(1)?;
        let pretty = trunk_match.as_str().trim().to_string();
        let version = if pretty == "trunk" {
            "dev-trunk".to_string()
        } else {
            Version::parse(&pretty).ok()?.to_string()
        };
        Some(GuessedVersion {
            version,
            commit: Some(String::new()),
            pretty_version: Some(pretty),
            feature_version: None,
            feature_pretty_version: None,
        })
    }

    /// Find the nearest non-feature branch by diff size. Sequential port of
    /// `guessFeatureVersion`; Composer runs candidates in parallel.
    fn guess_feature_version(
        &self,
        package_config: &Value,
        version: Option<&str>,
        branches: &[String],
        scm_cmdline: &[&str],
        path: &Path,
    ) -> (Option<String>, Option<String>) {
        let version = version.map(str::to_string);
        let pretty_version = version.clone();

        let Some(v) = version.clone() else {
            return (version, pretty_version);
        };

        // Skip if the branch has a non-self.version branch-alias OR self.version is referenced.
        let has_branch_alias = package_config
            .get("extra")
            .and_then(|e| e.get("branch-alias"))
            .and_then(|b| b.get(&v))
            .is_some();
        let uses_self_version = serde_json::to_string(package_config)
            .map(|s| s.contains("\"self.version\""))
            .unwrap_or(false);
        if has_branch_alias && !uses_self_version {
            return (Some(v), pretty_version);
        }

        // Composer also returns early if `self.version` is referenced — see L283.
        // The PHP precedence is: skip iff (no branch-alias) OR (json contains self.version).
        if uses_self_version {
            return (Some(v), pretty_version);
        }

        let branch = v.strip_prefix("dev-").unwrap_or(&v).to_string();

        if !is_feature_branch_name(package_config, &branch) {
            return (Some(v), pretty_version);
        }

        let mut sorted: Vec<String> = branches.to_vec();
        sorted.sort_by(|a, b| {
            let a_remote = a.starts_with("remotes/");
            let b_remote = b.starts_with("remotes/");
            if a_remote != b_remote {
                return if a_remote {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Less
                };
            }
            // strnatcasecmp(b, a) — natural-sort, descending, case-insensitive.
            natural_cmp(&b.to_ascii_lowercase(), &a.to_ascii_lowercase())
        });

        let mut last_index: i64 = -1;
        let mut length: usize = usize::MAX;
        let mut version = Some(v);
        let mut pretty = pretty_version;

        for (index, candidate) in sorted.iter().enumerate() {
            let candidate_version = REMOTES_PREFIX_RE.replace(candidate, "").to_string();
            if candidate.as_str() == branch.as_str()
                || is_feature_branch_name(package_config, &candidate_version)
            {
                continue;
            }
            let cmd: Vec<String> = scm_cmdline
                .iter()
                .map(|c| {
                    c.replace("%candidate%", candidate)
                        .replace("%branch%", &branch)
                })
                .collect();
            let cmd_refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let Ok(output) = self.process.execute(&cmd_refs, Some(path)) else {
                continue;
            };
            if output.status != 0 {
                continue;
            }
            let len = output.stdout.len();
            if len < length || (len == length && last_index < index as i64) {
                last_index = index as i64;
                length = len;
                version = Some(normalize_branch(&candidate_version));
                pretty = Some(format!("dev-{candidate_version}"));
                if length == 0 {
                    break;
                }
            }
        }

        (version, pretty)
    }
}

fn postprocess(mut v: GuessedVersion) -> GuessedVersion {
    if v.feature_version.is_some()
        && v.feature_version == Some(v.version.clone())
        && v.feature_pretty_version == v.pretty_version
    {
        v.feature_version = None;
        v.feature_pretty_version = None;
    }

    if v.version.ends_with("-dev") && contains_long_nines(&v.version) {
        v.pretty_version = Some(replace_long_nines_with_x(&v.version));
    }
    if let Some(ref fv) = v.feature_version
        && fv.ends_with("-dev")
        && contains_long_nines(fv)
    {
        v.feature_pretty_version = Some(replace_long_nines_with_x(fv));
    }
    v
}

fn contains_long_nines(s: &str) -> bool {
    NINE_SEVEN_RE.is_match(s)
}

fn replace_long_nines_with_x(s: &str) -> String {
    NINE_SEVEN_GROUP_RE.replace_all(s, ".x").to_string()
}

fn is_feature_branch_name(package_config: &Value, branch_name: &str) -> bool {
    let mut non_feature = String::new();
    if let Some(arr) = package_config
        .get("non-feature-branches")
        .and_then(Value::as_array)
    {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if !parts.is_empty() {
            non_feature = parts.join("|");
        }
    }
    let pattern = format!(
        r"^({non_feature}|master|main|latest|next|current|support|tip|trunk|default|develop|\d+\..+)$"
    );
    let Ok(re) = Regex::new(&pattern) else {
        return true;
    };
    !re.is_match(branch_name)
}

/// Natural-order, case-insensitive string comparison (mirrors PHP `strnatcasecmp`).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let mut na = String::new();
                    let mut nb = String::new();
                    while let Some(&c) = ai.peek() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        na.push(c);
                        ai.next();
                    }
                    while let Some(&c) = bi.peek() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        nb.push(c);
                        bi.next();
                    }
                    let na_v: u128 = na.parse().unwrap_or(0);
                    let nb_v: u128 = nb.parse().unwrap_or(0);
                    match na_v.cmp(&nb_v) {
                        std::cmp::Ordering::Equal => continue,
                        ord => return ord,
                    }
                } else {
                    match ac.cmp(&bc) {
                        std::cmp::Ordering::Equal => {
                            ai.next();
                            bi.next();
                        }
                        ord => return ord,
                    }
                }
            }
        }
    }
}

static CURRENT_BRANCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:\* ) *(\(no branch\)|\(detached from \S+\)|\(HEAD detached at \S+\)|\S+) *([a-f0-9]+) .*$",
    )
    .unwrap()
});

static REMOTE_HEAD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ *.+/HEAD ").unwrap());

static ANY_BRANCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\* )? *((?:remotes/(?:origin|upstream)/)?[^\s/]+) *([a-f0-9]+) .*$").unwrap()
});

static REMOTES_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^remotes/[^/]+/").unwrap());

static NINE_SEVEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.9{7}").unwrap());

static NINE_SEVEN_GROUP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\.9{7})+").unwrap());

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_postprocess_strips_duplicate_feature() {
        let v = GuessedVersion {
            version: "1.0.0.0".into(),
            commit: None,
            pretty_version: Some("1.0.0".into()),
            feature_version: Some("1.0.0.0".into()),
            feature_pretty_version: Some("1.0.0".into()),
        };
        let p = postprocess(v);
        assert_eq!(p.feature_version, None);
        assert_eq!(p.feature_pretty_version, None);
    }

    #[test]
    fn test_postprocess_nine_seven_to_x() {
        let v = GuessedVersion {
            version: "1.9999999.9999999.9999999-dev".into(),
            commit: None,
            pretty_version: Some("dev-1.x".into()),
            feature_version: None,
            feature_pretty_version: None,
        };
        let p = postprocess(v);
        assert_eq!(p.pretty_version.as_deref(), Some("1.x-dev"));
    }

    #[test]
    fn test_is_feature_branch_known_mainlines() {
        let cfg = json!({});
        assert!(!is_feature_branch_name(&cfg, "master"));
        assert!(!is_feature_branch_name(&cfg, "main"));
        assert!(!is_feature_branch_name(&cfg, "develop"));
        assert!(!is_feature_branch_name(&cfg, "1.0"));
        assert!(is_feature_branch_name(&cfg, "feature/x"));
    }

    #[test]
    fn test_is_feature_branch_with_non_feature_list() {
        let cfg = json!({"non-feature-branches": ["staging", "release-.+"]});
        assert!(!is_feature_branch_name(&cfg, "staging"));
        assert!(!is_feature_branch_name(&cfg, "release-2"));
        assert!(is_feature_branch_name(&cfg, "wip-x"));
    }

    #[test]
    fn test_natural_cmp_orders_naturally() {
        assert_eq!(natural_cmp("1.10", "1.9"), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp("1.2", "1.10"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("abc", "abc"), std::cmp::Ordering::Equal);
    }
}
