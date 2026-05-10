use super::packagist::SecurityAdvisory;
use super::repository::RepositorySet;
use crate::advisory::{AbandonedHandling, AuditFormat};
use crate::console::IoInterface;
use crate::{console_writeln, console_writeln_error};
use indexmap::IndexMap;
use std::collections::BTreeMap;

/// A package being audited, with version and abandonment information.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub version_normalized: Option<String>,
    /// Raw abandoned field from JSON: `true` = abandoned no replacement, `String` = replacement name.
    pub abandoned_raw: Option<serde_json::Value>,
}

impl PackageInfo {
    /// Mirrors `CompletePackage::isAbandoned()`.
    pub fn is_abandoned(&self) -> bool {
        matches!(
            &self.abandoned_raw,
            Some(serde_json::Value::Bool(true)) | Some(serde_json::Value::String(_))
        )
    }

    /// Mirrors `CompletePackage::getReplacementPackage()`.
    pub fn replacement_package(&self) -> Option<&str> {
        match &self.abandoned_raw {
            Some(serde_json::Value::String(s)) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// An advisory paired with the installed version of the package it affects.
#[derive(Debug, Clone)]
pub struct MatchedAdvisory {
    pub advisory: SecurityAdvisory,
    pub installed_version: String,
}

/// A matched advisory that was filtered out by the ignore list.
#[derive(Debug, Clone)]
pub struct IgnoredAdvisory {
    pub advisory: SecurityAdvisory,
    pub installed_version: String,
    pub ignore_reason: Option<String>,
}

/// Result of `Auditor::process_advisories`.
#[derive(Debug, Default)]
pub struct ProcessedAdvisories {
    pub advisories: BTreeMap<String, Vec<MatchedAdvisory>>,
    pub ignored_advisories: BTreeMap<String, Vec<IgnoredAdvisory>>,
}

/// An abandoned package found during audit.
#[derive(Debug, Clone)]
pub struct AbandonedPackage {
    pub name: String,
    pub version: String,
    pub replacement: Option<String>,
}

/// Options passed to `Auditor::audit()`.
pub struct AuditOptions<'a> {
    pub format: AuditFormat,
    pub warning_only: bool,
    pub ignore_list: &'a IndexMap<String, Option<String>>,
    pub abandoned: AbandonedHandling,
    pub ignored_severities: &'a IndexMap<String, Option<String>>,
    pub ignore_unreachable: bool,
    pub ignore_abandoned: &'a IndexMap<String, Option<String>>,
}

/// Mirrors `Composer\Advisory\Auditor`.
pub struct Auditor;

impl Auditor {
    pub fn new() -> Self {
        Self
    }

    /// Main audit entry point. Mirrors `Composer\Advisory\Auditor::audit()`.
    ///
    /// Returns a bitmask: 0=ok, 1=vulnerable, 2=abandoned, 3=both.
    pub async fn audit(
        &self,
        io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        repo_set: &RepositorySet,
        packages: &[PackageInfo],
        options: &AuditOptions<'_>,
    ) -> anyhow::Result<u8> {
        let format = options.format;
        let (all_advisories, unreachable_repos) = repo_set
            .get_matching_security_advisories(
                packages,
                format == AuditFormat::Summary,
                options.ignore_unreachable,
            )
            .await?;

        let ProcessedAdvisories {
            advisories,
            ignored_advisories,
        } = self.process_advisories(
            all_advisories,
            options.ignore_list,
            options.ignored_severities,
        );

        let abandoned_packages = if options.abandoned == AbandonedHandling::Ignore {
            vec![]
        } else {
            self.filter_abandoned_packages(packages, options.ignore_abandoned)
        };

        let abandoned_count = if options.abandoned == AbandonedHandling::Fail {
            abandoned_packages.len()
        } else {
            0
        };

        let affected_packages_count = advisories.len();
        let bitmask = self.calculate_bitmask(affected_packages_count > 0, abandoned_count > 0);

        if format == AuditFormat::Json {
            self.render_json(
                &advisories,
                &ignored_advisories,
                &unreachable_repos,
                &abandoned_packages,
                &io,
            );
            return Ok(bitmask);
        }

        let (ignored_pkg_count, ignored_total) = self.count_ignored(&ignored_advisories);
        let (active_pkg_count, active_total) = self.count_matched(&advisories);

        if active_pkg_count > 0 || ignored_pkg_count > 0 {
            if ignored_pkg_count > 0 {
                let plurality = if ignored_total == 1 { "y" } else { "ies" };
                let pkg_plurality = if ignored_pkg_count == 1 { "" } else { "s" };
                let punctuation = if format == AuditFormat::Summary {
                    "."
                } else {
                    ":"
                };
                let msg = format!(
                    "Found {ignored_total} ignored security vulnerability advisor{plurality} affecting {ignored_pkg_count} package{pkg_plurality}{punctuation}"
                );
                console_writeln_error!(io, "<info>{msg}</info>");
                self.output_advisories_ignored(&io, &ignored_advisories, format);
            }

            if active_pkg_count > 0 {
                let plurality = if active_total == 1 { "y" } else { "ies" };
                let pkg_plurality = if active_pkg_count == 1 { "" } else { "s" };
                let punctuation = if format == AuditFormat::Summary {
                    "."
                } else {
                    ":"
                };
                let msg = format!(
                    "Found {active_total} security vulnerability advisor{plurality} affecting {active_pkg_count} package{pkg_plurality}{punctuation}"
                );
                if options.warning_only {
                    console_writeln_error!(io, "<warning>{msg}</warning>");
                } else {
                    console_writeln_error!(io, "<error>{msg}</error>");
                }
                self.output_advisories(&io, &advisories, format);
            }

            if format == AuditFormat::Summary {
                console_writeln_error!(io, "Run \"mozart audit\" for a full list of advisories.");
            }
        } else {
            console_writeln_error!(
                io,
                "<info>No security vulnerability advisories found.</info>",
            );
        }

        if !unreachable_repos.is_empty() {
            console_writeln_error!(
                io,
                "<warning>The following repositories were unreachable:</warning>",
            );
            for repo in &unreachable_repos {
                console_writeln_error!(io, "  - {repo}");
            }
        }

        if !abandoned_packages.is_empty() && format != AuditFormat::Summary {
            self.output_abandoned_packages(&io, &abandoned_packages, format);
        }

        Ok(bitmask)
    }

    /// Mirrors `Composer\Advisory\Auditor::processAdvisories()`.
    ///
    /// Splits advisories into active and ignored based on the ignore list and ignored severities.
    /// Checks by: package name, advisory ID, severity, CVE, and source remote IDs.
    pub fn process_advisories(
        &self,
        all_advisories: BTreeMap<String, Vec<MatchedAdvisory>>,
        ignore_list: &IndexMap<String, Option<String>>,
        ignored_severities: &IndexMap<String, Option<String>>,
    ) -> ProcessedAdvisories {
        if ignore_list.is_empty() && ignored_severities.is_empty() {
            return ProcessedAdvisories {
                advisories: all_advisories,
                ignored_advisories: BTreeMap::new(),
            };
        }

        let mut advisories: BTreeMap<String, Vec<MatchedAdvisory>> = BTreeMap::new();
        let mut ignored: BTreeMap<String, Vec<IgnoredAdvisory>> = BTreeMap::new();

        for (package, pkg_advisories) in all_advisories {
            for matched in pkg_advisories {
                let adv = &matched.advisory;
                let mut is_active = true;
                let mut ignore_reason: Option<String> = None;

                // Check by package name
                if let Some(reason) = ignore_list.get(&package) {
                    is_active = false;
                    ignore_reason = reason.clone();
                }

                // Check by advisory ID
                if is_active && let Some(reason) = ignore_list.get(&adv.advisory_id) {
                    is_active = false;
                    ignore_reason = reason.clone();
                }

                // Check by severity
                if is_active
                    && let Some(ref sev) = adv.severity
                    && let Some(reason) = ignored_severities.get(sev.as_str())
                {
                    is_active = false;
                    ignore_reason = reason
                        .clone()
                        .or_else(|| Some(format!("{sev} severity is ignored")));
                }

                // Check by CVE
                if is_active
                    && let Some(ref cve) = adv.cve
                    && let Some(reason) = ignore_list.get(cve.as_str())
                {
                    is_active = false;
                    ignore_reason = reason.clone();
                }

                // Check by source remote IDs
                if is_active {
                    for source in &adv.sources {
                        if let Some(reason) = ignore_list.get(&source.remote_id) {
                            is_active = false;
                            ignore_reason = reason.clone();
                            break;
                        }
                    }
                }

                if is_active {
                    advisories.entry(package.clone()).or_default().push(matched);
                } else {
                    ignored
                        .entry(package.clone())
                        .or_default()
                        .push(IgnoredAdvisory {
                            advisory: matched.advisory,
                            installed_version: matched.installed_version,
                            ignore_reason,
                        });
                }
            }
        }

        ProcessedAdvisories {
            advisories,
            ignored_advisories: ignored,
        }
    }

    /// Mirrors `Composer\Advisory\Auditor::filterAbandonedPackages()`.
    pub fn filter_abandoned_packages(
        &self,
        packages: &[PackageInfo],
        ignore_abandoned: &IndexMap<String, Option<String>>,
    ) -> Vec<AbandonedPackage> {
        packages
            .iter()
            .filter(|pkg| {
                if !pkg.is_abandoned() {
                    return false;
                }
                if !ignore_abandoned.is_empty() {
                    let name_lower = pkg.name.to_lowercase();
                    // Case-insensitive exact name match (wildcard support deferred)
                    if ignore_abandoned
                        .keys()
                        .any(|k| k.to_lowercase() == name_lower)
                    {
                        return false;
                    }
                }
                true
            })
            .map(|pkg| AbandonedPackage {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                replacement: pkg.replacement_package().map(|s| s.to_string()),
            })
            .collect()
    }

    /// Mirrors `Composer\Advisory\Auditor::needsCompleteAdvisoryLoad()`.
    ///
    /// Mozart always fetches full advisories (no partial optimization), so this is always false.
    pub fn needs_complete_advisory_load(
        &self,
        advisories: &BTreeMap<String, Vec<MatchedAdvisory>>,
        _ignore_list: &IndexMap<String, Option<String>>,
    ) -> bool {
        let _ = advisories;
        false
    }

    fn calculate_bitmask(&self, has_vulnerable: bool, has_abandoned: bool) -> u8 {
        let mut bitmask = 0u8;
        if has_vulnerable {
            bitmask |= 1;
        }
        if has_abandoned {
            bitmask |= 2;
        }
        bitmask
    }

    fn count_ignored(&self, advisories: &BTreeMap<String, Vec<IgnoredAdvisory>>) -> (usize, usize) {
        let pkg_count = advisories.len();
        let total = advisories.values().map(|v| v.len()).sum();
        (pkg_count, total)
    }

    fn count_matched(&self, advisories: &BTreeMap<String, Vec<MatchedAdvisory>>) -> (usize, usize) {
        let pkg_count = advisories.len();
        let total = advisories.values().map(|v| v.len()).sum();
        (pkg_count, total)
    }

    fn output_advisories(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        advisories: &BTreeMap<String, Vec<MatchedAdvisory>>,
        format: AuditFormat,
    ) {
        match format {
            AuditFormat::Table => self.output_advisories_table(io, advisories),
            AuditFormat::Plain => self.output_advisories_plain(io, advisories),
            AuditFormat::Summary => {}
            AuditFormat::Json => unreachable!(),
        }
    }

    fn output_advisories_ignored(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        advisories: &BTreeMap<String, Vec<IgnoredAdvisory>>,
        format: AuditFormat,
    ) {
        match format {
            AuditFormat::Table => self.output_ignored_advisories_table(io, advisories),
            AuditFormat::Plain => self.output_ignored_advisories_plain(io, advisories),
            AuditFormat::Summary => {}
            AuditFormat::Json => unreachable!(),
        }
    }

    fn output_advisories_table(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        advisories: &BTreeMap<String, Vec<MatchedAdvisory>>,
    ) {
        for pkg_advisories in advisories.values() {
            for matched in pkg_advisories {
                self.render_advisory_table(io, &matched.advisory, &matched.installed_version, None);
            }
        }
    }

    fn output_ignored_advisories_table(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        advisories: &BTreeMap<String, Vec<IgnoredAdvisory>>,
    ) {
        for pkg_advisories in advisories.values() {
            for ignored in pkg_advisories {
                self.render_advisory_table(
                    io,
                    &ignored.advisory,
                    &ignored.installed_version,
                    ignored.ignore_reason.as_deref(),
                );
            }
        }
    }

    fn render_advisory_table(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        adv: &SecurityAdvisory,
        installed_version: &str,
        ignore_reason: Option<&str>,
    ) {
        let label_width = 17usize;
        let mut rows: Vec<(&str, String)> = vec![
            ("Package", adv.package_name.clone()),
            ("Version", installed_version.to_string()),
            ("Severity", adv.severity.clone().unwrap_or_default()),
            ("Advisory ID", adv.advisory_id.clone()),
            (
                "CVE",
                adv.cve.clone().unwrap_or_else(|| "NO CVE".to_string()),
            ),
            ("Title", adv.title.clone()),
            ("URL", adv.link.clone().unwrap_or_default()),
            ("Affected versions", adv.affected_versions.clone()),
            ("Reported at", adv.reported_at.clone()),
        ];
        if let Some(reason) = ignore_reason {
            rows.push(("Ignore reason", reason.to_string()));
        }

        let value_width = rows.iter().map(|(_, v)| v.len()).max().unwrap_or(0).max(20);
        let separator = format!(
            "+-{:-<lw$}-+-{:-<vw$}-+",
            "",
            "",
            lw = label_width,
            vw = value_width
        );

        console_writeln_error!(io, "{}", separator);
        for (label, value) in &rows {
            console_writeln_error!(
                io,
                "| {:<lw$} | {:<vw$} |",
                label,
                value,
                lw = label_width,
                vw = value_width,
            );
        }
        console_writeln_error!(io, "{}", &separator);
        console_writeln_error!(io, "");
    }

    fn output_advisories_plain(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        advisories: &BTreeMap<String, Vec<MatchedAdvisory>>,
    ) {
        let mut first = true;
        for pkg_advisories in advisories.values() {
            for matched in pkg_advisories {
                if !first {
                    console_writeln_error!(io, "--------");
                }
                self.render_advisory_plain(io, &matched.advisory, &matched.installed_version, None);
                first = false;
            }
        }
    }

    fn output_ignored_advisories_plain(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        advisories: &BTreeMap<String, Vec<IgnoredAdvisory>>,
    ) {
        let mut first = true;
        for pkg_advisories in advisories.values() {
            for ignored in pkg_advisories {
                if !first {
                    console_writeln_error!(io, "--------");
                }
                self.render_advisory_plain(
                    io,
                    &ignored.advisory,
                    &ignored.installed_version,
                    ignored.ignore_reason.as_deref(),
                );
                first = false;
            }
        }
    }

    fn render_advisory_plain(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        adv: &SecurityAdvisory,
        installed_version: &str,
        ignore_reason: Option<&str>,
    ) {
        console_writeln_error!(io, "Package: {}", adv.package_name);
        console_writeln_error!(io, "Version: {installed_version}");
        console_writeln_error!(io, "Severity: {}", adv.severity.as_deref().unwrap_or(""),);
        console_writeln_error!(io, "Advisory ID: {}", adv.advisory_id);
        console_writeln_error!(io, "CVE: {}", adv.cve.as_deref().unwrap_or("NO CVE"));
        console_writeln_error!(io, "Title: {}", adv.title);
        console_writeln_error!(io, "URL: {}", adv.link.as_deref().unwrap_or(""));
        console_writeln_error!(io, "Affected versions: {}", adv.affected_versions);
        console_writeln_error!(io, "Reported at: {}", adv.reported_at);
        if let Some(reason) = ignore_reason {
            console_writeln_error!(io, "Ignore reason: {reason}");
        }
    }

    fn output_abandoned_packages(
        &self,
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
        packages: &[AbandonedPackage],
        format: AuditFormat,
    ) {
        let count = packages.len();
        let plurality = if count == 1 { "" } else { "s" };
        console_writeln_error!(
            io,
            "<error>Found {count} abandoned package{plurality}:</error>",
        );

        if format == AuditFormat::Plain {
            for pkg in packages {
                match &pkg.replacement {
                    Some(repl) => console_writeln_error!(
                        io,
                        "{} ({}) is abandoned. Use {} instead.",
                        pkg.name,
                        pkg.version,
                        repl,
                    ),
                    None => console_writeln_error!(
                        io,
                        "{} ({}) is abandoned. No replacement was suggested.",
                        pkg.name,
                        pkg.version,
                    ),
                }
            }
            return;
        }

        // Table format
        let name_width = 20usize;
        let ver_width = packages
            .iter()
            .map(|a| a.version.len())
            .max()
            .unwrap_or(0)
            .max("Version".len());
        let repl_width = packages
            .iter()
            .map(|a| {
                a.replacement
                    .as_deref()
                    .unwrap_or("No replacement suggested")
                    .len()
            })
            .max()
            .unwrap_or(0)
            .max("Suggested Replacement".len());

        console_writeln_error!(
            io,
            "| {:<nw$} | {:<vw$} | {:<rw$} |",
            "Abandoned Package",
            "Version",
            "Suggested Replacement",
            nw = name_width,
            vw = ver_width,
            rw = repl_width,
        );
        console_writeln_error!(
            io,
            "+-{:-<nw$}-+-{:-<vw$}-+-{:-<rw$}-+",
            "",
            "",
            "",
            nw = name_width,
            vw = ver_width,
            rw = repl_width,
        );
        for pkg in packages {
            let replacement = pkg
                .replacement
                .as_deref()
                .unwrap_or("No replacement suggested");
            console_writeln_error!(
                io,
                "| {:<nw$} | {:<vw$} | {:<rw$} |",
                pkg.name,
                pkg.version,
                replacement,
                nw = name_width,
                vw = ver_width,
                rw = repl_width,
            );
        }
        console_writeln_error!(io, "");
    }

    fn render_json(
        &self,
        advisories: &BTreeMap<String, Vec<MatchedAdvisory>>,
        ignored_advisories: &BTreeMap<String, Vec<IgnoredAdvisory>>,
        unreachable_repos: &[String],
        abandoned_packages: &[AbandonedPackage],
        io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    ) {
        let mut advisories_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        for (pkg_name, matched_list) in advisories {
            let arr: Vec<serde_json::Value> = matched_list
                .iter()
                .map(|m| serde_json::to_value(&m.advisory).unwrap_or(serde_json::Value::Null))
                .collect();
            advisories_map.insert(pkg_name.clone(), serde_json::Value::Array(arr));
        }

        let mut output = serde_json::json!({ "advisories": advisories_map });

        // ignored-advisories (only if non-empty)
        if !ignored_advisories.is_empty() {
            let mut ignored_map: serde_json::Map<String, serde_json::Value> =
                serde_json::Map::new();
            for (pkg_name, ignored_list) in ignored_advisories {
                let arr: Vec<serde_json::Value> = ignored_list
                    .iter()
                    .map(|i| {
                        let mut val =
                            serde_json::to_value(&i.advisory).unwrap_or(serde_json::Value::Null);
                        if let serde_json::Value::Object(ref mut obj) = val {
                            obj.insert(
                                "ignoreReason".to_string(),
                                i.ignore_reason
                                    .as_ref()
                                    .map(|r| serde_json::Value::String(r.clone()))
                                    .unwrap_or(serde_json::Value::Null),
                            );
                        }
                        val
                    })
                    .collect();
                ignored_map.insert(pkg_name.clone(), serde_json::Value::Array(arr));
            }
            if let serde_json::Value::Object(ref mut obj) = output {
                obj.insert(
                    "ignored-advisories".to_string(),
                    serde_json::Value::Object(ignored_map),
                );
            }
        }

        // unreachable-repositories (only if non-empty)
        if !unreachable_repos.is_empty() {
            let repos_arr: Vec<serde_json::Value> = unreachable_repos
                .iter()
                .map(|r| serde_json::Value::String(r.clone()))
                .collect();
            if let serde_json::Value::Object(ref mut obj) = output {
                obj.insert(
                    "unreachable-repositories".to_string(),
                    serde_json::Value::Array(repos_arr),
                );
            }
        }

        // abandoned map: package_name => replacement (null if none)
        let mut abandoned_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        for pkg in abandoned_packages {
            abandoned_map.insert(
                pkg.name.clone(),
                pkg.replacement
                    .as_ref()
                    .map(|r| serde_json::Value::String(r.clone()))
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        if let serde_json::Value::Object(ref mut obj) = output {
            obj.insert(
                "abandoned".to_string(),
                serde_json::Value::Object(abandoned_map),
            );
        }

        let json_str = serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string());
        console_writeln!(io, "{}", &json_str);
    }
}

impl Default for Auditor {
    fn default() -> Self {
        Self::new()
    }
}
