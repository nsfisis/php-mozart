use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// Output from a process execution.
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Wrapper around `std::process::Command` for executing external programs.
///
/// Corresponds to Composer's `ProcessExecutor`.
pub struct ProcessExecutor {
    timeout: Option<Duration>,
    env_overrides: HashMap<String, Option<String>>,
}

impl Default for ProcessExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessExecutor {
    pub fn new() -> Self {
        Self {
            timeout: None,
            env_overrides: HashMap::new(),
        }
    }

    pub fn with_timeout(secs: u64) -> Self {
        Self {
            timeout: Some(Duration::from_secs(secs)),
            env_overrides: HashMap::new(),
        }
    }

    /// Set an environment variable override for all subsequent executions.
    pub fn set_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.env_overrides.insert(key.into(), Some(value.into()));
    }

    /// Remove an environment variable for all subsequent executions.
    pub fn remove_env(&mut self, key: impl Into<String>) {
        self.env_overrides.insert(key.into(), None);
    }

    /// Execute a command. Does not error on non-zero exit status.
    pub fn execute(&self, args: &[&str], cwd: Option<&Path>) -> Result<ProcessOutput> {
        if args.is_empty() {
            bail!("No command specified");
        }

        let mut cmd = Command::new(args[0]);
        if args.len() > 1 {
            cmd.args(&args[1..]);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        for (key, value) in &self.env_overrides {
            match value {
                Some(v) => {
                    cmd.env(key, v);
                }
                None => {
                    cmd.env_remove(key);
                }
            }
        }

        if let Some(timeout) = self.timeout {
            let mut child = cmd
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            let start = Instant::now();
            loop {
                match child.try_wait()? {
                    Some(status) => {
                        let mut stdout = String::new();
                        let mut stderr = String::new();
                        if let Some(ref mut out) = child.stdout {
                            std::io::Read::read_to_string(out, &mut stdout)?;
                        }
                        if let Some(ref mut err) = child.stderr {
                            std::io::Read::read_to_string(err, &mut stderr)?;
                        }
                        return Ok(ProcessOutput {
                            status: status.code().unwrap_or(-1),
                            stdout,
                            stderr,
                        });
                    }
                    None => {
                        if start.elapsed() > timeout {
                            let _ = child.kill();
                            bail!("Process timed out after {} seconds", timeout.as_secs());
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        } else {
            let output = cmd.output()?;
            Ok(ProcessOutput {
                status: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }

    /// Execute a command, returning an error if the exit status is non-zero.
    pub fn execute_checked(&self, args: &[&str], cwd: Option<&Path>) -> Result<ProcessOutput> {
        let output = self.execute(args, cwd)?;
        if output.status != 0 {
            bail!(
                "Command `{}` failed with exit code {}\nstdout: {}\nstderr: {}",
                args.join(" "),
                output.status,
                output.stdout.trim(),
                output.stderr.trim(),
            );
        }
        Ok(output)
    }

    /// Split output into non-empty lines.
    pub fn split_lines(output: &str) -> Vec<&str> {
        output.lines().filter(|l| !l.is_empty()).collect()
    }
}
