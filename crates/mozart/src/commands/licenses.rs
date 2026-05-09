use crate::composer::Composer;
use clap::Args;
use indexmap::IndexMap;
use mozart_core::console::Console;
use mozart_core::console::hyperlink;
use mozart_core::console_writeln;
use mozart_core::package_info;
use mozart_core::package_info::PackageUrls;
use mozart_core::package_sorter::sort_packages_alphabetically;
use mozart_core::repository_utils;
use mozart_core::repository_utils::Required;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Args)]
pub struct LicensesArgs {
    /// Format of the output: text, json or summary
    #[arg(short, long)]
    pub format: Option<String>,

    /// Disables search in require-dev packages.
    #[arg(long)]
    pub no_dev: bool,

    /// Shows licenses from the lock file instead of what's currently installed.
    #[arg(long)]
    pub locked: bool,
}

/// Unified view over an installed or locked package, carrying the
/// fields the `licenses` command renders. Mirrors the slice of
/// `CompletePackageInterface` consumed by `LicensesCommand` — name,
/// version, license, requires, and the URL bits used to build a
/// `<href>` link in the text output.
struct LicenseEntry {
    pretty_name: String,
    name: String,
    version: String,
    licenses: Vec<String>,
    requires: BTreeMap<String, String>,
    support_source: Option<String>,
    source_url: Option<String>,
    homepage: Option<String>,
}

impl Required for LicenseEntry {
    fn package_name(&self) -> &str {
        &self.name
    }
    fn requires(&self) -> &BTreeMap<String, String> {
        &self.requires
    }
}

impl PackageUrls for LicenseEntry {
    fn support_source(&self) -> Option<&str> {
        self.support_source.as_deref()
    }
    fn source_url(&self) -> Option<&str> {
        self.source_url.as_deref()
    }
    fn homepage(&self) -> Option<&str> {
        self.homepage.as_deref()
    }
}

pub async fn execute(
    args: &LicensesArgs,
    cli: &super::Cli,
    console: &Console,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let format = args.format.as_deref().unwrap_or("text");
    if format != "text" && format != "json" && format != "summary" {
        anyhow::bail!(
            "Unsupported format \"{}\".  See help for supported formats.",
            format
        );
    }

    let composer = Composer::require(&working_dir)?;

    // TODO(plugins): dispatch CommandEvent for `licenses`.

    let root = composer.package();

    // RawPackageData stores `license` as `Option<String>` only, so we
    // re-parse the composer.json to also accept the array form Composer
    // recognises via `RootPackageLoader`'s `(array) $config['license']`
    // coercion. Track widening `RawPackageData::license` separately.
    let root_licenses = read_root_licenses(&working_dir.join("composer.json"))?;
    let root_pretty_name = root.name.clone();
    let root_version = root
        .version
        .clone()
        .unwrap_or_else(|| "No version set".to_string());

    let mut entries = if args.locked {
        load_locked_entries(&working_dir, args.no_dev)?
    } else {
        load_installed_entries(&working_dir, &root.require, args.no_dev)?
    };

    sort_packages_alphabetically(&mut entries, |e| e.name.as_str());

    match format {
        "json" => render_json(
            &root_pretty_name,
            &root_version,
            &root_licenses,
            &entries,
            console,
        )?,
        "summary" => render_summary(&entries, console),
        _ => render_text(
            &root_pretty_name,
            &root_version,
            &root_licenses,
            &entries,
            console,
        ),
    }

    Ok(())
}

fn read_root_licenses(composer_json_path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let raw = std::fs::read_to_string(composer_json_path)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    Ok(match value.get("license") {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    })
}

fn load_installed_entries(
    working_dir: &std::path::Path,
    root_requires: &BTreeMap<String, String>,
    no_dev: bool,
) -> anyhow::Result<Vec<LicenseEntry>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_core::repository::installed::InstalledPackages::read(&vendor_dir)?;

    let entries: Vec<LicenseEntry> = installed.packages.iter().map(installed_to_entry).collect();

    if no_dev {
        // Mirrors Composer's `--no-dev` branch in `LicensesCommand`:
        // `RepositoryUtils::filterRequiredPackages($repo->getPackages(), $root)`
        // — root's `require` only, transitively. Dev-only requires of
        // the root, and packages reachable only through them, drop out.
        let kept = repository_utils::filter_required_packages(&entries, root_requires, None);
        let mut out = Vec::with_capacity(kept.len());
        // We can't `entries[idx].clone()` without Clone; rebuild from
        // owned `entries` by index in two passes.
        let mut by_idx: Vec<Option<LicenseEntry>> = entries.into_iter().map(Some).collect();
        for idx in kept {
            if let Some(e) = by_idx[idx].take() {
                out.push(e);
            }
        }
        Ok(out)
    } else {
        Ok(entries)
    }
}

