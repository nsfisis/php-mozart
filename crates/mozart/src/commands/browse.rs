use crate::composer::Composer;
use clap::Args;
use mozart_core::console::Console;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::exit_code;
use mozart_core::package::Package;
use mozart_core::repository::browse_repos::{BrowseRepos, CompletePackageView};
use mozart_core::repository::cache::{Cache, build_cache_config};
use mozart_core::repository::installed::InstalledPackages;
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

pub async fn execute(args: &BrowseArgs, cli: &super::Cli, console: &Console) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let cache = Cache::repo(&build_cache_config(cli.no_cache));

    let composer = Composer::try_load(&working_dir)?;
    let repos = build_repos(composer.as_ref(), cache);

    let packages: Vec<String> = if args.packages.is_empty() {
        console_writeln_error!(
            console,
            "No package specified, opening homepage for the root package"
        );
        // Mirrors HomeCommand's `$this->requireComposer()->getPackage()->getName()`.
        let composer = composer.ok_or_else(|| {
            anyhow::anyhow!(
                "Composer could not find a composer.json file in {}",
                working_dir.display()
            )
        })?;
        vec![composer.package().name().to_string()]
    } else {
        args.packages.clone()
    };

    let mut return_code = 0i32;
    for package_name in &packages {
        let mut handled = false;
        let mut package_exists = false;
        'outer: for repo in repos.iter() {
            for view in repo.find_packages(package_name).await? {
                package_exists = true;
                if handle_package(&view, args.homepage, args.show, console)? {
                    handled = true;
                    break 'outer;
                }
            }
        }

        if !package_exists {
            return_code = 1;
            console_writeln_error!(
                console,
                "<warning>Package {} not found</warning>",
                package_name,
            );
        }

        if !handled {
            return_code = 1;
            let kind = if args.homepage {
                "Invalid or missing homepage"
            } else {
                "Invalid or missing repository URL"
            };
            console_writeln_error!(console, "<warning>{} for {}</warning>", kind, package_name);
        }
    }

    if return_code != 0 {
        return Err(exit_code::bail_silent(return_code));
    }

    Ok(())
}

fn build_repos(composer: Option<&Composer>, cache: Cache) -> BrowseRepos {
    let (root, installed) = match composer {
        Some(c) => {
            let root = Some(c.package().clone());
            let installed = InstalledPackages::read(c.installation_manager().vendor_dir()).ok();
            (root, installed)
        }
        None => (None, None),
    };
    BrowseRepos::new(root, installed, cache)
}

/// Port of `HomeCommand::handlePackage`. Returns `true` on success
/// (URL printed or browser opened), `false` when no valid URL was
/// available — matching Composer's signal for the outer loop.
fn handle_package(
    view: &CompletePackageView,
    show_homepage: bool,
    show_only: bool,
    console: &Console,
) -> anyhow::Result<bool> {
    let mut url = view
        .support_source
        .clone()
        .or_else(|| view.source_url.clone());
    if url.is_none() || show_homepage {
        url = view.homepage.clone();
    }

    let Some(url) = url.filter(|u| is_valid_url(u)) else {
        return Ok(false);
    };

    if show_only {
        console_writeln!(console, "<info>{}</info>", url);
    } else {
        open_browser(&url, console)?;
    }
    Ok(true)
}

fn is_valid_url(url: &str) -> bool {
    url::Url::parse(url).is_ok()
}

fn open_browser(url: &str, console: &Console) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "\"web\"", "explorer", url])
            .status()?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let xdg_open = which("xdg-open");
        let open = which("open");
        if xdg_open {
            Command::new("xdg-open").arg(url).status()?;
        } else if open {
            Command::new("open").arg(url).status()?;
        } else {
            console_writeln_error!(
                console,
                "No suitable browser opening command found, open yourself: {}",
                url,
            );
        }
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn which(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console() -> Console {
        Console::new(0, false, false, false, true)
    }

    fn view(
        support: Option<&str>,
        source: Option<&str>,
        homepage: Option<&str>,
    ) -> CompletePackageView {
        CompletePackageView {
            support_source: support.map(str::to_string),
            source_url: source.map(str::to_string),
            homepage: homepage.map(str::to_string),
        }
    }

    #[test]
    fn is_valid_url_accepts_filter_var_compatible_schemes() {
        assert!(is_valid_url("https://example.com"));
        assert!(is_valid_url("http://example.com/path?query=1"));
        assert!(is_valid_url("ftp://example.com/a"));
    }

    #[test]
    fn is_valid_url_rejects_malformed() {
        assert!(!is_valid_url(""));
        assert!(!is_valid_url("not-a-url"));
        assert!(!is_valid_url("https://"));
    }

    #[test]
    fn handle_package_prefers_support_source() {
        let v = view(
            Some("https://github.com/vendor/pkg"),
            Some("https://github.com/vendor/pkg.git"),
            Some("https://vendor.example.com"),
        );
        assert!(handle_package(&v, false, true, &console()).unwrap());
    }

    #[test]
    fn handle_package_falls_back_to_source_url() {
        let v = view(
            None,
            Some("https://github.com/vendor/pkg.git"),
            Some("https://vendor.example.com"),
        );
        assert!(handle_package(&v, false, true, &console()).unwrap());
    }

    #[test]
    fn handle_package_falls_back_to_homepage_when_no_source() {
        let v = view(None, None, Some("https://vendor.example.com"));
        assert!(handle_package(&v, false, true, &console()).unwrap());
    }

    #[test]
    fn handle_package_show_homepage_overrides_to_homepage() {
        let v = view(
            Some("https://github.com/vendor/pkg"),
            Some("https://github.com/vendor/pkg.git"),
            Some("https://vendor.example.com"),
        );
        assert!(handle_package(&v, true, true, &console()).unwrap());
    }

    #[test]
    fn handle_package_returns_false_when_no_valid_url() {
        let v = view(None, None, None);
        assert!(!handle_package(&v, false, true, &console()).unwrap());

        // Invalid URL strings still cause `handlePackage` to bail.
        let bad = view(Some("not-a-url"), None, None);
        assert!(!handle_package(&bad, false, true, &console()).unwrap());
    }

    #[test]
    fn handle_package_show_homepage_with_missing_homepage_returns_false() {
        let v = view(Some("https://github.com/vendor/pkg"), None, None);
        // -H and homepage absent → falls through and bails.
        assert!(!handle_package(&v, true, true, &console()).unwrap());
    }
}
