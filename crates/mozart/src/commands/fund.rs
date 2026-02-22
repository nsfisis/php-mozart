use clap::Args;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct FundArgs {
    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

// ─── Data structures ────────────────────────────────────────────────────────

struct FundingLink {
    url: String,
    funding_type: Option<String>,
}

struct FundingEntry {
    full_name: String,
    links: Vec<FundingLink>,
}

// ─── Main entry point ───────────────────────────────────────────────────────

pub async fn execute(
    args: &FundArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // Validate format
    let format = args.format.as_deref().unwrap_or("text");
    if format != "text" && format != "json" {
        anyhow::bail!(
            "Invalid format \"{}\". Supported formats: text, json",
            format
        );
    }

    // Try lock file first (preferred), fall back to installed.json
    let lock_path = working_dir.join("composer.lock");
    let entries = if lock_path.exists() {
        collect_funding_from_locked(&working_dir)?
    } else {
        collect_funding_from_installed(&working_dir)?
    };

    let grouped = group_by_vendor(&entries);

    match format {
        "json" => render_json(&grouped)?,
        _ => render_text(&grouped),
    }

    Ok(())
}

// ─── Package loading ─────────────────────────────────────────────────────────

fn collect_funding_from_locked(working_dir: &Path) -> anyhow::Result<Vec<FundingEntry>> {
    let lock_path = working_dir.join("composer.lock");
    let lock = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)?;

    let mut all_packages: Vec<&mozart_registry::lockfile::LockedPackage> =
        lock.packages.iter().collect();
    if let Some(ref pkgs_dev) = lock.packages_dev {
        all_packages.extend(pkgs_dev.iter());
    }

    let entries = all_packages
        .iter()
        .filter_map(|p| {
            let funding_vec = p.funding.as_deref()?;
            if funding_vec.is_empty() {
                return None;
            }
            let links = extract_funding_links(funding_vec);
            if links.is_empty() {
                return None;
            }
            Some(FundingEntry {
                full_name: p.name.clone(),
                links,
            })
        })
        .collect();

    Ok(entries)
}

fn collect_funding_from_installed(working_dir: &Path) -> anyhow::Result<Vec<FundingEntry>> {
    let vendor_dir = working_dir.join("vendor");
    let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;

    let entries = installed
        .packages
        .iter()
        .filter_map(|p| {
            let funding_val = p.extra_fields.get("funding")?;
            let funding_arr = funding_val.as_array()?;
            if funding_arr.is_empty() {
                return None;
            }
            let links = extract_funding_links(funding_arr);
            if links.is_empty() {
                return None;
            }
            Some(FundingEntry {
                full_name: p.name.clone(),
                links,
            })
        })
        .collect();

    Ok(entries)
}

// ─── Funding helpers ──────────────────────────────────────────────────────────

fn extract_funding_links(funding_json: &[serde_json::Value]) -> Vec<FundingLink> {
    funding_json
        .iter()
        .filter_map(|entry| {
            let url = entry.get("url").and_then(|v| v.as_str()).unwrap_or("");
            if url.is_empty() {
                return None;
            }
            let funding_type = entry
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Some(FundingLink {
                url: url.to_string(),
                funding_type,
            })
        })
        .collect()
}

fn rewrite_github_url(url: &str, funding_type: Option<&str>) -> String {
    if funding_type != Some("github") {
        return url.to_string();
    }
    // Match exactly https://github.com/{user} with no further path segments
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        // rest must be a single path segment (no '/')
        if !rest.is_empty() && !rest.contains('/') {
            return format!("https://github.com/sponsors/{}", rest);
        }
    }
    url.to_string()
}

