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

use super::{
    ExecuteContext, InstallerExecutor, PackageOperation, format_full_pretty_alias,
    format_full_pretty_version,
};
use mozart_semver::Version;

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
                let alias_full =
                    format_full_pretty_alias(&alias.alias, &alias.alias_normalized, target);
                let target_full = format_full_pretty_version(target);
                self.trace.push(format!(
                    "Marking {} ({}) as installed, alias of {} ({})",
                    alias.package, alias_full, alias.package, target_full
                ));
            }
            PackageOperation::MarkAliasUninstalled {
                name,
                alias_full,
                target_full,
            } => {
                self.trace.push(format!(
                    "Marking {} ({}) as uninstalled, alias of {} ({})",
                    name, alias_full, name, target_full
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

/// Mirrors `Composer\Package\Version\VersionParser::isUpgrade`. Returns true
/// when `to` should be treated as an upgrade from `from` for the purpose of
/// the trace verb (`Upgrading` vs `Downgrading`).
///
/// The rules:
///   1. Same string → upgrade.
///   2. `dev-master` / `dev-trunk` / `dev-default` substitute to the
///      `9999999-dev` default-branch alias before further checks (they are
///      not literal dev-* names; they are the conventional "latest" branch).
///   3. After that substitution, if either side starts with `dev-` (i.e. is
///      a dev branch other than the defaults) → upgrade. Composer treats
///      hopping between dev branches as a forward move regardless of order.
///   4. Otherwise sort numerically and check the original `from` ended up
///      first (= the smaller value).
fn is_upgrade(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    let original_from = from;
    let normalize_default = |s: &str| -> String {
        if matches!(s, "dev-master" | "dev-trunk" | "dev-default") {
            "9999999-dev".to_string()
        } else {
            s.to_string()
        }
    };
    let from_norm = normalize_default(from);
    let to_norm = normalize_default(to);
    if from_norm.starts_with("dev-") || to_norm.starts_with("dev-") {
        return true;
    }
    match (Version::parse(&from_norm), Version::parse(&to_norm)) {
        (Ok(a), Ok(b)) => b >= a,
        _ => {
            // Mirror Composer's fall-through: with two unparseable strings
            // there is nothing to compare, treat the move as an upgrade.
            let _ = original_from;
            true
        }
    }
}
