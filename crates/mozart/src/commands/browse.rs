use clap::Args;
use mozart_core::console_format;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct BrowseArgs {
    /// Package(s) to browse
    pub packages: Vec<String>,

    /// Open the homepage instead of the repository URL
    #[arg(short = 'H', long)]
    pub homepage: bool,

    /// Only show the homepage or repository URL
    #[arg(short, long)]
    pub show: bool,
}

// ─── Main entry point ────────────────────────────────────────────────────────

pub async fn execute(
    args: &BrowseArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    // If no packages specified, use root package name from composer.json
    let packages: Vec<String> = if args.packages.is_empty() {
        let composer_json = working_dir.join("composer.json");
        if !composer_json.exists() {
            anyhow::bail!(
                "No composer.json found in the current directory and no package specified."
            );
        }
        eprintln!("No package specified, opening homepage for the root package");
        let root = mozart_core::package::read_from_file(&composer_json)?;
        vec![root.name.clone()]
    } else {
        args.packages.clone()
    };

    let mut exit_code = 0i32;

    for package_name in &packages {
        match resolve_url(package_name, &working_dir, args.homepage).await? {
            ResolveResult::Found(url) => {
                if args.show {
                    console.write_stdout(
                        &console_format!("<info>{}</info>", url),
                        mozart_core::console::Verbosity::Normal,
                    );
                } else {
                    open_browser(&url)?;
                }
            }
            ResolveResult::NotFound => {
                eprintln!(
                    "{}",
                    console_format!("<warning>Package {} not found</warning>", package_name)
                );
                exit_code = 1;
            }
            ResolveResult::NoUrl => {
                let msg = if args.homepage {
                    format!("Invalid or missing homepage for {}", package_name)
                } else {
                    format!("Invalid or missing repository URL for {}", package_name)
                };
                eprintln!("{}", console_format!("<warning>{}</warning>", msg));
                exit_code = 1;
            }
        }
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}

// ─── URL resolution ───────────────────────────────────────────────────────────

enum ResolveResult {
    /// Package found and URL resolved
    Found(String),
    /// Package found but no valid URL available
    NoUrl,
    /// Package not found in any source
    NotFound,
}

async fn resolve_url(
    package_name: &str,
    working_dir: &Path,
    prefer_homepage: bool,
) -> anyhow::Result<ResolveResult> {
    // 1. Check root package (composer.json)
    let composer_json = working_dir.join("composer.json");
    if composer_json.exists()
        && let Ok(root) = mozart_core::package::read_from_file(&composer_json)
        && root.name.eq_ignore_ascii_case(package_name)
    {
        return Ok(match extract_url_from_root(&root, prefer_homepage) {
            Some(url) => ResolveResult::Found(url),
            None => ResolveResult::NoUrl,
        });
    }

    // 2. Check lock file (composer.lock)
    let lock_path = working_dir.join("composer.lock");
    if lock_path.exists()
        && let Ok(lock) = mozart_registry::lockfile::LockFile::read_from_file(&lock_path)
    {
        let all_packages = lock
            .packages
            .iter()
            .chain(lock.packages_dev.as_deref().unwrap_or(&[]));

        for pkg in all_packages {
            if pkg.name.eq_ignore_ascii_case(package_name) {
                return Ok(match extract_url_from_locked(pkg, prefer_homepage) {
                    Some(url) => ResolveResult::Found(url),
                    None => ResolveResult::NoUrl,
                });
            }
        }
    }

    // 3. Fall back to Packagist API
    match mozart_registry::packagist::fetch_package_versions(package_name, None).await {
        Ok(versions) if !versions.is_empty() => {
            // Find the latest stable version (first non-dev, or fallback to first)
            let best = versions
                .iter()
                .find(|v| !v.version.starts_with("dev-") && !v.version.ends_with("-dev"))
                .or_else(|| versions.first());

            if let Some(version) = best {
                return Ok(match extract_url_from_packagist(version, prefer_homepage) {
                    Some(url) => ResolveResult::Found(url),
                    None => ResolveResult::NoUrl,
                });
            }
            Ok(ResolveResult::NotFound)
        }
        _ => Ok(ResolveResult::NotFound),
    }
}

// ─── URL extraction ───────────────────────────────────────────────────────────

fn extract_url_from_locked(
    pkg: &mozart_registry::lockfile::LockedPackage,
    prefer_homepage: bool,
) -> Option<String> {
    if prefer_homepage {
        return pkg
            .homepage
            .as_deref()
            .filter(|u| is_valid_url(u))
            .map(|u| u.to_string());
    }

    // Priority: support.source → source.url → homepage
    if let Some(ref support) = pkg.support
        && let Some(source_url) = support.get("source").and_then(|v| v.as_str())
        && is_valid_url(source_url)
    {
        return Some(source_url.to_string());
    }

    if let Some(ref source) = pkg.source
        && is_valid_url(&source.url)
    {
        return Some(source.url.clone());
    }

    pkg.homepage
        .as_deref()
        .filter(|u| is_valid_url(u))
        .map(|u| u.to_string())
}

fn extract_url_from_root(
    root: &mozart_core::package::RawPackageData,
    prefer_homepage: bool,
) -> Option<String> {
    if prefer_homepage {
        return root
            .homepage
            .as_deref()
            .filter(|u| is_valid_url(u))
            .map(|u| u.to_string());
    }

    // Priority: support.source → homepage (no source.url in RawPackageData)
    if let Some(support_val) = root.extra_fields.get("support")
        && let Some(source_url) = support_val.get("source").and_then(|v| v.as_str())
        && is_valid_url(source_url)
    {
        return Some(source_url.to_string());
    }

    root.homepage
        .as_deref()
        .filter(|u| is_valid_url(u))
        .map(|u| u.to_string())
}

fn extract_url_from_packagist(
    pkg: &mozart_registry::packagist::PackagistVersion,
    prefer_homepage: bool,
) -> Option<String> {
    if prefer_homepage {
        return pkg
            .homepage
            .as_deref()
            .filter(|u| is_valid_url(u))
            .map(|u| u.to_string());
    }

    // Priority: support.source → source.url → homepage
    if let Some(ref support) = pkg.support
        && let Some(source_url) = support.get("source").and_then(|v| v.as_str())
        && is_valid_url(source_url)
    {
        return Some(source_url.to_string());
    }

    if let Some(ref source) = pkg.source
        && is_valid_url(&source.url)
    {
        return Some(source.url.clone());
    }

    pkg.homepage
        .as_deref()
        .filter(|u| is_valid_url(u))
        .map(|u| u.to_string())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn is_valid_url(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https"),
        Err(_) => false,
    }
}

fn open_browser(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).status()?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "web", "explorer", url])
            .status()?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        if Command::new("which")
            .arg("xdg-open")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            Command::new("xdg-open").arg(url).status()?;
            return Ok(());
        }
        if Command::new("which")
            .arg("open")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            Command::new("open").arg(url).status()?;
            return Ok(());
        }
        eprintln!(
            "No suitable browser opener found. Please open manually: {}",
            url
        );
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_locked_package(
        source_url: Option<&str>,
        homepage: Option<&str>,
        support_source: Option<&str>,
    ) -> mozart_registry::lockfile::LockedPackage {
        let support = support_source.map(|s| serde_json::json!({"source": s}));
        let source = source_url.map(|url| mozart_registry::lockfile::LockedSource {
            source_type: "git".to_string(),
            url: url.to_string(),
            reference: None,
        });
        mozart_registry::lockfile::LockedPackage {
            name: "vendor/package".to_string(),
            version: "1.0.0".to_string(),
            version_normalized: None,
            source,
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
            homepage: homepage.map(|s| s.to_string()),
            keywords: None,
            authors: None,
            support,
            funding: None,
            time: None,
            extra_fields: BTreeMap::new(),
        }
    }

    // ── is_valid_url ──────────────────────────────────────────────────────────

    #[test]
    fn test_is_valid_url() {
        assert!(!is_valid_url("https://"));
        assert!(is_valid_url("https://example.com"));
        assert!(is_valid_url("http://example.com/path?query=1"));
        assert!(!is_valid_url("ftp://example.com"));
        assert!(!is_valid_url("not-a-url"));
        assert!(!is_valid_url(""));
    }

    // ── extract_url_from_locked ───────────────────────────────────────────────

    #[test]
    fn test_extract_url_from_locked_prefers_support_source() {
        // Has all three: support.source should win
        let pkg = make_locked_package(
            Some("https://github.com/vendor/package.git"),
            Some("https://vendor.example.com"),
            Some("https://github.com/vendor/package"),
        );
        let url = extract_url_from_locked(&pkg, false);
        assert_eq!(url, Some("https://github.com/vendor/package".to_string()));
    }

    #[test]
    fn test_extract_url_from_locked_prefers_homepage() {
        // With prefer_homepage=true, only homepage is returned
        let pkg = make_locked_package(
            Some("https://github.com/vendor/package.git"),
            Some("https://vendor.example.com"),
            Some("https://github.com/vendor/package"),
        );
        let url = extract_url_from_locked(&pkg, true);
        assert_eq!(url, Some("https://vendor.example.com".to_string()));
    }

    #[test]
    fn test_extract_url_from_locked_fallback_to_source() {
        // No support.source, has source.url
        let pkg = make_locked_package(
            Some("https://github.com/vendor/package.git"),
            Some("https://vendor.example.com"),
            None,
        );
        let url = extract_url_from_locked(&pkg, false);
        assert_eq!(
            url,
            Some("https://github.com/vendor/package.git".to_string())
        );
    }

    #[test]
    fn test_extract_url_from_locked_fallback_to_homepage() {
        // No source URLs, falls back to homepage
        let pkg = make_locked_package(None, Some("https://vendor.example.com"), None);
        let url = extract_url_from_locked(&pkg, false);
        assert_eq!(url, Some("https://vendor.example.com".to_string()));
    }

    #[test]
    fn test_extract_url_from_locked_no_urls() {
        // No URLs at all
        let pkg = make_locked_package(None, None, None);
        let url = extract_url_from_locked(&pkg, false);
        assert_eq!(url, None);
    }
}