fn group_by_vendor(entries: &[FundingEntry]) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();

    for entry in entries {
        // Split full_name into vendor and package parts
        let (vendor, package_name) = match entry.full_name.split_once('/') {
            Some((v, p)) => (v.to_string(), p.to_string()),
            None => (entry.full_name.clone(), entry.full_name.clone()),
        };

        let vendor_map = grouped.entry(vendor).or_default();

        for link in &entry.links {
            let url = rewrite_github_url(&link.url, link.funding_type.as_deref());
            vendor_map
                .entry(url)
                .or_default()
                .push(package_name.clone());
        }
    }

    grouped
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn render_text(grouped: &BTreeMap<String, BTreeMap<String, Vec<String>>>) {
    if grouped.is_empty() {
        println!(
            "No funding links were found in your package dependencies. \
             This doesn't mean they don't need your support!"
        );
        return;
    }

    println!(
        "The following packages were found in your dependencies which publish funding information:"
    );

    for (vendor, url_map) in grouped {
        println!();
        println!("{}", vendor);
        for (url, packages) in url_map {
            // Deduplicate consecutive identical entries and join with ", "
            let mut deduped: Vec<&str> = Vec::new();
            for pkg in packages {
                if deduped.last().copied() != Some(pkg.as_str()) {
                    deduped.push(pkg.as_str());
                }
            }
            println!("  {}", deduped.join(", "));
            println!("    {}", url);
        }
    }

    println!();
    println!("Please consider following these links and sponsoring the work of package authors!");
    println!("Thank you!");
}

