use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;

use crate::process::{ProcessExecutor, ProcessOutput};

/// Modern GitHub token pattern (40+ hex chars, `ghp_…`, `github_pat_…`).
///
/// Mirrors `Composer\Util\GitHub::GITHUB_TOKEN_REGEX`.
static GITHUB_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([a-fA-F0-9]{12,}|gh[a-zA-Z]_[a-zA-Z0-9_]+|github_pat_[a-zA-Z0-9_]+)$").unwrap()
});

/// `[?&]access_token=...` query parameter.
static ACCESS_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([&?]access_token=)[^&]+").unwrap());

/// `<scheme>://user:password@` credential block.
static CREDENTIALS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?P<prefix>[a-z0-9]+://)?(?P<user>[^:/\s@]+):(?P<password>[^@\s/]+)@").unwrap()
});

/// Git utility for mirror management and protocol fallback.
///
/// Corresponds to Composer's `Util\Git`.
pub struct GitUtil {
    process: ProcessExecutor,
    cache_dir: PathBuf,
}

impl GitUtil {
    pub fn new(process: ProcessExecutor, cache_dir: PathBuf) -> Self {
        Self { process, cache_dir }
    }

    /// Returns environment variable overrides to clean Git state.
    /// Removes `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` to avoid
    /// interference from the calling process's Git context.
    pub fn clean_env() -> Vec<(&'static str, Option<&'static str>)> {
        vec![
            ("GIT_DIR", None),
            ("GIT_WORK_TREE", None),
            ("GIT_INDEX_FILE", None),
            ("GIT_TERMINAL_PROMPT", Some("0")),
        ]
    }

    /// Synchronize a bare mirror in the cache directory.
    ///
    /// On first call, clones a bare mirror. On subsequent calls, updates it.
    /// Returns the path to the mirror directory.
    pub fn sync_mirror(&self, url: &str) -> Result<PathBuf> {
        let mirror_dir = self.mirror_path(url);

        if mirror_dir.join("HEAD").exists() {
            // Update existing mirror
            self.run_command(
                &["git", "remote", "set-url", "origin", "--", url],
                url,
                Some(&mirror_dir),
            )?;
            self.run_command(
                &["git", "remote", "update", "--prune", "origin"],
                url,
                Some(&mirror_dir),
            )?;
        } else {
            // Create new mirror
            std::fs::create_dir_all(&mirror_dir)?;
            self.run_command(
                &[
                    "git",
                    "clone",
                    "--mirror",
                    "--",
                    url,
                    mirror_dir.to_str().unwrap_or(""),
                ],
                url,
                None,
            )?;
        }

        Ok(mirror_dir)
    }

    /// Fetch a specific refspec from the mirror.
    pub fn fetch_ref(&self, mirror_dir: &Path, refspec: &str) -> Result<bool> {
        let output = self
            .process
            .execute(&["git", "fetch", "origin", refspec], Some(mirror_dir))?;
        Ok(output.status == 0)
    }

    /// Get the default branch of a repository.
    pub fn get_default_branch(&self, mirror_dir: &Path) -> Result<Option<String>> {
        let output = self
            .process
            .execute(&["git", "remote", "show", "origin"], Some(mirror_dir))?;
        if output.status != 0 {
            return Ok(None);
        }
        for line in output.stdout.lines() {
            let trimmed = line.trim();
            if let Some(branch) = trimmed.strip_prefix("HEAD branch:") {
                let branch = branch.trim();
                if branch != "(unknown)" {
                    return Ok(Some(branch.to_string()));
                }
            }
        }
        Ok(None)
    }

    /// Execute a git command with protocol fallback.
    ///
    /// Tries the URL as-is first, then falls back through protocol variations
    /// (ssh → https → git://) if the command fails.
    pub fn run_command(
        &self,
        args: &[&str],
        url: &str,
        cwd: Option<&Path>,
    ) -> Result<ProcessOutput> {
        let mut executor = ProcessExecutor::new();
        for (key, value) in Self::clean_env() {
            match value {
                Some(v) => executor.set_env(key, v),
                None => executor.remove_env(key),
            }
        }

        // Try the command as-is first
        let output = executor.execute(args, cwd)?;
        if output.status == 0 {
            return Ok(output);
        }

        // Try protocol fallback for remote URLs
        let fallback_urls = Self::get_fallback_urls(url);
        for fallback_url in &fallback_urls {
            let new_args: Vec<&str> = args
                .iter()
                .map(|&a| if a == url { fallback_url.as_str() } else { a })
                .collect();
            let fallback_output = executor.execute(&new_args, cwd)?;
            if fallback_output.status == 0 {
                return Ok(fallback_output);
            }
        }

        // Return the original error
        if output.status != 0 {
            bail!(
                "Git command `{}` failed with exit code {}\nstdout: {}\nstderr: {}",
                args.join(" "),
                output.status,
                output.stdout.trim(),
                output.stderr.trim(),
            );
        }
        Ok(output)
    }

