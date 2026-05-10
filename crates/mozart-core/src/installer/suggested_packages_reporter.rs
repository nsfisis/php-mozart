//! Port of `Composer\Installer\SuggestedPackagesReporter`.
//!
//! Collects suggestions from packages and renders them grouped by package,
//! by suggestion, or as a flat list. Mirrors the bitfield-mode API that
//! Composer's reporter exposes so other entry points (install/update) can
//! emit a minimalistic post-install hint with the same code path.

use crate::console::{IoInterface, Verbosity};
use crate::console_format;
use crate::installer::installed_repo::InstalledRepoLite;
use indexmap::IndexSet;
use std::collections::BTreeMap;

pub const MODE_LIST: u32 = 1;
pub const MODE_BY_PACKAGE: u32 = 2;
pub const MODE_BY_SUGGESTION: u32 = 4;

/// One suggestion record. Mirrors `array{source, target, reason}` in PHP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub source: String,
    pub target: String,
    pub reason: String,
}

/// Anything that can yield a (pretty name, suggest map) for the reporter.
///
/// Mirrors Composer's `PackageInterface::getPrettyName()` + `getSuggests()`.
/// Implemented for `RawPackageData` here, and for the registry crate's
/// `LockedPackage` / `InstalledPackageEntry` next to those types.
pub trait HasSuggests {
    fn pretty_name(&self) -> &str;
    /// Iterator yielding `(target, reason)` pairs.
    fn suggests(&self) -> Vec<(String, String)>;
}

impl HasSuggests for crate::package::RawPackageData {
    fn pretty_name(&self) -> &str {
        &self.name
    }

    fn suggests(&self) -> Vec<(String, String)> {
        let Some(val) = self.extra_fields.get("suggest") else {
            return Vec::new();
        };
        let Some(obj) = val.as_object() else {
            return Vec::new();
        };
        obj.iter()
            .filter_map(|(target, reason)| reason.as_str().map(|r| (target.clone(), r.to_string())))
            .collect()
    }
}

/// Stand-in for Composer's `$onlyDependentsOf` package.
///
/// Holds the root package's name plus its direct require / require-dev
/// targets. The reporter uses this to filter to only direct dependents'
/// suggestions when no explicit packages or `--all` flag was given.
#[derive(Debug, Clone, Default)]
pub struct RootInfo {
    pub name: String,
    pub direct_deps: IndexSet<String>,
}

impl RootInfo {
    /// Lower-cased filter set: root name + all direct deps.
    fn source_filter(&self) -> IndexSet<String> {
        let mut set = self.direct_deps.clone();
        if !self.name.is_empty() {
            set.insert(self.name.to_lowercase());
        }
        set
    }
}

/// Collects and renders package suggestions.
///
/// Construct with [`SuggestedPackagesReporter::new`], feed packages via
/// [`Self::add_suggestions_from_package`] or [`Self::add_package`], then
/// render with [`Self::output`] (or [`Self::output_minimalistic`] for the
/// install/update one-liner).
pub struct SuggestedPackagesReporter<'a> {
    suggested_packages: Vec<Suggestion>,
    io: &'a dyn IoInterface,
}

impl<'a> SuggestedPackagesReporter<'a> {
    pub fn new(io: &'a dyn IoInterface) -> Self {
        Self {
            suggested_packages: Vec::new(),
            io,
        }
    }

    pub fn packages(&self) -> &[Suggestion] {
        &self.suggested_packages
    }

    pub fn add_package(&mut self, source: String, target: String, reason: String) -> &mut Self {
        self.suggested_packages.push(Suggestion {
            source,
            target,
            reason,
        });
        self
    }

    pub fn add_suggestions_from_package<P: HasSuggests + ?Sized>(
        &mut self,
        package: &P,
    ) -> &mut Self {
        let source = package.pretty_name().to_string();
        for (target, reason) in package.suggests() {
            self.add_package(source.clone(), target, reason);
        }
        self
    }

