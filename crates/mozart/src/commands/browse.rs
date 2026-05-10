use crate::composer::Composer;
use clap::Args;
use mozart_core::console::IoInterface;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::exit_code;
use mozart_core::package::PackageInterface as _;
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

pub async fn execute(
    args: &BrowseArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let cache = Cache::repo(&build_cache_config(cli.no_cache));

    let composer = Composer::try_load(io.clone(), &working_dir)?;
    let repos = build_repos(composer.as_ref(), cache);

    let packages: Vec<String> = if args.packages.is_empty() {
        console_writeln_error!(
            io,
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
                if handle_package(&view, args.homepage, args.show, io.clone())? {
                    handled = true;
                    break 'outer;
                }
            }
        }

        if !package_exists {
            return_code = 1;
            console_writeln_error!(io, "<warning>Package {} not found</warning>", package_name,);
        }

        if !handled {
            return_code = 1;
            let kind = if args.homepage {
                "Invalid or missing homepage"
            } else {
                "Invalid or missing repository URL"
            };
            console_writeln_error!(io, "<warning>{} for {}</warning>", kind, package_name);
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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
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
        console_writeln!(io, "<info>{}</info>", url);
    } else {
        open_browser(&url, io)?;
    }

    Ok(true)
}

fn is_valid_url(url: &str) -> bool {
    url::Url::parse(url).is_ok()
}

fn open_browser(
    url: &str,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
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
                io,
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
