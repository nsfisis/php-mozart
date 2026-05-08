use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use crate::process::ProcessExecutor;
use crate::util::git::GitUtil;

use super::VcsDownloader;

/// Match `<hex> HEAD` lines in `git show-ref --head -d` output.
static HEAD_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^([a-f0-9]+) HEAD$").unwrap());

/// Git downloader using clone/checkout with optional mirror cache.
///
/// Corresponds to Composer's `Downloader\GitDownloader`.
pub struct GitDownloader {
    git_util: GitUtil,
}

impl GitDownloader {
    pub fn new(git_util: GitUtil) -> Self {
        Self { git_util }
    }
}

impl VcsDownloader for GitDownloader {
    fn download(&self, url: &str, _reference: &str, _target: &Path) -> Result<()> {
        // Pre-sync the mirror so install can use --reference
        self.git_util.sync_mirror(url)?;
        Ok(())
    }

    fn install(&self, url: &str, reference: &str, target: &Path) -> Result<()> {
        let target_str = target.to_string_lossy();
        let mirror_path = self.git_util.mirror_path(url);

        if mirror_path.join("HEAD").exists() {
            // Clone with mirror reference for efficiency
            let mirror_str = mirror_path.to_string_lossy().to_string();
            self.git_util.run_command(
                &[
                    "git",
                    "clone",
                    "--no-checkout",
                    "--dissociate",
                    "--reference",
                    &mirror_str,
                    "--",
                    url,
                    &target_str,
                ],
                url,
                None,
            )?;
        } else {
            self.git_util.run_command(
                &["git", "clone", "--no-checkout", "--", url, &target_str],
                url,
                None,
            )?;
        }

        // Checkout the specific reference
        let process = ProcessExecutor::new();
        process.execute_checked(&["git", "checkout", reference, "--force"], Some(target))?;

        Ok(())
    }

    fn update(&self, url: &str, _old_ref: &str, new_ref: &str, target: &Path) -> Result<()> {
        let process = ProcessExecutor::new();

        // Update remote URL
        process.execute_checked(
            &["git", "remote", "set-url", "origin", "--", url],
            Some(target),
        )?;

        // Fetch latest
        self.git_util
            .run_command(&["git", "fetch", "origin"], url, Some(target))?;

        // Checkout new reference
        process.execute_checked(&["git", "checkout", new_ref, "--force"], Some(target))?;

        Ok(())
    }

    fn remove(&self, target: &Path) -> Result<()> {
        if target.exists() {
            std::fs::remove_dir_all(target)?;
        }
        Ok(())
    }

    fn local_changes(&self, target: &Path) -> Result<Option<String>> {
        if !target.join(".git").exists() {
            return Ok(None);
        }
        let process = ProcessExecutor::new();
        let output = process.execute(
            &["git", "status", "--porcelain", "--untracked-files=no"],
            Some(target),
        )?;
        let trimmed = output.stdout.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    fn vcs_reference(&self, target: &Path) -> Result<Option<String>> {
        if !target.join(".git").exists() {
            return Ok(None);
        }
        let process = ProcessExecutor::new();
        let output = process.execute(&["git", "rev-parse", "HEAD"], Some(target))?;
        if output.status != 0 {
            return Ok(None);
        }
        let trimmed = output.stdout.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    fn unpushed_changes(&self, target: &Path) -> Result<Option<String>> {
        if !target.join(".git").exists() {
            return Ok(None);
        }
        let process = ProcessExecutor::new();

        let mut refs = match collect_show_ref(&process, target)? {
            Some(r) => r,
            None => return Ok(None),
        };

        let head_ref = match HEAD_REF_RE
            .captures(&refs)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
        {
            Some(h) => h,
            None => return Ok(None),
        };

        let candidate_branches = collect_local_branches(&refs, &head_ref);
        if candidate_branches.is_empty() {
            // not on a branch (detached / tag) — skip
            return Ok(None);
        }

        let mut branch = candidate_branches[0].clone();
        let mut unpushed_changes: Option<String> = None;
        let mut branch_not_found_error = false;

        for i in 0..=1 {
            let mut remote_branches: Vec<String> = Vec::new();

            for candidate in &candidate_branches {
                let matches = collect_remote_branches(&refs, candidate);
                if !matches.is_empty() {
                    branch = candidate.clone();
                    remote_branches = matches;
                    break;
                }
            }

            if remote_branches.is_empty() {
                unpushed_changes = Some(format!(
                    "Branch {branch} could not be found on any remote and appears to be unpushed"
                ));
                branch_not_found_error = true;
            } else {
                if branch_not_found_error {
                    unpushed_changes = None;
                }
                for remote_branch in &remote_branches {
                    let range = format!("{remote_branch}...{branch}");
                    let output = process.execute_checked(
                        &["git", "diff", "--name-status", &range, "--"],
                        Some(target),
                    )?;
                    let trimmed = output.stdout.trim().to_string();
                    match unpushed_changes {
                        None => unpushed_changes = Some(trimmed),
                        Some(ref existing) if trimmed.len() < existing.len() => {
                            unpushed_changes = Some(trimmed);
                        }
                        _ => {}
                    }
                }
            }

            if unpushed_changes.as_deref().is_some_and(|s| !s.is_empty()) && i == 0 {
                let _ = process.execute(&["git", "fetch", "--all"], Some(target))?;
                refs = match collect_show_ref(&process, target)? {
                    Some(r) => r,
                    None => return Ok(unpushed_changes),
                };
            }

            if unpushed_changes.as_deref().is_none_or(str::is_empty) {
                break;
            }
        }

        Ok(unpushed_changes.filter(|s| !s.is_empty()))
    }

    fn commit_logs(&self, from: &str, to: &str, target: &Path) -> Result<String> {
        let process = ProcessExecutor::new();
        let range = format!("{from}..{to}");
        let output = process.execute(
            &["git", "log", &range, "--oneline", "--no-decorate"],
            Some(target),
        )?;
        Ok(output.stdout)
    }
}

fn collect_show_ref(process: &ProcessExecutor, target: &Path) -> Result<Option<String>> {
    let output = process.execute(&["git", "show-ref", "--head", "-d"], Some(target))?;
    if output.status != 0 {
        anyhow::bail!(
            "Failed to execute git show-ref --head -d\n\n{}",
            output.stderr.trim()
        );
    }
    Ok(Some(output.stdout.trim().to_string()))
}

fn collect_local_branches(refs: &str, head_ref: &str) -> Vec<String> {
    let pattern = format!(r"(?im)^{} refs/heads/(.+)$", regex::escape(head_ref));
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    re.captures_iter(refs)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

fn collect_remote_branches(refs: &str, candidate: &str) -> Vec<String> {
    let pattern = format!(
        r"(?im)^[a-f0-9]+ refs/remotes/((?:[^/]+)/{})$",
        regex::escape(candidate)
    );
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    re.captures_iter(refs)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}