fn load_locked_entries(
    working_dir: &std::path::Path,
    no_dev: bool,
) -> anyhow::Result<Vec<LicenseEntry>> {
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "Valid composer.json and composer.lock files are required to run this command with --locked"
        );
    }
    let lock = mozart_core::repository::lockfile::LockFile::read_from_file(&lock_path)?;

    // Mirrors `Locker::getLockedRepository(!$noDev)`: the prod-only call
    // returns just `packages`, the dev-included call returns the union.
    let mut entries: Vec<LicenseEntry> = lock.packages.iter().map(locked_to_entry).collect();
    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        entries.extend(pkgs_dev.iter().map(locked_to_entry));
    }
    Ok(entries)
}

fn installed_to_entry(
    pkg: &mozart_core::repository::installed::InstalledPackageEntry,
) -> LicenseEntry {
    let licenses = pkg
        .extra_fields
        .get("license")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let requires = pkg
        .extra_fields
        .get("require")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let support_source = pkg
        .support
        .as_ref()
        .and_then(|s| s.get("source"))
        .and_then(|s| s.as_str())
        .map(String::from);

    let source_url = pkg
        .source
        .as_ref()
        .and_then(|s| s.get("url"))
        .and_then(|s| s.as_str())
        .map(String::from);

    LicenseEntry {
        pretty_name: pkg.name.clone(),
        name: pkg.name.to_lowercase(),
        version: pkg.version.clone(),
        licenses,
        requires,
        support_source,
        source_url,
        homepage: pkg.homepage.clone(),
    }
}

fn locked_to_entry(pkg: &mozart_core::repository::lockfile::LockedPackage) -> LicenseEntry {
    let support_source = pkg
        .support
        .as_ref()
        .and_then(|s| s.get("source"))
        .and_then(|s| s.as_str())
        .map(String::from);

    LicenseEntry {
        pretty_name: pkg.name.clone(),
        name: pkg.name.to_lowercase(),
        version: pkg.version.clone(),
        licenses: pkg.license.clone().unwrap_or_default(),
        requires: pkg.require.clone(),
        support_source,
        source_url: pkg.source.as_ref().map(|s| s.url.clone()),
        homepage: pkg.homepage.clone(),
    }
}

fn render_text(
    root_pretty_name: &str,
    root_version: &str,
    root_licenses: &[String],
    entries: &[LicenseEntry],
    console: &Console,
) {
    let license_display = if root_licenses.is_empty() {
        "none".to_string()
    } else {
        root_licenses.join(", ")
    };
    console_writeln!(console, "Name: <comment>{root_pretty_name}</comment>");
    console_writeln!(console, "Version: <comment>{root_version}</comment>");
    console_writeln!(console, "Licenses: <comment>{license_display}</comment>");
    console_writeln!(console, "Dependencies:");
    console_writeln!(console, "");

    if entries.is_empty() {
        return;
    }

    let name_width = entries
        .iter()
        .map(|e| e.pretty_name.len())
        .max()
        .unwrap_or(0)
        .max("Name".len());
    let version_width = entries
        .iter()
        .map(|e| e.version.len())
        .max()
        .unwrap_or(0)
        .max("Version".len());

    console_writeln!(
        console,
        "{:<nw$}  {:<vw$}  Licenses",
        "Name",
        "Version",
        nw = name_width,
        vw = version_width,
    );

    for entry in entries {
        let license_str = if entry.licenses.is_empty() {
            "none".to_string()
        } else {
            entry.licenses.join(", ")
        };
        let padded_name = format!("{:<nw$}", entry.pretty_name, nw = name_width);
        let name_cell = match package_info::view_source_or_homepage_url(entry) {
            Some(url) => hyperlink(&url, &padded_name, console.decorated),
            None => padded_name,
        };
        console_writeln!(
            console,
            "{}  {:<vw$}  {}",
            name_cell,
            entry.version,
            license_str,
            vw = version_width,
        );
    }
}

fn render_json(
    root_pretty_name: &str,
    root_version: &str,
    root_licenses: &[String],
    entries: &[LicenseEntry],
    console: &Console,
) -> anyhow::Result<()> {
    let root_license_arr: Vec<serde_json::Value> = root_licenses
        .iter()
        .map(|s| serde_json::Value::String(s.clone()))
        .collect();

    let mut dependencies: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for entry in entries {
        let license_arr: Vec<serde_json::Value> = entry
            .licenses
            .iter()
            .map(|l| serde_json::Value::String(l.clone()))
            .collect();
        dependencies.insert(
            entry.pretty_name.clone(),
            serde_json::json!({
                "version": entry.version,
                "license": license_arr,
            }),
        );
    }

    let output = serde_json::json!({
        "name": root_pretty_name,
        "version": root_version,
        "license": root_license_arr,
        "dependencies": dependencies,
    });

    let buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
    output.serialize(&mut ser)?;
    console_writeln!(console, "{}", &String::from_utf8(ser.into_inner())?);
    Ok(())
}

