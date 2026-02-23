use std::path::Path;

use anyhow::Result;

use crate::process::{ProcessExecutor, ProcessOutput};

/// Mercurial utility for command execution.
pub struct HgUtil {
    process: ProcessExecutor,
}

impl HgUtil {
    pub fn new(process: ProcessExecutor) -> Self {
        Self { process }
    }

    /// Execute a Mercurial command.
    pub fn execute(&self, args: &[&str], cwd: Option<&Path>) -> Result<ProcessOutput> {
        let mut full_args = vec!["hg"];
        full_args.extend_from_slice(args);
        self.process.execute_checked(&full_args, cwd)
    }

    /// Execute a Mercurial command, not erroring on non-zero exit.
    pub fn execute_unchecked(&self, args: &[&str], cwd: Option<&Path>) -> Result<ProcessOutput> {
        let mut full_args = vec!["hg"];
        full_args.extend_from_slice(args);
        self.process.execute(&full_args, cwd)
    }
}