    /// Render the collected suggestions according to `mode`.
    ///
    /// `installed_repo` — when set, suggestions whose target is already
    /// installed are suppressed.
    /// `only_dependents_of` — when set, only suggestions whose source is the
    /// root package itself or one of its direct require/require-dev targets
    /// are shown; an "additional suggestions can be shown with --all" hint
    /// is emitted at the end if any were filtered out.
    pub fn output(
        &self,
        mode: u32,
        installed_repo: Option<&InstalledRepoLite>,
        only_dependents_of: Option<&RootInfo>,
    ) {
        let suggestions = self.get_filtered_suggestions(installed_repo, only_dependents_of);

        // Build (sorted by source/target) maps, last-reason-wins on duplicates.
        let mut suggesters: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut suggested: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for s in &suggestions {
            suggesters
                .entry(s.source.clone())
                .or_default()
                .insert(s.target.clone(), s.reason.clone());
            suggested
                .entry(s.target.clone())
                .or_default()
                .insert(s.source.clone(), s.reason.clone());
        }

        if mode & MODE_LIST != 0 {
            for name in suggested.keys() {
                self.write_line(&console_format!("<info>{}</info>", name));
            }
            return;
        }

        if mode & MODE_BY_PACKAGE != 0 {
            for (suggester, suggestions) in &suggesters {
                self.write_line(&console_format!(
                    "<comment>{}</comment> suggests:",
                    suggester
                ));
                for (suggestion, reason) in suggestions {
                    self.write_suggestion_info(suggestion, reason);
                }
                self.write_line("");
            }
        }

        if mode & MODE_BY_SUGGESTION != 0 {
            if mode & MODE_BY_PACKAGE != 0 {
                self.write_line(&"-".repeat(78));
            }
            for (suggestion, suggesters) in &suggested {
                self.write_line(&console_format!(
                    "<comment>{}</comment> is suggested by:",
                    suggestion
                ));
                for (suggester, reason) in suggesters {
                    self.write_suggestion_comment(suggester, reason);
                }
                self.write_line("");
            }
        }

        if only_dependents_of.is_some() {
            let all_suggestions = self.get_filtered_suggestions(installed_repo, None);
            let diff = all_suggestions.len().saturating_sub(suggestions.len());
            if diff > 0 {
                self.write_line(&format!(
                    "{} by transitive dependencies can be shown with {}",
                    console_format!("<info>{} additional suggestions</info>", diff),
                    console_format!("<info>--all</info>"),
                ));
            }
        }
    }

    /// One-line stderr hint emitted by `install` / `update` after the run.
    pub fn output_minimalistic(
        &self,
        installed_repo: Option<&InstalledRepoLite>,
        only_dependents_of: Option<&RootInfo>,
    ) {
        let suggestions = self.get_filtered_suggestions(installed_repo, only_dependents_of);
        if !suggestions.is_empty() {
            self.io.write(
                &console_format!(
                    "<info>{} package suggestions were added by new dependencies, use `composer suggest` to see details.</info>",
                    suggestions.len()
                ),
                Verbosity::Normal,
            );
        }
    }

    fn write_line(&self, msg: &str) {
        if self.io.verbosity() >= Verbosity::Normal {
            println!("{msg}");
        }
    }

    fn write_suggestion_info(&self, target: &str, reason: &str) {
        let reason = Self::escape_output(reason);
        if reason.is_empty() {
            self.write_line(&console_format!(" - <info>{}</info>", target));
        } else {
            self.write_line(&console_format!(" - <info>{}</info>: {}", target, reason));
        }
    }

    fn write_suggestion_comment(&self, source: &str, reason: &str) {
        let reason = Self::escape_output(reason);
        if reason.is_empty() {
            self.write_line(&console_format!(" - <comment>{}</comment>", source));
        } else {
            self.write_line(&console_format!(
                " - <comment>{}</comment>: {}",
                source,
                reason
            ));
        }
    }