fn render_summary(entries: &[LicenseEntry], console: &Console) {
    let counts = tally_licenses(entries);

    if counts.is_empty() {
        console_writeln!(console, "No dependencies found.");
        return;
    }

    const COL2_HEADER: &str = "Number of dependencies";

    let license_width = counts
        .iter()
        .map(|(l, _)| l.len())
        .max()
        .unwrap_or(0)
        .max("License".len());
    let count_width = counts
        .iter()
        .map(|(_, c)| c.to_string().len())
        .max()
        .unwrap_or(0)
        .max(COL2_HEADER.len());

    let border_col1 = "-".repeat(license_width + 2);
    let border_col2 = "-".repeat(count_width + 2);

    console_writeln!(console, " {} {}", border_col1, border_col2);
    console_writeln!(
        console,
        "  {:<lw$}   {:<cw$}",
        "License",
        COL2_HEADER,
        lw = license_width,
        cw = count_width,
    );
    console_writeln!(console, " {} {}", border_col1, border_col2);
    for (license, count) in &counts {
        console_writeln!(
            console,
            "  {:<lw$}   {:<cw$}",
            license,
            count,
            lw = license_width,
            cw = count_width,
        );
    }
    console_writeln!(console, " {} {}", border_col1, border_col2);
}

/// Mirror of `LicensesCommand::execute`'s `summary` accumulator.
///
/// PHP iterates the (already alphabetically sorted) packages, increments
/// `$usedLicenses[$name]++`, then `arsort()` — descending by count,
/// ties resolved in the array's existing order (which is first-seen).
/// `IndexMap` preserves first-seen order; sorting it with a stable
/// `sort_by` reproduces PHP's tie-break exactly.
fn tally_licenses(entries: &[LicenseEntry]) -> Vec<(String, usize)> {
    let mut counts: IndexMap<String, usize> = IndexMap::new();
    for entry in entries {
        if entry.licenses.is_empty() {
            *counts.entry("none".to_string()).or_insert(0) += 1;
        } else {
            for lic in &entry.licenses {
                *counts.entry(lic.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut result: Vec<(String, usize)> = counts.into_iter().collect();
    result.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn entry(name: &str, licenses: &[&str]) -> LicenseEntry {
        LicenseEntry {
            pretty_name: name.to_string(),
            name: name.to_lowercase(),
            version: "1.0.0".to_string(),
            licenses: licenses.iter().map(|s| s.to_string()).collect(),
            requires: BTreeMap::new(),
            support_source: None,
            source_url: None,
            homepage: None,
        }
    }

    #[test]
    fn tally_licenses_orders_by_count_then_first_seen() {
        // First MIT entry comes before Apache-2.0; tie-break must keep
        // MIT first when their counts collide.
        let entries = vec![
            entry("a/a", &["MIT"]),
            entry("b/b", &["Apache-2.0"]),
            entry("c/c", &["BSD-3-Clause"]),
        ];
        let counts = tally_licenses(&entries);
        // All three at count 1 — input order preserved.
        assert_eq!(
            counts,
            vec![
                ("MIT".to_string(), 1),
                ("Apache-2.0".to_string(), 1),
                ("BSD-3-Clause".to_string(), 1),
            ]
        );
    }

    #[test]
    fn tally_licenses_count_descending() {
        let entries = vec![
            entry("a/a", &["Apache-2.0"]),
            entry("b/b", &["MIT"]),
            entry("c/c", &["MIT"]),
        ];
        let counts = tally_licenses(&entries);
        assert_eq!(counts[0], ("MIT".to_string(), 2));
        assert_eq!(counts[1], ("Apache-2.0".to_string(), 1));
    }

    #[test]
    fn tally_licenses_empty() {
        assert!(tally_licenses(&[]).is_empty());
    }

    #[test]
    fn tally_licenses_no_license_counts_as_none() {
        let entries = vec![entry("a/a", &[])];
        let counts = tally_licenses(&entries);
        assert_eq!(counts, vec![("none".to_string(), 1)]);
    }

    #[test]
    fn read_root_licenses_string_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composer.json");
        std::fs::write(&path, r#"{"name": "test/p", "license": "MIT"}"#).unwrap();
        assert_eq!(read_root_licenses(&path).unwrap(), vec!["MIT"]);
    }

    #[test]
    fn read_root_licenses_array_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composer.json");
        std::fs::write(
            &path,
            r#"{"name": "test/p", "license": ["MIT", "Apache-2.0"]}"#,
        )
        .unwrap();
        assert_eq!(
            read_root_licenses(&path).unwrap(),
            vec!["MIT", "Apache-2.0"]
        );
    }

    #[test]
    fn read_root_licenses_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("composer.json");
        std::fs::write(&path, r#"{"name": "test/p"}"#).unwrap();
        assert!(read_root_licenses(&path).unwrap().is_empty());
    }

    #[test]
    fn installed_to_entry_extracts_require_and_license() {
        use mozart_core::repository::installed::InstalledPackageEntry;
        let mut extra = BTreeMap::new();
        extra.insert("license".to_string(), serde_json::json!(["MIT"]));
        extra.insert(
            "require".to_string(),
            serde_json::json!({"psr/log": "^1.0"}),
        );
        let pkg = InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: extra,
        };
        let e = installed_to_entry(&pkg);
        assert_eq!(e.licenses, vec!["MIT"]);
        assert_eq!(e.requires.get("psr/log").map(String::as_str), Some("^1.0"));
    }

    #[test]
    fn installed_to_entry_pulls_support_source_and_source_url() {
        use mozart_core::repository::installed::InstalledPackageEntry;
        let pkg = InstalledPackageEntry {
            name: "vendor/pkg".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: Some(serde_json::json!({"type": "git", "url": "https://example.com/repo.git"})),
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: Some("https://example.com/".to_string()),
            support: Some(serde_json::json!({"source": "https://github.com/v/p"})),
            extra_fields: BTreeMap::new(),
        };
        let e = installed_to_entry(&pkg);
        assert_eq!(e.support_source.as_deref(), Some("https://github.com/v/p"));
        assert_eq!(
            e.source_url.as_deref(),
            Some("https://example.com/repo.git")
        );
        assert_eq!(e.homepage.as_deref(), Some("https://example.com/"));
        // PackageInfo helpers should pick support source first.
        assert_eq!(
            package_info::view_source_or_homepage_url(&e).as_deref(),
            Some("https://github.com/v/p"),
        );
    }

    #[test]
    fn no_dev_filters_to_root_require_closure() {
        // Set up: root requires a/a only. b/b is in installed but not
        // reachable; should be dropped under --no-dev.
        let dir = tempfile::tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project", "require": {"a/a": "*"}}"#,
        )
        .unwrap();

        let mut installed = mozart_core::repository::installed::InstalledPackages::new();
        installed.upsert(mozart_core::repository::installed::InstalledPackageEntry {
            name: "a/a".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: BTreeMap::new(),
        });
        installed.upsert(mozart_core::repository::installed::InstalledPackageEntry {
            name: "b/b".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            homepage: None,
            support: None,
            extra_fields: BTreeMap::new(),
        });
        installed.write(&vendor_dir).unwrap();

        let mut root_req = BTreeMap::new();
        root_req.insert("a/a".to_string(), "*".to_string());

        let kept = load_installed_entries(working_dir, &root_req, true).unwrap();
        let names: Vec<&str> = kept.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a/a"]);

        // Without --no-dev: both packages are listed.
        let all = load_installed_entries(working_dir, &root_req, false).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn locked_no_dev_drops_packages_dev() {
        use mozart_core::repository::lockfile::{LockFile, LockedPackage};
        let dir = tempfile::tempdir().unwrap();
        let working_dir = dir.path();
        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project"}"#,
        )
        .unwrap();
        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc".to_string(),
            packages: vec![LockedPackage {
                name: "psr/log".to_string(),
                version: "3.0.0".to_string(),
                version_normalized: None,
                source: None,
                dist: None,
                require: BTreeMap::new(),
                require_dev: BTreeMap::new(),
                conflict: BTreeMap::new(),
                provide: BTreeMap::new(),
                replace: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: Some(vec!["MIT".to_string()]),
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
                provide: BTreeMap::new(),
                replace: BTreeMap::new(),
                suggest: None,
                package_type: None,
                autoload: None,
                autoload_dev: None,
                license: Some(vec!["BSD-3-Clause".to_string()]),
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

        let prod = load_locked_entries(working_dir, true).unwrap();
        assert_eq!(prod.len(), 1);
        assert_eq!(prod[0].name, "psr/log");

        let all = load_locked_entries(working_dir, false).unwrap();
        assert_eq!(all.len(), 2);
    }
}