    /// Get the Git version string.
    pub fn get_version(&self) -> Option<String> {
        let output = self.process.execute(&["git", "--version"], None).ok()?;
        if output.status != 0 {
            return None;
        }
        // "git version 2.39.2" -> "2.39.2"
        output
            .stdout
            .trim()
            .strip_prefix("git version ")
            .map(|s| s.to_string())
    }

    /// Sanitize a URL for use as a cache directory name.
    ///
    /// Mirrors Composer's `Preg::replace('{[^a-z0-9.]}i', '-', Url::sanitize($url))`
    /// pattern (see `GitDriver::initialize` and `GitDownloader`): credentials and
    /// access tokens are first redacted, then every byte outside `[a-zA-Z0-9.]`
    /// is replaced with `-`. The redaction step keeps cache keys stable across
    /// URLs that differ only in their embedded token.
    pub fn sanitize_url(url: &str) -> String {
        let redacted = sanitize_url_credentials(url);
        redacted
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    }

    /// Get the cache mirror path for a URL.
    pub fn mirror_path(&self, url: &str) -> PathBuf {
        self.cache_dir.join(Self::sanitize_url(url))
    }

    /// Generate fallback URLs for protocol switching.
    fn get_fallback_urls(url: &str) -> Vec<String> {
        let mut urls = Vec::new();

        // ssh -> https fallback
        if url.starts_with("git@") {
            // git@github.com:owner/repo.git -> https://github.com/owner/repo.git
            if let Some(rest) = url.strip_prefix("git@") {
                let converted = rest.replacen(':', "/", 1);
                urls.push(format!("https://{converted}"));
            }
        }

        // git:// -> https:// fallback
        if let Some(rest) = url.strip_prefix("git://") {
            urls.push(format!("https://{rest}"));
        }

        // https -> git:// fallback
        if let Some(rest) = url.strip_prefix("https://") {
            urls.push(format!("git://{rest}"));
        }

        urls
    }
}

/// Redact credentials and access tokens from `url`.
///
/// Mirrors Composer's `Util\Url::sanitize`. Two replacements are applied:
/// 1. `[?&]access_token=…` query values → `***`
/// 2. `<scheme>://user:password@` credentials → `***:***@` if `user` looks like
///    a GitHub token, otherwise just `user:***@`
fn sanitize_url_credentials(url: &str) -> String {
    let url = ACCESS_TOKEN_RE.replace_all(url, "${1}***");
    CREDENTIALS_RE
        .replace_all(&url, |caps: &regex::Captures<'_>| {
            let prefix = caps.name("prefix").map(|m| m.as_str()).unwrap_or("");
            let user = &caps["user"];
            if GITHUB_TOKEN_RE.is_match(user) {
                format!("{prefix}***:***@")
            } else {
                format!("{prefix}{user}:***@")
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_url_replaces_special_chars_with_dash() {
        assert_eq!(
            GitUtil::sanitize_url("https://github.com/owner/repo.git"),
            "https---github.com-owner-repo.git"
        );
    }

    #[test]
    fn sanitize_url_preserves_dot() {
        // Dot must survive — it appears in hostnames and ".git" suffixes.
        let key = GitUtil::sanitize_url("git://example.org/foo.bar/baz.git");
        assert!(key.contains(".org"));
        assert!(key.ends_with(".git"));
    }

    #[test]
    fn sanitize_url_redacts_password_in_credentials() {
        let key = GitUtil::sanitize_url("https://alice:s3cret@example.com/repo.git");
        // Password is replaced with ***, then non-alphanumerics become '-'.
        assert!(key.contains("alice"));
        assert!(!key.contains("s3cret"));
    }

    #[test]
    fn sanitize_url_redacts_user_when_looks_like_github_token() {
        // 40-hex token in the user position triggers full redaction.
        let token = "abcdef0123456789abcdef0123456789abcdef01";
        let key = GitUtil::sanitize_url(&format!("https://{token}:x-oauth-basic@github.com/o/r"));
        assert!(!key.contains("abcdef"));
    }

    #[test]
    fn sanitize_url_redacts_modern_github_pat() {
        // ghp_xxx and github_pat_xxx forms.
        let key1 = GitUtil::sanitize_url("https://ghp_abc123XYZ:x@github.com/o/r");
        assert!(!key1.contains("ghp_"));
        let key2 = GitUtil::sanitize_url("https://github_pat_abc123:x@github.com/o/r");
        assert!(!key2.contains("github_pat_"));
    }

    #[test]
    fn sanitize_url_strips_access_token_query() {
        let key = GitUtil::sanitize_url("https://api.github.com/x?access_token=secrettoken");
        assert!(!key.contains("secrettoken"));
    }

    #[test]
    fn sanitize_url_token_variants_share_cache_key() {
        // Two pulls of the same repo with different access tokens should land
        // in the same cache subdirectory.
        let a = GitUtil::sanitize_url("https://api.github.com/repo?access_token=tokenA");
        let b = GitUtil::sanitize_url("https://api.github.com/repo?access_token=tokenB");
        assert_eq!(a, b);
    }
}
