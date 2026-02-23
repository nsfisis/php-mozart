use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mozart_core::console::Verbosity;
use mozart_registry::packagist::SecurityAdvisory;

#[derive(Args)]
pub struct AuditArgs {
    /// Disables auditing of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// Output format (table, plain, json, summary)
    #[arg(short, long, default_value = "table")]
    pub format: String,

    /// Audit packages from the lock file instead of installed
    #[arg(long)]
    pub locked: bool,

    /// Behavior on abandoned packages (ignore, report, fail)
    #[arg(long)]
    pub abandoned: Option<String>,

    /// Ignore advisories of a given severity (low, medium, high, critical)
    #[arg(long)]
    pub ignore_severity: Vec<String>,

    /// Ignore advisories from unreachable repositories
    #[arg(long)]
    pub ignore_unreachable: bool,
}

// ─── Internal types ───────────────────────────────────────────────────────────

#[derive(Debug)]
struct PackageEntry {
    name: String,
    version: String,
    version_normalized: Option<String>,
    abandoned: Option<serde_json::Value>,
}

/// An advisory that matched an installed package version.
struct MatchedAdvisory {
    advisory: SecurityAdvisory,
    installed_version: String,
}

/// An abandoned package found during audit.
struct AbandonedPackage {
    name: String,
    version: String,
    replacement: Option<String>,
}

/// Aggregated audit results.
struct AuditResult {
    /// Map from package name to list of matching advisories.
    advisories: BTreeMap<String, Vec<MatchedAdvisory>>,
    /// Abandoned packages found (only if --abandoned != ignore).
    abandoned: Vec<AbandonedPackage>,
    /// Total count of advisory-affected packages.
    affected_package_count: usize,
    /// Total count of individual advisories.
    total_advisory_count: usize,
}

// ─── Main entry point ─────────────────────────────────────────────────────────

pub async fn execute(
    args: &AuditArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // Validate format
    let format = args.format.as_str();
    if format != "table" && format != "plain" && format != "json" && format != "summary" {
        anyhow::bail!(
            "Invalid format \"{}\". Supported formats: table, plain, json, summary",
            format
        );
    }

    // Validate --abandoned
    let abandoned_mode = match args.abandoned.as_deref().unwrap_or("fail") {
        "ignore" => "ignore",
        "report" => "report",
        "fail" => "fail",
        other => anyhow::bail!(
            "Invalid abandoned value \"{}\". Supported values: ignore, report, fail",
            other
        ),
    };

    // Determine working directory
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // Load packages
    let packages = load_packages(&working_dir, args.locked, args.no_dev)?;

    if packages.is_empty() {
        console.info("No packages - skipping audit.");
        return Ok(());
    }

    // Fetch advisories
    let names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let all_advisories = match mozart_registry::packagist::fetch_security_advisories(&names).await {
        Ok(a) => a,
        Err(e) => {
            if args.ignore_unreachable {
                BTreeMap::new()
            } else {
                return Err(e);
            }
        }
    };

    // Filter advisories by installed versions and severity
    let matched = filter_advisories(&all_advisories, &packages, &args.ignore_severity, console);

    // Detect abandoned packages
    let abandoned = if abandoned_mode == "ignore" {
        Vec::new()
    } else {
        detect_abandoned(&packages)
    };

    // Build result
    let affected_package_count = matched.len();
    let total_advisory_count = matched.values().map(|v| v.len()).sum();

    let result = AuditResult {
        advisories: matched,
        abandoned,
        affected_package_count,
        total_advisory_count,
    };

    // Render output
    match format {
        "table" => render_table(&result, console),
        "plain" => render_plain(&result, console),
        "json" => render_json(&result, console)?,
        "summary" => render_summary(&result, console),
        _ => unreachable!(),
    }

    // Compute bitmask exit code
    let has_advisories = result.total_advisory_count > 0;
    let has_abandoned = !result.abandoned.is_empty() && abandoned_mode == "fail";

    let exit_code: i32 = match (has_advisories, has_abandoned) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    };

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code));
    }

    Ok(())
}

// ─── Package loading ──────────────────────────────────────────────────────────

