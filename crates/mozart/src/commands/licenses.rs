use clap::Args;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct LicensesArgs {
    /// Output format (text, json, summary)
    #[arg(short, long)]
    pub format: Option<String>,

    /// Disables listing of require-dev packages
    #[arg(long)]
    pub no_dev: bool,

    /// List packages from the lock file
    #[arg(long)]
    pub locked: bool,
}

// ─── Data structures ────────────────────────────────────────────────────────

struct LicenseEntry {
    name: String,
    version: String,
    licenses: Vec<String>,
}

// ─── Main entry point ───────────────────────────────────────────────────────

pub fn execute(args: &LicensesArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // Validate format
    let format = args.format.as_deref().unwrap_or("text");
    if format != "text" && format != "json" && format != "summary" {
        anyhow::bail!(
            "Invalid format \"{}\". Supported formats: text, json, summary",
            format
        );
    }

    // Load root package
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }
    let root = crate::package::read_from_file(&composer_json_path)?;

    let root_name = root.name.clone();
    let root_version = root
        .extra_fields
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("No version set")
        .to_string();
    let root_license = root.license.clone().unwrap_or_else(|| "none".to_string());

    // Load dependency entries
    let entries = if args.locked {
        load_locked_licenses(&working_dir, args.no_dev)?
    } else {
        load_installed_licenses(&working_dir, args.no_dev)?
    };

    // Render output
    match format {
        "json" => render_json(&root_name, &root_version, &root_license, &entries)?,
        "summary" => render_summary(&entries),
        _ => render_text(&root_name, &root_version, &root_license, &entries),
    }

    Ok(())
}

// ─── Package loading ─────────────────────────────────────────────────────────

fn load_installed_licenses(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<LicenseEntry>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = crate::installed::InstalledPackages::read(&vendor_dir)?;

    let dev_names: HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    let mut entries: Vec<LicenseEntry> = installed
        .packages
        .iter()
        .filter(|p| {
            if no_dev && dev_names.contains(&p.name.to_lowercase()) {
                return false;
            }
            true
        })
        .map(|p| LicenseEntry {
            name: p.name.clone(),
            version: p.version.clone(),
            licenses: extract_installed_licenses(p),
        })
        .collect();

    entries.sort_by_key(|a| a.name.to_lowercase());
    Ok(entries)
}

fn load_locked_licenses(working_dir: &Path, no_dev: bool) -> anyhow::Result<Vec<LicenseEntry>> {
    let lock_path = working_dir.join("composer.lock");
    if !lock_path.exists() {
        anyhow::bail!(
            "A valid composer.json and composer.lock file is required to run this command with --locked"
        );
    }

    let lock = crate::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut all_packages: Vec<&crate::lockfile::LockedPackage> = lock.packages.iter().collect();

    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        all_packages.extend(pkgs_dev.iter());
    }

    let mut entries: Vec<LicenseEntry> = all_packages
        .iter()
        .map(|p| LicenseEntry {
            name: p.name.clone(),
            version: p.version.clone(),
            licenses: p.license.clone().unwrap_or_default(),
        })
        .collect();

    entries.sort_by_key(|a| a.name.to_lowercase());
    Ok(entries)
}

// ─── License extraction ───────────────────────────────────────────────────────

