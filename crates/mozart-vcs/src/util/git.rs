use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use sha1::{Digest, Sha1};

use crate::process::{ProcessExecutor, ProcessOutput};

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

    /// Sanitize a URL for use as a directory name.
    pub fn sanitize_url(url: &str) -> String {
        let mut hasher = Sha1::new();
        hasher.update(url.as_bytes());
        let hash = hasher.finalize();
        format!("{:x}", hash)
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