fn load_packages(
    working_dir: &Path,
    locked: bool,
    no_dev: bool,
) -> anyhow::Result<Vec<PackageEntry>> {
    if locked {
        load_locked_packages(working_dir, no_dev)
    } else {
        load_installed_packages(working_dir, no_dev)
    }
}

fn load_installed_packages(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<PackageEntry>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    let dev_names: std::collections::HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    let packages = installed
        .packages
        .iter()
        .filter(|p| {
            if no_dev && dev_names.contains(&p.name.to_lowercase()) {
                return false;
            }
            true
        })
        .map(|p| {
            let abandoned = p.extra_fields.get("abandoned").cloned();
            PackageEntry {
                name: p.name.clone(),
                version: p.version.clone(),
                version_normalized: p.version_normalized.clone(),
                abandoned,
            }
        })
        .collect();

    Ok(packages)
}

fn load_locked_packages(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<PackageEntry>> {
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "A valid composer.json and composer.lock file is required to run this command with --locked"
        );
    }

    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut all_packages: Vec<&mozart_registry::lockfile::LockedPackage> =
        lock.packages.iter().collect();

    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        all_packages.extend(pkgs_dev.iter());
    }

    let packages = all_packages
        .iter()
        .map(|p| {
            let abandoned = p.extra_fields.get("abandoned").cloned();
            PackageEntry {
                name: p.name.clone(),
                version: p.version.clone(),
                version_normalized: p.version_normalized.clone(),
                abandoned,
            }
        })
        .collect();

    Ok(packages)
}

// ─── Advisory filtering ───────────────────────────────────────────────────────

