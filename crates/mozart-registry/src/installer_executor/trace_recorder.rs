//! Recording-only [`InstallerExecutor`] for in-process tests.
//!
//! Mirrors `Composer\Test\Mock\InstallationManagerMock` — every call appends
//! a string to a `Vec<String>` matching Composer's
//! `(string) $operation` output (after `strip_tags`). No filesystem or
//! network I/O happens. The recorded trace is what tests assert against
//! `--EXPECT--` in Composer's `.test` fixture format.
//!
//! Trace line shapes (byte-equivalent to Composer's `*Operation::__toString`
//! after `strip_tags`):
//!
//! - Install: `Installing <name> (<version>)`
//! - Update (upgrade direction): `Upgrading <name> (<oldVersion> => <newVersion>)`
//! - Update (downgrade direction): `Downgrading <name> (<oldVersion> => <newVersion>)`
//! - Uninstall: `Removing <name> (<version>)`

use mozart_semver::Version;

use super::{
    ExecuteContext, InstallerExecutor, PackageOperation, format_full_pretty_version,
    format_full_pretty_with_pretty,
};

/// Recording-only executor. Construct with [`TraceRecorderExecutor::new`],
/// then read [`TraceRecorderExecutor::trace`] after the run completes.
pub struct TraceRecorderExecutor {
    trace: Vec<String>,
}

impl TraceRecorderExecutor {
    pub fn new() -> Self {
        Self { trace: Vec::new() }
    }

    /// Recorded operation strings, in the order [`InstallerExecutor`] was
    /// invoked. Pass this to `assert_eq!` against the fixture's `--EXPECT--`
    /// section after splitting on newlines.
    pub fn trace(&self) -> &[String] {
        &self.trace
    }

    /// Take ownership of the recorded trace. Use after the run if the
    /// executor is going out of scope.
    pub fn into_trace(self) -> Vec<String> {
        self.trace
    }
}

impl Default for TraceRecorderExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl InstallerExecutor for TraceRecorderExecutor {
    async fn install_package(
        &mut self,
        op: PackageOperation<'_>,
        _ctx: &ExecuteContext,
    ) -> anyhow::Result<()> {
        match op {
            PackageOperation::Install { package } => {
                self.trace.push(format!(
                    "Installing {} ({})",
                    package.name,
                    format_full_pretty_version(package)
                ));
            }
            PackageOperation::Update {
                from_version,
                from_full_pretty,
                to_full_pretty,
                package,
            } => {
                let action = if is_upgrade(from_version, &package.version) {
                    "Upgrading"
                } else {
                    "Downgrading"
                };
                self.trace.push(format!(
                    "{} {} ({} => {})",
                    action, package.name, from_full_pretty, to_full_pretty
                ));
            }
            PackageOperation::MarkAliasInstalled { alias, target } => {
                let alias_full = format_full_pretty_with_pretty(&alias.alias, target);
                let target_full = format_full_pretty_version(target);
                self.trace.push(format!(
                    "Marking {} ({}) as installed, alias of {} ({})",
                    alias.package, alias_full, alias.package, target_full
                ));
            }
        }
        Ok(())
    }

    fn uninstall_package(
        &mut self,
        name: &str,
        version: &str,
        _ctx: &ExecuteContext,
    ) -> anyhow::Result<()> {
        self.trace.push(format!("Removing {} ({})", name, version));
        Ok(())
    }
}

/// Mirrors `Composer\Package\Version\VersionParser::isUpgrade` — returns
/// true when `to` is a strictly higher version than `from`. Both unparseable
/// or both equal means treat as upgrade (Composer's behavior on edge cases).
fn is_upgrade(from: &str, to: &str) -> bool {
    match (Version::parse(from), Version::parse(to)) {
        (Ok(a), Ok(b)) => b >= a,
        _ => true,
    }
}
