use crate::composer::Composer;
use clap::Args;
use indexmap::IndexMap;
use mozart_core::console::IoInterface;
use mozart_core::console::hyperlink;
use mozart_core::console_writeln;
use mozart_core::package::PackageInterface as _;
use mozart_core::package_info;
use mozart_core::package_info::PackageUrls;
use mozart_core::package_sorter::sort_packages_alphabetically;
use mozart_core::repository_utils;
use mozart_core::repository_utils::Required;
use serde::Serialize as _;

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
    requires: indexmap::IndexMap<String, String>,
    support_source: Option<String>,
    source_url: Option<String>,
    homepage: Option<String>,
}

impl Required for LicenseEntry {
    fn package_name(&self) -> &str {
        &self.name
    }
    fn requires(&self) -> &indexmap::IndexMap<String, String> {
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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let format = args.format.as_deref().unwrap_or("text");
    if format != "text" && format != "json" && format != "summary" {
        anyhow::bail!(
            "Unsupported format \"{}\".  See help for supported formats.",
            format
        );
    }

    let composer = Composer::require(io.clone(), &working_dir)?;

    // TODO(plugins): dispatch CommandEvent for `licenses`.

    let root = composer.package();

    // Re-parse composer.json to handle the array form of `license` —
    // `RawPackageData` only deserializes the string form, so both string
    // and array values must be read from the raw JSON here.
    let root_licenses = read_root_licenses(&working_dir.join("composer.json"))?;
    let root_pretty_name = root.name().to_string();
    let root_version = {
        let v = root.pretty_version();
        if v == "1.0.0+no-version-set" {
            "No version set".to_string()
        } else {
            v.to_string()
        }
    };

    let mut entries = if args.locked {
        load_locked_entries(&working_dir, args.no_dev)?
    } else {
        load_installed_entries(&working_dir, root.requires(), args.no_dev)?
    };

    sort_packages_alphabetically(&mut entries, |e| e.name.as_str());

    match format {
        "json" => render_json(
            &root_pretty_name,
            &root_version,
            &root_licenses,
            &entries,
            io,
        )?,
        "summary" => render_summary(&entries, io.clone()),
        _ => render_text(
            &root_pretty_name,
            &root_version,
            &root_licenses,
            &entries,
            io,
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

fn load_installed_entries<V>(
    working_dir: &std::path::Path,
    root_requires: &indexmap::IndexMap<String, V>,
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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) {
    let license_display = if root_licenses.is_empty() {
        "none".to_string()
    } else {
        root_licenses.join(", ")
    };
    console_writeln!(io, "Name: <comment>{root_pretty_name}</comment>");
    console_writeln!(io, "Version: <comment>{root_version}</comment>");
    console_writeln!(io, "Licenses: <comment>{license_display}</comment>");
    console_writeln!(io, "Dependencies:");
    console_writeln!(io, "");

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
        io,
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
            Some(url) => hyperlink(&url, &padded_name, io.lock().unwrap().is_decorated()),
            None => padded_name,
        };
        console_writeln!(
            io,
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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
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
    console_writeln!(io, "{}", &String::from_utf8(ser.into_inner())?);
    Ok(())
}

fn render_summary(
    entries: &[LicenseEntry],
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) {
    let counts = tally_licenses(entries);

    if counts.is_empty() {
        console_writeln!(io, "No dependencies found.");
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

    console_writeln!(io, " {} {}", border_col1, border_col2);
    console_writeln!(
        io,
        "  {:<lw$}   {:<cw$}",
        "License",
        COL2_HEADER,
        lw = license_width,
        cw = count_width,
    );
    console_writeln!(io, " {} {}", border_col1, border_col2);
    for (license, count) in &counts {
        console_writeln!(
            io,
            "  {:<lw$}   {:<cw$}",
            license,
            count,
            lw = license_width,
            cw = count_width,
        );
    }
    console_writeln!(io, " {} {}", border_col1, border_col2);
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
