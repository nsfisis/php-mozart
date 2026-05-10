use crate::composer::Composer;
use clap::Args;
use mozart_core::console::{IoInterface, hyperlink};
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_core::exit_code;
use mozart_core::repository::cache::{Cache, build_cache_config};
use mozart_core::repository::installed::InstalledPackages;
use mozart_core::repository::repository::{PackageQuery, RepositorySet};
use serde::Serialize as _;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Args)]
pub struct FundArgs {
    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

pub async fn execute(
    args: &FundArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let format = args.format.as_deref().unwrap_or("text");
    if !matches!(format, "text" | "json") {
        io.lock().unwrap().error(&console_format!(
            "<error>Unsupported format \"{format}\". See help for supported formats.</error>"
        ));
        return Err(exit_code::bail_silent(exit_code::GENERAL_ERROR));
    }

    let working_dir = cli.working_dir()?;
    let composer = Composer::require(io.clone(), &working_dir)?;
    let installed = InstalledPackages::read(composer.installation_manager().vendor_dir())?;

    // Configured remote repositories from `composer.json` are not yet wired
    // up; this matches the known divergence already present in
    // `commands/search.rs` and Composer's full `CompositeRepository`.
    let repo_cache = Cache::repo(&build_cache_config(cli.no_cache));
    let remote_repos = RepositorySet::with_packagist(repo_cache);

    let mut packages_to_load: BTreeSet<String> =
        installed.packages.iter().map(|p| p.name.clone()).collect();

    let mut fundings: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();

    // Pass 1: load default-branch metadata from remote repos and pull funding
    // info from there first. Mirrors `FundCommand::execute` L60-74. Composer
    // passes `['dev' => STABILITY_DEV]` so default-branch versions are
    // returned; Mozart's repo layer does not filter by stability, so an
    // unconstrained query yields them naturally.
    if !packages_to_load.is_empty() {
        let queries: Vec<PackageQuery<'_>> = packages_to_load
            .iter()
            .map(|n| PackageQuery {
                name: n.as_str(),
                constraint: None,
            })
            .collect();
        let result = remote_repos.load_packages(&queries).await?;

        for named in &result {
            if !named.version.default_branch {
                continue;
            }
            let Some(funding) = named.version.funding.as_deref() else {
                continue;
            };
            if funding.is_empty() {
                continue;
            }
            insert_funding_data(&mut fundings, &named.name, funding);
            packages_to_load.remove(&named.name);
        }
    }

    // Pass 2: fall back to installed-package funding for names whose default
    // branch had nothing. Mirrors `FundCommand::execute` L77-85.
    for installed_pkg in &installed.packages {
        if !packages_to_load.contains(&installed_pkg.name) {
            continue;
        }
        let Some(funding_val) = installed_pkg.extra_fields.get("funding") else {
            continue;
        };
        let Some(funding) = funding_val.as_array() else {
            continue;
        };
        if funding.is_empty() {
            continue;
        }
        insert_funding_data(&mut fundings, &installed_pkg.name, funding);
    }

    // BTreeMap iteration is alphabetical — covers `ksort($fundings)`.

    match format {
        "json" => render_json(&fundings, io.clone())?,
        _ => render_text(&fundings, io.clone()),
    }

    Ok(())
}

/// Mirror of `FundCommand::insertFundingData`. Splits the package name on
/// `/`, applies the GitHub profile-to-sponsors rewrite, and appends the
/// package onto `fundings[vendor][url]`.
fn insert_funding_data(
    fundings: &mut BTreeMap<String, BTreeMap<String, Vec<String>>>,
    pretty_name: &str,
    funding: &[serde_json::Value],
) {
    let Some((vendor, package_name)) = pretty_name.split_once('/') else {
        return;
    };
    for entry in funding {
        let url = entry.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            continue;
        }
        let funding_type = entry.get("type").and_then(|v| v.as_str());
        let url = rewrite_github_url(url, funding_type);
        fundings
            .entry(vendor.to_string())
            .or_default()
            .entry(url)
            .or_default()
            .push(package_name.to_string());
    }
}

fn rewrite_github_url(url: &str, funding_type: Option<&str>) -> String {
    if funding_type != Some("github") {
        return url.to_string();
    }
    if let Some(rest) = url.strip_prefix("https://github.com/")
        && !rest.is_empty()
        && !rest.contains('/')
    {
        return format!("https://github.com/sponsors/{rest}");
    }
    url.to_string()
}

fn render_text(
    fundings: &BTreeMap<String, BTreeMap<String, Vec<String>>>,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) {
    if fundings.is_empty() {
        console_writeln!(
            io,
            "No funding links were found in your package dependencies. \
             This doesn't mean they don't need your support!",
        );
        return;
    }

    console_writeln!(
        io,
        "The following packages were found in your dependencies which publish funding information:",
    );

    let mut prev: Option<String> = None;
    for (vendor, url_map) in fundings {
        console_writeln!(io, "");
        console_writeln!(io, "<comment>{vendor}</comment>");
        for (url, packages) in url_map {
            let line = format!("  <info>{}</info>", packages.join(", "));
            if prev.as_deref() != Some(line.as_str()) {
                console_writeln!(io, "{line}");
                prev = Some(line);
            }
            let link = hyperlink(url, url, io.lock().unwrap().is_decorated());
            console_writeln!(io, "    {link}");
        }
    }

    console_writeln!(io, "");
    console_writeln!(
        io,
        "Please consider following these links and sponsoring the work of package authors!",
    );
    console_writeln!(io, "Thank you!");
}

fn render_json(
    fundings: &BTreeMap<String, BTreeMap<String, Vec<String>>>,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
    if fundings.is_empty() {
        // Composer's `JsonFile::encode([])` emits `[]` (PHP `json_encode` of
        // an empty native array). Mozart's empty `BTreeMap` would emit `{}`.
        let empty: Vec<()> = Vec::new();
        empty.serialize(&mut ser)?;
    } else {
        fundings.serialize(&mut ser)?;
    }
    console_writeln!(io, "{}", &String::from_utf8(ser.into_inner())?);
    Ok(())
}