fn render_json(grouped: &BTreeMap<String, BTreeMap<String, Vec<String>>>) -> anyhow::Result<()> {
    let buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
    grouped.serialize(&mut ser)?;
    println!("{}", String::from_utf8(ser.into_inner())?);
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper ────────────────────────────────────────────────────────────────

    fn make_funding_json(entries: &[(&str, &str)]) -> Vec<serde_json::Value> {
        entries
            .iter()
            .map(|(t, u)| serde_json::json!({"type": t, "url": u}))
            .collect()
    }

    // ── extract_funding_links ─────────────────────────────────────────────────

    #[test]
    fn test_extract_funding_links_basic() {
        let json = make_funding_json(&[("github", "https://github.com/Seldaek")]);
        let links = extract_funding_links(&json);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://github.com/Seldaek");
        assert_eq!(links[0].funding_type.as_deref(), Some("github"));
    }

    #[test]
    fn test_extract_funding_links_missing_url() {
        let json = vec![
            serde_json::json!({"type": "github", "url": ""}),
            serde_json::json!({"type": "tidelift"}),
            serde_json::json!({"type": "github", "url": "https://github.com/user"}),
        ];
        let links = extract_funding_links(&json);
        // Only the last entry has a non-empty url
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://github.com/user");
    }

    #[test]
    fn test_extract_funding_links_empty() {
        let links = extract_funding_links(&[]);
        assert!(links.is_empty());
    }

    // ── rewrite_github_url ────────────────────────────────────────────────────

    #[test]
    fn test_rewrite_github_url_profile() {
        let result = rewrite_github_url("https://github.com/Seldaek", Some("github"));
        assert_eq!(result, "https://github.com/sponsors/Seldaek");
    }

    #[test]
    fn test_rewrite_github_url_already_sponsors() {
        // Has a second path segment, so not rewritten
        let result = rewrite_github_url("https://github.com/sponsors/Seldaek", Some("github"));
        assert_eq!(result, "https://github.com/sponsors/Seldaek");
    }

    #[test]
    fn test_rewrite_github_url_non_github_type() {
        let result = rewrite_github_url("https://github.com/fabpot", Some("tidelift"));
        assert_eq!(result, "https://github.com/fabpot");
    }

    #[test]
    fn test_rewrite_github_url_deep_path() {
        // https://github.com/user/repo has a second path segment
        let result = rewrite_github_url("https://github.com/user/repo", Some("github"));
        assert_eq!(result, "https://github.com/user/repo");
    }

    // ── group_by_vendor ───────────────────────────────────────────────────────

    #[test]
    fn test_group_by_vendor_basic() {
        let entries = vec![
            FundingEntry {
                full_name: "symfony/console".to_string(),
                links: vec![FundingLink {
                    url: "https://github.com/fabpot".to_string(),
                    funding_type: Some("github".to_string()),
                }],
            },
            FundingEntry {
                full_name: "symfony/http-kernel".to_string(),
                links: vec![FundingLink {
                    url: "https://github.com/fabpot".to_string(),
                    funding_type: Some("github".to_string()),
                }],
            },
        ];

        let grouped = group_by_vendor(&entries);
        assert_eq!(grouped.len(), 1);
        let symfony = grouped.get("symfony").unwrap();
        // URL should be rewritten to sponsors
        let url = "https://github.com/sponsors/fabpot";
        let packages = symfony.get(url).unwrap();
        assert_eq!(packages.len(), 2);
        assert!(packages.contains(&"console".to_string()));
        assert!(packages.contains(&"http-kernel".to_string()));
    }

    #[test]
    fn test_group_by_vendor_multiple_urls() {
        let entries = vec![FundingEntry {
            full_name: "symfony/console".to_string(),
            links: vec![
                FundingLink {
                    url: "https://github.com/fabpot".to_string(),
                    funding_type: Some("github".to_string()),
                },
                FundingLink {
                    url: "https://tidelift.com/funding/github/packagist/symfony/symfony"
                        .to_string(),
                    funding_type: Some("tidelift".to_string()),
                },
            ],
        }];

        let grouped = group_by_vendor(&entries);
        let symfony = grouped.get("symfony").unwrap();
        assert_eq!(symfony.len(), 2);
        assert!(symfony.contains_key("https://github.com/sponsors/fabpot"));
        assert!(
            symfony.contains_key("https://tidelift.com/funding/github/packagist/symfony/symfony")
        );
    }

    #[test]
    fn test_group_by_vendor_empty() {
        let grouped = group_by_vendor(&[]);
        assert!(grouped.is_empty());
    }

    // ── Integration tests ─────────────────────────────────────────────────────

    #[test]
    fn test_fund_from_lockfile() {
        use mozart_registry::lockfile::{LockFile, LockedPackage};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        let lock = LockFile {
            readme: LockFile::default_readme(),
            content_hash: "abc123".to_string(),
            packages: vec![
                LockedPackage {
                    name: "monolog/monolog".to_string(),
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
                    funding: Some(vec![serde_json::json!({
                        "type": "github",
                        "url": "https://github.com/Seldaek"
                    })]),
                    time: None,
                    extra_fields: BTreeMap::new(),
                },
                LockedPackage {
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
                    funding: None, // no funding
                    time: None,
                    extra_fields: BTreeMap::new(),
                },
            ],
            packages_dev: Some(vec![]),
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

        let entries = collect_funding_from_locked(working_dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].full_name, "monolog/monolog");
        assert_eq!(entries[0].links.len(), 1);
        assert_eq!(entries[0].links[0].url, "https://github.com/Seldaek");
        assert_eq!(entries[0].links[0].funding_type.as_deref(), Some("github"));
    }

    #[test]
    fn test_fund_from_installed() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();
        let vendor_dir = working_dir.join("vendor");

        let mut installed = mozart_registry::installed::InstalledPackages::new();

        let mut extra = BTreeMap::new();
        extra.insert(
            "funding".to_string(),
            serde_json::json!([{
                "type": "github",
                "url": "https://github.com/Seldaek"
            }]),
        );
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
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

        // Package without funding
        installed.upsert(mozart_registry::installed::InstalledPackageEntry {
            name: "psr/log".to_string(),
            version: "3.0.0".to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: None,
            install_path: None,
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        });

        installed.write(&vendor_dir).unwrap();

        let entries = collect_funding_from_installed(working_dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].full_name, "monolog/monolog");
        assert_eq!(entries[0].links[0].url, "https://github.com/Seldaek");
    }

    #[test]
    fn test_fund_no_funding_data() {
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
            packages_dev: Some(vec![]),
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

        let entries = collect_funding_from_locked(working_dir).unwrap();
        assert!(entries.is_empty());

        let grouped = group_by_vendor(&entries);
        assert!(grouped.is_empty());
    }
}