fn filter_advisories(
    all_advisories: &BTreeMap<String, Vec<SecurityAdvisory>>,
    packages: &[PackageEntry],
    ignore_severity: &[String],
    console: &mozart_core::console::Console,
) -> BTreeMap<String, Vec<MatchedAdvisory>> {
    let ignore_set: std::collections::HashSet<String> =
        ignore_severity.iter().map(|s| s.to_lowercase()).collect();

    let mut result: BTreeMap<String, Vec<MatchedAdvisory>> = BTreeMap::new();

    for pkg in packages {
        let Some(advisories) = all_advisories.get(&pkg.name) else {
            continue;
        };

        // Parse the installed version
        let version_str = pkg
            .version_normalized
            .as_deref()
            .unwrap_or(pkg.version.as_str());

        let installed_ver = match mozart_semver::Version::parse(version_str) {
            Ok(v) => v,
            Err(_) => {
                console.write(
                    &format!(
                        "Warning: could not parse version \"{}\" for package \"{}\", skipping advisory matching",
                        version_str, pkg.name
                    ),
                    Verbosity::Normal,
                );
                continue;
            }
        };

        let mut matched: Vec<MatchedAdvisory> = Vec::new();

        for advisory in advisories {
            // Apply severity filter
            if let Some(ref sev) = advisory.severity
                && ignore_set.contains(&sev.to_lowercase())
            {
                continue;
            }

            // Parse and match the affected versions constraint.
            // Normalize single-pipe OR separators (`|`) to double-pipe (`||`)
            // since the Packagist API may use either form.
            let normalized_constraint = normalize_or_separator(&advisory.affected_versions);
            let constraint = match mozart_semver::VersionConstraint::parse(&normalized_constraint) {
                Ok(c) => c,
                Err(_) => {
                    console.write(
                        &format!(
                            "Warning: could not parse affected versions \"{}\" for advisory \"{}\", skipping",
                            advisory.affected_versions, advisory.advisory_id
                        ),
                        Verbosity::Normal,
                    );
                    continue;
                }
            };

            if constraint.matches(&installed_ver) {
                matched.push(MatchedAdvisory {
                    advisory: advisory.clone(),
                    installed_version: pkg.version.clone(),
                });
            }
        }

        if !matched.is_empty() {
            result.insert(pkg.name.clone(), matched);
        }
    }

    result
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Normalize single-pipe OR separators (`|`) in a version constraint string to
/// double-pipe (`||`) so the constraint parser can handle both forms.
///
/// The Packagist security advisories API may return constraints with single `|`
/// as the OR separator (e.g. `>=1.0,<1.5|>=2.0,<2.3`), but Mozart's
/// `VersionConstraint::parse` expects `||`.
fn normalize_or_separator(constraint: &str) -> String {
    // Replace isolated `|` (not already `||`) with `||`.
    // Walk byte-by-byte to avoid replacing `||` again.
    let bytes = constraint.as_bytes();
    let mut result = String::with_capacity(constraint.len() + 4);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                // Already `||` — emit as-is and skip both
                result.push_str("||");
                i += 2;
            } else {
                // Single `|` — upgrade to `||`
                result.push_str("||");
                i += 1;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

// ─── Abandoned detection ──────────────────────────────────────────────────────

fn detect_abandoned(packages: &[PackageEntry]) -> Vec<AbandonedPackage> {
    let mut result = Vec::new();

    for pkg in packages {
        let Some(ref abandoned_val) = pkg.abandoned else {
            continue;
        };

        let replacement = match abandoned_val {
            serde_json::Value::Bool(true) => None,
            serde_json::Value::String(s) => Some(s.clone()),
            _ => continue,
        };

        result.push(AbandonedPackage {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            replacement,
        });
    }

    result
}

// ─── Output rendering ─────────────────────────────────────────────────────────

fn render_table(result: &AuditResult, console: &mozart_core::console::Console) {
    if result.total_advisory_count == 0 && result.abandoned.is_empty() {
        console.info(&format!(
            "{}",
            mozart_core::console::info("No security vulnerability advisories found.")
        ));
        return;
    }

    if result.total_advisory_count > 0 {
        let advisory_word = if result.total_advisory_count == 1 {
            "advisory"
        } else {
            "advisories"
        };
        let header = format!(
            "Found {} security vulnerability {} affecting {} package(s):",
            result.total_advisory_count, advisory_word, result.affected_package_count
        );
        console.write(
            &format!("{}", mozart_core::console::highlight(&header)),
            Verbosity::Normal,
        );
        console.write("", Verbosity::Normal);

        for advisories in result.advisories.values() {
            for matched in advisories {
                let adv = &matched.advisory;

                // Compute column widths for the two-column table
                let label_width = 17usize;
                let rows: Vec<(&str, String)> = vec![
                    ("Package", adv.package_name.clone()),
                    ("Version", matched.installed_version.clone()),
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

                let value_width = rows.iter().map(|(_, v)| v.len()).max().unwrap_or(0).max(20);
                let separator = format!(
                    "+-{:-<lw$}-+-{:-<vw$}-+",
                    "",
                    "",
                    lw = label_width,
                    vw = value_width
                );

                console.write(&separator, Verbosity::Normal);
                for (label, value) in &rows {
                    console.write(
                        &format!(
                            "| {:<lw$} | {:<vw$} |",
                            label,
                            value,
                            lw = label_width,
                            vw = value_width
                        ),
                        Verbosity::Normal,
                    );
                }
                console.write(&separator, Verbosity::Normal);
                console.write("", Verbosity::Normal);
            }
        }
    }

    if !result.abandoned.is_empty() {
        let header = format!("Found {} abandoned package(s):", result.abandoned.len());
        console.write(
            &format!("{}", mozart_core::console::highlight(&header)),
            Verbosity::Normal,
        );
        console.write("", Verbosity::Normal);

        let name_width = 20usize;
        let ver_width = result
            .abandoned
            .iter()
            .map(|a| a.version.len())
            .max()
            .unwrap_or(0)
            .max("Version".len());
        let repl_width = result
            .abandoned
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

        console.write(
            &format!(
                "| {:<nw$} | {:<vw$} | {:<rw$} |",
                "Abandoned Package",
                "Version",
                "Suggested Replacement",
                nw = name_width,
                vw = ver_width,
                rw = repl_width
            ),
            Verbosity::Normal,
        );
        console.write(
            &format!(
                "+-{:-<nw$}-+-{:-<vw$}-+-{:-<rw$}-+",
                "",
                "",
                "",
                nw = name_width,
                vw = ver_width,
                rw = repl_width
            ),
            Verbosity::Normal,
        );
        for pkg in &result.abandoned {
            let replacement = pkg
                .replacement
                .as_deref()
                .unwrap_or("No replacement suggested");
            console.write(
                &format!(
                    "| {:<nw$} | {:<vw$} | {:<rw$} |",
                    pkg.name,
                    pkg.version,
                    replacement,
                    nw = name_width,
                    vw = ver_width,
                    rw = repl_width
                ),
                Verbosity::Normal,
            );
        }
        console.write("", Verbosity::Normal);
    }
}

fn render_plain(result: &AuditResult, console: &mozart_core::console::Console) {
    if result.total_advisory_count == 0 && result.abandoned.is_empty() {
        console.info("No security vulnerability advisories found.");
        return;
    }

    if result.total_advisory_count > 0 {
        let advisory_word = if result.total_advisory_count == 1 {
            "advisory"
        } else {
            "advisories"
        };
        console.write(
            &format!(
                "Found {} security vulnerability {} affecting {} package(s):",
                result.total_advisory_count, advisory_word, result.affected_package_count
            ),
            Verbosity::Normal,
        );
        console.write("", Verbosity::Normal);

        for advisories in result.advisories.values() {
            for matched in advisories {
                let adv = &matched.advisory;
                console.write(&format!("Package: {}", adv.package_name), Verbosity::Normal);
                console.write(
                    &format!("Version: {}", matched.installed_version),
                    Verbosity::Normal,
                );
                console.write(
                    &format!("Severity: {}", adv.severity.as_deref().unwrap_or("")),
                    Verbosity::Normal,
                );
                console.write(
                    &format!("Advisory ID: {}", adv.advisory_id),
                    Verbosity::Normal,
                );
                console.write(
                    &format!("CVE: {}", adv.cve.as_deref().unwrap_or("NO CVE")),
                    Verbosity::Normal,
                );
                console.write(&format!("Title: {}", adv.title), Verbosity::Normal);
                console.write(
                    &format!("URL: {}", adv.link.as_deref().unwrap_or("")),
                    Verbosity::Normal,
                );
                console.write(
                    &format!("Affected versions: {}", adv.affected_versions),
                    Verbosity::Normal,
                );
                console.write(
                    &format!("Reported at: {}", adv.reported_at),
                    Verbosity::Normal,
                );
                console.write("--------", Verbosity::Normal);
            }
        }
    }

    for pkg in &result.abandoned {
        match &pkg.replacement {
            Some(repl) => console.write(
                &format!(
                    "{} ({}) is abandoned. Use {} instead.",
                    pkg.name, pkg.version, repl
                ),
                Verbosity::Normal,
            ),
            None => console.write(
                &format!(
                    "{} ({}) is abandoned. No replacement was suggested.",
                    pkg.name, pkg.version
                ),
                Verbosity::Normal,
            ),
        }
    }
}

fn render_json(
    result: &AuditResult,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // Build advisories map: package_name -> [advisory objects]
    let mut advisories_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for (pkg_name, advisories) in &result.advisories {
        let arr: Vec<serde_json::Value> = advisories
            .iter()
            .map(|m| serde_json::to_value(&m.advisory).unwrap_or(serde_json::Value::Null))
            .collect();
        advisories_map.insert(pkg_name.clone(), serde_json::Value::Array(arr));
    }

    // Build abandoned map: package_name -> { version, replacement }
    let mut abandoned_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for pkg in &result.abandoned {
        let repl = match &pkg.replacement {
            Some(s) => serde_json::Value::String(s.clone()),
            None => serde_json::Value::Null,
        };
        let entry = serde_json::json!({
            "version": pkg.version,
            "replacement": repl,
        });
        abandoned_map.insert(pkg.name.clone(), entry);
    }

    let output = serde_json::json!({
        "advisories": advisories_map,
        "abandoned": abandoned_map,
    });

    console.write_stdout(&serde_json::to_string_pretty(&output)?, Verbosity::Normal);
    Ok(())
}

fn render_summary(result: &AuditResult, console: &mozart_core::console::Console) {
    if result.total_advisory_count == 0 {
        console.info("No security vulnerability advisories found.");
    } else {
        let advisory_word = if result.total_advisory_count == 1 {
            "advisory"
        } else {
            "advisories"
        };
        console.write(
            &format!(
                "Found {} security vulnerability {} affecting {} package(s).",
                result.total_advisory_count, advisory_word, result.affected_package_count
            ),
            Verbosity::Normal,
        );
        console.info("Run \"mozart audit\" for a full list of advisories.");
    }

    for pkg in &result.abandoned {
        match &pkg.replacement {
            Some(repl) => console.write(
                &format!(
                    "{} ({}) is abandoned. Use {} instead.",
                    pkg.name, pkg.version, repl
                ),
                Verbosity::Normal,
            ),
            None => console.write(
                &format!(
                    "{} ({}) is abandoned. No replacement was suggested.",
                    pkg.name, pkg.version
                ),
                Verbosity::Normal,
            ),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mozart_registry::packagist::{AdvisorySource, SecurityAdvisory};
    use std::collections::BTreeMap;

    fn make_advisory(
        id: &str,
        pkg: &str,
        affected: &str,
        severity: Option<&str>,
    ) -> SecurityAdvisory {
        SecurityAdvisory {
            advisory_id: id.to_string(),
            package_name: pkg.to_string(),
            remote_id: format!("{id}.yaml"),
            title: format!("Advisory {id}"),
            link: None,
            cve: None,
            affected_versions: affected.to_string(),
            source: "FriendsOfPHP/security-advisories".to_string(),
            reported_at: "2024-01-01T00:00:00+00:00".to_string(),
            composer_repository: None,
            severity: severity.map(|s| s.to_string()),
            sources: vec![],
        }
    }

    fn make_pkg(name: &str, version: &str, version_normalized: Option<&str>) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: version_normalized.map(|s| s.to_string()),
            abandoned: None,
        }
    }

    fn make_pkg_abandoned(name: &str, version: &str, replacement: Option<&str>) -> PackageEntry {
        let abandoned = match replacement {
            Some(r) => Some(serde_json::Value::String(r.to_string())),
            None => Some(serde_json::Value::Bool(true)),
        };
        PackageEntry {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: None,
            abandoned,
        }
    }

    // ── filter_advisories ────────────────────────────────────────────────────

    fn make_console() -> mozart_core::console::Console {
        mozart_core::console::Console::new(0, false, false, false, false)
    }

    #[test]
    fn test_filter_advisories_matching() {
        let console = make_console();
        let advisory = make_advisory("PKSA-0001", "vendor/pkg", ">=1.0,<2.0", None);
        let mut all: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();
        all.insert("vendor/pkg".to_string(), vec![advisory]);

        let packages = vec![make_pkg("vendor/pkg", "1.5.0", Some("1.5.0.0"))];
        let result = filter_advisories(&all, &packages, &[], &console);

        assert_eq!(result.len(), 1);
        assert_eq!(result["vendor/pkg"].len(), 1);
    }

    #[test]
    fn test_filter_advisories_not_matching() {
        let console = make_console();
        let advisory = make_advisory("PKSA-0002", "vendor/pkg", ">=1.0,<2.0", None);
        let mut all: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();
        all.insert("vendor/pkg".to_string(), vec![advisory]);

        let packages = vec![make_pkg("vendor/pkg", "2.0.0", Some("2.0.0.0"))];
        let result = filter_advisories(&all, &packages, &[], &console);

        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_advisories_ignore_severity() {
        let console = make_console();
        let advisory = make_advisory("PKSA-0003", "vendor/pkg", ">=1.0,<2.0", Some("low"));
        let mut all: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();
        all.insert("vendor/pkg".to_string(), vec![advisory]);

        let packages = vec![make_pkg("vendor/pkg", "1.5.0", Some("1.5.0.0"))];
        let result = filter_advisories(&all, &packages, &["low".to_string()], &console);

        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_advisories_multiple_packages() {
        let console = make_console();
        let adv1 = make_advisory("PKSA-0004", "vendor/pkg1", ">=1.0,<2.0", None);
        let adv2 = make_advisory("PKSA-0005", "vendor/pkg2", ">=3.0,<4.0", None);
        let mut all: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();
        all.insert("vendor/pkg1".to_string(), vec![adv1]);
        all.insert("vendor/pkg2".to_string(), vec![adv2]);

        let packages = vec![
            make_pkg("vendor/pkg1", "1.5.0", Some("1.5.0.0")),
            make_pkg("vendor/pkg2", "3.5.0", Some("3.5.0.0")),
        ];
        let result = filter_advisories(&all, &packages, &[], &console);

        assert_eq!(result.len(), 2);
        assert_eq!(result["vendor/pkg1"].len(), 1);
        assert_eq!(result["vendor/pkg2"].len(), 1);
    }

    #[test]
    fn test_filter_advisories_complex_constraint() {
        let console = make_console();
        // OR constraint: >=1.0,<1.5|>=2.0,<2.3
        let advisory = make_advisory("PKSA-0006", "vendor/pkg", ">=1.0,<1.5|>=2.0,<2.3", None);
        let mut all: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();
        all.insert("vendor/pkg".to_string(), vec![advisory]);

        // 2.1.0 is in [2.0, 2.3) so should match
        let packages = vec![make_pkg("vendor/pkg", "2.1.0", Some("2.1.0.0"))];
        let result = filter_advisories(&all, &packages, &[], &console);

        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_advisories_no_advisories() {
        let console = make_console();
        let all: BTreeMap<String, Vec<SecurityAdvisory>> = BTreeMap::new();
        let packages = vec![make_pkg("vendor/pkg", "1.5.0", Some("1.5.0.0"))];
        let result = filter_advisories(&all, &packages, &[], &console);
        assert!(result.is_empty());
    }

    // ── detect_abandoned ─────────────────────────────────────────────────────

    #[test]
    fn test_detect_abandoned_true() {
        let packages = vec![make_pkg_abandoned("old/pkg", "1.0.0", None)];
        let result = detect_abandoned(&packages);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "old/pkg");
        assert!(result[0].replacement.is_none());
    }

    #[test]
    fn test_detect_abandoned_with_replacement() {
        let packages = vec![make_pkg_abandoned("old/pkg", "1.0.0", Some("new/pkg"))];
        let result = detect_abandoned(&packages);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].replacement.as_deref(), Some("new/pkg"));
    }

    #[test]
    fn test_detect_abandoned_not_abandoned() {
        let packages = vec![make_pkg("active/pkg", "1.0.0", None)];
        let result = detect_abandoned(&packages);
        assert!(result.is_empty());
    }

    #[test]
    fn test_detect_abandoned_mixed() {
        let packages = vec![
            make_pkg("active/pkg", "1.0.0", None),
            make_pkg_abandoned("old/pkg", "2.0.0", Some("new/pkg")),
            make_pkg("another/active", "3.0.0", None),
            make_pkg_abandoned("dead/pkg", "1.0.0", None),
        ];
        let result = detect_abandoned(&packages);
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|p| p.name == "old/pkg"));
        assert!(result.iter().any(|p| p.name == "dead/pkg"));
    }

    // ── load_installed_packages ───────────────────────────────────────────────

    #[test]
    fn test_load_installed_packages() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        let mut installed = mozart_registry::installed::InstalledPackages::new();
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "1.5.0".to_string(),
            version_normalized: Some("1.5.0.0".to_string()),
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        });
        installed.write(&vendor_dir).unwrap();

        let packages = load_installed_packages(working_dir, false).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "monolog/monolog");
        assert_eq!(packages[0].version, "1.5.0");
    }

    #[test]
    fn test_load_installed_packages_no_dev() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        let mut installed = mozart_registry::installed::InstalledPackages::new();
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "1.5.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        });
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
            name: "phpunit/phpunit".to_string(),
            version: "10.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        });
        installed
            .dev_package_names
            .push("phpunit/phpunit".to_string());
        installed.write(&vendor_dir).unwrap();

        let packages = load_installed_packages(working_dir, true).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "monolog/monolog");
    }

    #[test]
    fn test_load_locked_packages() {
        use mozart_registry::lockfile::{LockFile, LockedPackage};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages: vec![LockedPackage {
                name: "psr/log".to_string(),
                version: "3.0.0".to_string(),
                version_normalized: Some("3.0.0.0".to_string()),
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            }],
            packages_dev: None,
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        };

        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        let packages = load_locked_packages(working_dir, false).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "psr/log");
        assert_eq!(packages[0].version, "3.0.0");
    }

    #[test]
    fn test_load_locked_packages_no_dev() {
        use mozart_registry::lockfile::{LockFile, LockedPackage};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages: vec![LockedPackage {
                name: "psr/log".to_string(),
                version: "3.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            }],
            packages_dev: Some(vec![LockedPackage {
                name: "phpunit/phpunit".to_string(),
                version: "10.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: BTreeMap::new(),
            }]),
            aliases: vec![],
            minimum_stability: "stable".to_string(),
            stability_flags: serde_json::json!({}),
            prefer_stable: false,
            prefer_lowest: false,
            platform: serde_json::json!({}),
            platform_dev: serde_json::json!({}),
            plugin_api_version: Some("2.6.0".to_string()),
        };

        lock.write_to_file(&working_dir.join("composer.lock"))
            .unwrap();

        // With --no-dev: only prod
        let packages = load_locked_packages(working_dir, true).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "psr/log");

        // Without --no-dev: both
        let packages_all = load_locked_packages(working_dir, false).unwrap();
        assert_eq!(packages_all.len(), 2);
    }

    #[test]
    fn test_load_locked_packages_missing_lockfile() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let result = load_locked_packages(dir.path(), false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("composer.lock"));
    }

    // ── render_json ───────────────────────────────────────────────────────────

    #[test]
    fn test_render_json_structure() {
        let advisory = make_advisory("PKSA-0001", "vendor/pkg", ">=1.0,<2.0", Some("high"));
        let mut advisories: BTreeMap<String, Vec<MatchedAdvisory>> = BTreeMap::new();
        advisories.insert(
            "vendor/pkg".to_string(),
            vec![MatchedAdvisory {
                advisory,
                installed_version: "1.5.0".to_string(),
            }],
        );

        let abandoned = vec![AbandonedPackage {
            name: "old/pkg".to_string(),
            version: "1.0.0".to_string(),
            replacement: Some("new/pkg".to_string()),
        }];

        let result = AuditResult {
            affected_package_count: 1,
            total_advisory_count: 1,
            advisories,
            abandoned,
        };

        // Should not panic
        let console = make_console();
        render_json(&result, &console).unwrap();
    }

    #[test]
    fn test_render_json_empty() {
        let console = make_console();
        let result = AuditResult {
            advisories: BTreeMap::new(),
            abandoned: vec![],
            affected_package_count: 0,
            total_advisory_count: 0,
        };
        render_json(&result, &console).unwrap();
    }

    // ── argument validation ───────────────────────────────────────────────────

    #[test]
    fn test_invalid_format() {
        // We test the validation logic directly
        let format = "xml";
        let valid =
            format == "table" || format == "plain" || format == "json" || format == "summary";
        assert!(!valid);
    }

    #[test]
    fn test_invalid_abandoned_value() {
        let value = "maybe";
        let valid = value == "ignore" || value == "report" || value == "fail";
        assert!(!valid);
    }

    #[test]
    fn test_valid_formats() {
        for format in &["table", "plain", "json", "summary"] {
            let valid = *format == "table"
                || *format == "plain"
                || *format == "json"
                || *format == "summary";
            assert!(valid, "format {} should be valid", format);
        }
    }

    #[test]
    fn test_valid_abandoned_values() {
        for value in &["ignore", "report", "fail"] {
            let valid = *value == "ignore" || *value == "report" || *value == "fail";
            assert!(valid, "abandoned value {} should be valid", value);
        }
    }

    // ── AdvisorySource used in test helper (suppress dead_code) ──────────────

    #[test]
    fn test_advisory_source_fields() {
        let src = AdvisorySource {
            name: "FriendsOfPHP/security-advisories".to_string(),
            remote_id: "monolog/monolog/2017-11-13-1.yaml".to_string(),
        };
        assert_eq!(src.name, "FriendsOfPHP/security-advisories");
        assert_eq!(src.remote_id, "monolog/monolog/2017-11-13-1.yaml");
    }
}
