use std::path::Path;

use anyhow::Result;

use crate::process::{ProcessExecutor, ProcessOutput};

/// SVN credentials for authenticated operations.
#[derive(Debug, Clone)]
pub struct SvnCredentials {
    pub username: String,
    pub password: String,
}

/// SVN utility for command execution with credential handling.
pub struct SvnUtil {
    process: ProcessExecutor,
}

impl SvnUtil {
    pub fn new(process: ProcessExecutor) -> Self {
        Self { process }
    }

    /// Execute an SVN command with `--non-interactive`.
    pub fn execute(&self, args: &[&str], cwd: Option<&Path>) -> Result<ProcessOutput> {
        let mut full_args = vec!["svn"];
        full_args.extend_from_slice(args);
        full_args.push("--non-interactive");
        self.process.execute_checked(&full_args, cwd)
    }

    /// Execute an SVN command with optional credentials, retrying on auth failure.
    pub fn execute_with_credentials(
        &self,
        args: &[&str],
        creds: Option<&SvnCredentials>,
        cwd: Option<&Path>,
    ) -> Result<ProcessOutput> {
        let mut full_args = vec!["svn"];
        full_args.extend_from_slice(args);
        full_args.push("--non-interactive");

        let cred_args: Vec<String>;
        if let Some(c) = creds {
            cred_args = vec![
                "--username".to_string(),
                c.username.clone(),
                "--password".to_string(),
                c.password.clone(),
            ];
            for arg in &cred_args {
                full_args.push(arg);
            }
        }

        let full_args_refs: Vec<&str> = full_args.iter().map(|s| &**s).collect();

        // Retry up to 5 times on auth failure
        let max_retries = 5;
        let mut last_output = None;
        for _ in 0..max_retries {
            let output = self.process.execute(&full_args_refs, cwd)?;
            if output.status == 0 {
                return Ok(output);
            }
            // Check if it's an auth error (SVN exit code or stderr hint)
            if !output.stderr.contains("authorization failed")
                && !output.stderr.contains("Could not authenticate")
                && !output.stderr.contains("Authentication failed")
            {
                // Not an auth error, return immediately
                last_output = Some(output);
                break;
            }
            last_output = Some(output);
        }

        match last_output {
            Some(output) if output.status != 0 => {
                anyhow::bail!(
                    "SVN command `{}` failed with exit code {}\nstderr: {}",
                    full_args_refs.join(" "),
                    output.status,
                    output.stderr.trim(),
                );
            }
            Some(output) => Ok(output),
            None => anyhow::bail!("SVN command failed with no output"),
        }
    }
}