fn extract_installed_licenses(pkg: &crate::installed::InstalledPackageEntry) -> Vec<String> {
    pkg.extra_fields
        .get("license")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ─── License counting ─────────────────────────────────────────────────────────

fn count_licenses(entries: &[LicenseEntry]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

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
    // Sort by count descending, then by name ascending for stability
    result.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    result
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn render_text(root_name: &str, root_version: &str, root_license: &str, entries: &[LicenseEntry]) {
    // Print root package header
    println!("Name: {}", root_name);
    println!("Version: {}", root_version);
    println!("Licenses: {}", root_license);
    println!("Dependencies:");
    println!();

    if entries.is_empty() {
        return;
    }

    // Compute column widths
    let name_width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let version_width = entries.iter().map(|e| e.version.len()).max().unwrap_or(0);

    for entry in entries {
        let license_str = if entry.licenses.is_empty() {
            "none".to_string()
        } else {
            entry.licenses.join(", ")
        };
        println!(
            "{:<nw$}  {:<vw$}  {}",
            entry.name,
            entry.version,
            license_str,
            nw = name_width,
            vw = version_width
        );
    }
}

fn render_json(
    root_name: &str,
    root_version: &str,
    root_license: &str,
    entries: &[LicenseEntry],
) -> anyhow::Result<()> {
    let root_license_arr: Vec<serde_json::Value> = if root_license == "none" {
        vec![]
    } else {
        vec![serde_json::Value::String(root_license.to_string())]
    };

    let mut dependencies: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for entry in entries {
        let license_arr: Vec<serde_json::Value> = entry
            .licenses
            .iter()
            .map(|l| serde_json::Value::String(l.clone()))
            .collect();
        dependencies.insert(
            entry.name.clone(),
            serde_json::json!({
                "version": entry.version,
                "license": license_arr,
            }),
        );
    }

    let output = serde_json::json!({
        "name": root_name,
        "version": root_version,
        "license": root_license_arr,
        "dependencies": dependencies,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn render_summary(entries: &[LicenseEntry]) {
    let counts = count_licenses(entries);

    if counts.is_empty() {
        println!("No dependencies found.");
        return;
    }

    let license_width = counts
        .iter()
        .map(|(l, _)| l.len())
        .max()
        .unwrap_or(0)
        .max("License".len());

    println!(
        "{:<lw$}  Number of dependencies",
        "License",
        lw = license_width
    );
    println!("{:-<lw$}  ----------------------", "", lw = license_width);

    for (license, count) in &counts {
        println!("{:<lw$}  {}", license, count, lw = license_width);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_installed_pkg(
        name: &str,
        version: &str,
        extra: BTreeMap<String, serde_json::Value>,
    ) -> crate::installed::InstalledPackageEntry {
        crate::installed::InstalledPackageEntry {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: extra,
        }
    }

    // ── extract_installed_licenses ────────────────────────────────────────────

    #[test]
    fn test_extract_installed_licenses_present() {
        let mut extra = BTreeMap::new();
        extra.insert("license".to_string(), serde_json::json!(["MIT"]));
        let pkg = make_installed_pkg("vendor/pkg", "1.0.0", extra);
        assert_eq!(extract_installed_licenses(&pkg), vec!["MIT"]);
    }

    #[test]
    fn test_extract_installed_licenses_multiple() {
        let mut extra = BTreeMap::new();
        extra.insert(
            "license".to_string(),
            serde_json::json!(["MIT", "Apache-2.0"]),
        );
        let pkg = make_installed_pkg("vendor/pkg", "1.0.0", extra);
        let result = extract_installed_licenses(&pkg);
        assert_eq!(result, vec!["MIT", "Apache-2.0"]);
    }

    #[test]
    fn test_extract_installed_licenses_absent() {
        let pkg = make_installed_pkg("vendor/pkg", "1.0.0", BTreeMap::new());
        assert!(extract_installed_licenses(&pkg).is_empty());
    }

    #[test]
    fn test_extract_installed_licenses_none_value() {
        let mut extra = BTreeMap::new();
        extra.insert("license".to_string(), serde_json::Value::Null);
        let pkg = make_installed_pkg("vendor/pkg", "1.0.0", extra);
        assert!(extract_installed_licenses(&pkg).is_empty());
    }

    // ── count_licenses ────────────────────────────────────────────────────────

    #[test]
    fn test_count_licenses() {
        let entries = vec![
            LicenseEntry {
                name: "a/a".to_string(),
                version: "1.0.0".to_string(),
                licenses: vec!["MIT".to_string()],
            },
            LicenseEntry {
                name: "b/b".to_string(),
                version: "1.0.0".to_string(),
                licenses: vec!["MIT".to_string()],
            },
            LicenseEntry {
                name: "c/c".to_string(),
                version: "1.0.0".to_string(),
                licenses: vec!["Apache-2.0".to_string()],
            },
        ];

        let counts = count_licenses(&entries);
        assert_eq!(counts.len(), 2);
        // MIT should come first (count=2)
        assert_eq!(counts[0], ("MIT".to_string(), 2));
        assert_eq!(counts[1], ("Apache-2.0".to_string(), 1));
    }

    #[test]
    fn test_count_licenses_empty() {
        let entries: Vec<LicenseEntry> = vec![];
        let counts = count_licenses(&entries);
        assert!(counts.is_empty());
    }

    #[test]
    fn test_count_licenses_no_license() {
        let entries = vec![LicenseEntry {
            name: "a/a".to_string(),
            version: "1.0.0".to_string(),
            licenses: vec![],
        }];
        let counts = count_licenses(&entries);
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0], ("none".to_string(), 1));
    }

    // ── Integration tests ─────────────────────────────────────────────────────

    #[test]
    fn test_load_installed_licenses_basic() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        // Write composer.json (required by execute, but not needed for load_installed_licenses)
        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project"}"#,
        )
        .unwrap();

        // Build installed packages
        let mut installed = crate::installed::InstalledPackages::new();
        let mut extra = BTreeMap::new();
        extra.insert("license".to_string(), serde_json::json!(["MIT"]));
        installed.upsert(crate::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: extra,
        });

        installed.write(&vendor_dir).unwrap();

        let entries = load_installed_licenses(working_dir, false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "monolog/monolog");
        assert_eq!(entries[0].version, "3.0.0");
        assert_eq!(entries[0].licenses, vec!["MIT"]);
    }

    #[test]
    fn test_load_installed_licenses_no_dev() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project"}"#,
        )
        .unwrap();

        let mut installed = crate::installed::InstalledPackages::new();

        // Production package
        let mut extra_prod = BTreeMap::new();
        extra_prod.insert("license".to_string(), serde_json::json!(["MIT"]));
        installed.upsert(crate::installed::InstalledPackageEntry {
            name: "monolog/monolog".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: extra_prod,
        });

        // Dev package
        let mut extra_dev = BTreeMap::new();
        extra_dev.insert("license".to_string(), serde_json::json!(["BSD-3-Clause"]));
        installed.upsert(crate::installed::InstalledPackageEntry {
            name: "phpunit/phpunit".to_string(),
            version: "10.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: extra_dev,
        });
        installed
            .dev_package_names
            .push("phpunit/phpunit".to_string());

        installed.write(&vendor_dir).unwrap();

        // With --no-dev: only production package
        let entries = load_installed_licenses(working_dir, true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "monolog/monolog");

        // Without --no-dev: both packages
        let entries_all = load_installed_licenses(working_dir, false).unwrap();
        assert_eq!(entries_all.len(), 2);
    }

    #[test]
    fn test_load_locked_licenses_basic() {
        use crate::lockfile::{LockFile, LockedPackage};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        std::fs::write(
            working_dir.join("composer.json"),
            r#"{"name": "test/project"}"#,
        )
        .unwrap();

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

        // With --no-dev: only production packages
        let entries = load_locked_licenses(working_dir, true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "psr/log");
        assert_eq!(entries[0].licenses, vec!["MIT"]);

        // Without --no-dev: both packages
        let entries_all = load_locked_licenses(working_dir, false).unwrap();
        assert_eq!(entries_all.len(), 2);
    }
}