    fn get_filtered_suggestions<'b>(
        &'b self,
        installed_repo: Option<&InstalledRepoLite>,
        only_dependents_of: Option<&RootInfo>,
    ) -> Vec<&'b Suggestion> {
        let source_filter = only_dependents_of.map(|r| r.source_filter());

        self.suggested_packages
            .iter()
            .filter(|s| {
                if let Some(repo) = installed_repo
                    && repo.contains(&s.target)
                {
                    return false;
                }
                if let Some(ref filter) = source_filter
                    && !filter.is_empty()
                    && !filter.contains(&s.source.to_lowercase())
                {
                    return false;
                }
                true
            })
            .collect()
    }

    /// Mirrors Composer's `escapeOutput` — strips control characters and
    /// converts newlines to spaces. Mozart's `console_format!` is a
    /// compile-time proc-macro so runtime `<...>` substrings don't get
    /// re-interpreted as tags; the explicit `<` backslash-escape that
    /// Composer adds via `OutputFormatter::escape` is a no-op for us.
    fn escape_output(s: &str) -> String {
        Self::remove_control_characters(s)
    }

    fn remove_control_characters(s: &str) -> String {
        s.replace('\n', " ")
            .chars()
            .filter(|c| !c.is_control())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::Console;

    fn console() -> Console {
        Console::new(0, false, false, true, true)
    }

    fn make_pkg(name: &'static str, suggests: &[(&'static str, &'static str)]) -> StubPkg {
        StubPkg {
            name,
            suggests: suggests
                .iter()
                .map(|(t, r)| (t.to_string(), r.to_string()))
                .collect(),
        }
    }

    struct StubPkg {
        name: &'static str,
        suggests: Vec<(String, String)>,
    }

    impl HasSuggests for StubPkg {
        fn pretty_name(&self) -> &str {
            self.name
        }
        fn suggests(&self) -> Vec<(String, String)> {
            self.suggests.clone()
        }
    }

    #[test]
    fn add_package_appends_record() {
        let console = console();
        let mut reporter = SuggestedPackagesReporter::new(&console);
        reporter.add_package("a/a".into(), "ext-intl".into(), "for i18n".into());
        assert_eq!(reporter.packages().len(), 1);
        assert_eq!(reporter.packages()[0].source, "a/a");
        assert_eq!(reporter.packages()[0].target, "ext-intl");
        assert_eq!(reporter.packages()[0].reason, "for i18n");
    }

    #[test]
    fn add_suggestions_from_package_uses_pretty_name() {
        let console = console();
        let mut reporter = SuggestedPackagesReporter::new(&console);
        let pkg = make_pkg(
            "Vendor/Pkg",
            &[("ext-intl", "for i18n"), ("ext-redis", "for cache")],
        );
        reporter.add_suggestions_from_package(&pkg);
        assert_eq!(reporter.packages().len(), 2);
        assert!(reporter.packages().iter().all(|s| s.source == "Vendor/Pkg"));
    }

    #[test]
    fn filter_skips_already_installed_targets() {
        let console = console();
        let mut reporter = SuggestedPackagesReporter::new(&console);
        reporter.add_package("a/a".into(), "ext-intl".into(), "r1".into());
        reporter.add_package("a/a".into(), "ext-redis".into(), "r2".into());

        let mut installed = InstalledRepoLite::new();
        installed.insert("ext-intl");

        let filtered = reporter.get_filtered_suggestions(Some(&installed), None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].target, "ext-redis");
    }

    #[test]
    fn filter_only_dependents_of() {
        let console = console();
        let mut reporter = SuggestedPackagesReporter::new(&console);
        reporter.add_package("vendor/direct".into(), "ext-x".into(), "".into());
        reporter.add_package("vendor/transitive".into(), "ext-y".into(), "".into());

        let root = RootInfo {
            name: "my/root".into(),
            direct_deps: ["vendor/direct".to_string()].into_iter().collect(),
        };

        let filtered = reporter.get_filtered_suggestions(None, Some(&root));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "vendor/direct");
    }

    #[test]
    fn filter_only_dependents_of_includes_root_itself() {
        let console = console();
        let mut reporter = SuggestedPackagesReporter::new(&console);
        reporter.add_package("my/root".into(), "ext-x".into(), "".into());
        reporter.add_package("vendor/transitive".into(), "ext-y".into(), "".into());

        let root = RootInfo {
            name: "my/root".into(),
            direct_deps: IndexSet::new(),
        };

        let filtered = reporter.get_filtered_suggestions(None, Some(&root));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "my/root");
    }

    #[test]
    fn remove_control_characters_strips_cntrl_and_newline() {
        let s = SuggestedPackagesReporter::remove_control_characters("foo\nbar\x07baz");
        assert_eq!(s, "foo bar".to_string() + "baz");
    }

    #[test]
    fn mode_constants_match_composer() {
        assert_eq!(MODE_LIST, 1);
        assert_eq!(MODE_BY_PACKAGE, 2);
        assert_eq!(MODE_BY_SUGGESTION, 4);
    }
}
