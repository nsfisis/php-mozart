use crate::composer::Composer;
use clap::Args;
use mozart_core::console::{Console, hyperlink};
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_core::exit_code;
use mozart_core::repository::cache::{Cache, build_cache_config};
use mozart_core::repository::installed::InstalledPackages;
use mozart_core::repository::repository::{PackageQuery, RepositorySet};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Args)]
pub struct FundArgs {
    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

pub async fn execute(args: &FundArgs, cli: &super::Cli, console: &Console) -> anyhow::Result<()> {
    let format = args.format.as_deref().unwrap_or("text");
    if !matches!(format, "text" | "json") {
        console.error(&console_format!(
            "<error>Unsupported format \"{format}\". See help for supported formats.</error>"
        ));
        return Err(exit_code::bail_silent(exit_code::GENERAL_ERROR));
    }

    let working_dir = cli.working_dir()?;
    let composer = Composer::require(&working_dir)?;
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
        "json" => render_json(&fundings, console)?,
        _ => render_text(&fundings, console),
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

fn render_text(fundings: &BTreeMap<String, BTreeMap<String, Vec<String>>>, console: &Console) {
    if fundings.is_empty() {
        console_writeln!(
            console,
            "No funding links were found in your package dependencies. \
             This doesn't mean they don't need your support!",
        );
        return;
    }

    console_writeln!(
        console,
        "The following packages were found in your dependencies which publish funding information:",
    );

    let mut prev: Option<String> = None;
    for (vendor, url_map) in fundings {
        console_writeln!(console, "");
        console_writeln!(console, "<comment>{vendor}</comment>");
        for (url, packages) in url_map {
            let line = format!("  <info>{}</info>", packages.join(", "));
            if prev.as_deref() != Some(line.as_str()) {
                console_writeln!(console, "{line}");
                prev = Some(line);
            }
            let link = hyperlink(url, url, console.decorated);
            console_writeln!(console, "    {link}");
        }
    }

    console_writeln!(console, "");
    console_writeln!(
        console,
        "Please consider following these links and sponsoring the work of package authors!",
    );
    console_writeln!(console, "Thank you!");
}

fn render_json(
    fundings: &BTreeMap<String, BTreeMap<String, Vec<String>>>,
    console: &Console,
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
    console_writeln!(console, "{}", &String::from_utf8(ser.into_inner())?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_funding_json(entries: &[(&str, &str)]) -> Vec<serde_json::Value> {
        entries
            .iter()
            .map(|(t, u)| serde_json::json!({"type": t, "url": u}))
            .collect()
    }

    #[test]
    fn insert_funding_data_basic() {
        let mut fundings = BTreeMap::new();
        let funding = make_funding_json(&[("github", "https://github.com/Seldaek")]);
        insert_funding_data(&mut fundings, "monolog/monolog", &funding);

        let monolog = fundings.get("monolog").unwrap();
        let url = "https://github.com/sponsors/Seldaek";
        let packages = monolog.get(url).unwrap();
        assert_eq!(packages, &vec!["monolog".to_string()]);
    }

    #[test]
    fn insert_funding_data_skips_empty_url() {
        let mut fundings = BTreeMap::new();
        let funding = vec![
            serde_json::json!({"type": "github", "url": ""}),
            serde_json::json!({"type": "tidelift"}),
            serde_json::json!({"type": "github", "url": "https://github.com/user"}),
        ];
        insert_funding_data(&mut fundings, "vendor/pkg", &funding);

        let vendor = fundings.get("vendor").unwrap();
        assert_eq!(vendor.len(), 1);
        assert!(vendor.contains_key("https://github.com/sponsors/user"));
    }

    #[test]
    fn insert_funding_data_skips_malformed_pretty_name() {
        let mut fundings = BTreeMap::new();
        let funding = make_funding_json(&[("github", "https://github.com/user")]);
        insert_funding_data(&mut fundings, "no-slash-name", &funding);
        assert!(fundings.is_empty());
    }

    #[test]
    fn insert_funding_data_groups_by_vendor() {
        let mut fundings = BTreeMap::new();
        let funding = make_funding_json(&[("github", "https://github.com/fabpot")]);
        insert_funding_data(&mut fundings, "symfony/console", &funding);
        insert_funding_data(&mut fundings, "symfony/http-kernel", &funding);

        let symfony = fundings.get("symfony").unwrap();
        let url = "https://github.com/sponsors/fabpot";
        let packages = symfony.get(url).unwrap();
        assert_eq!(packages.len(), 2);
        assert!(packages.contains(&"console".to_string()));
        assert!(packages.contains(&"http-kernel".to_string()));
    }

    #[test]
    fn insert_funding_data_multiple_urls() {
        let mut fundings = BTreeMap::new();
        let funding = vec![
            serde_json::json!({"type": "github", "url": "https://github.com/fabpot"}),
            serde_json::json!({
                "type": "tidelift",
                "url": "https://tidelift.com/funding/github/packagist/symfony/symfony"
            }),
        ];
        insert_funding_data(&mut fundings, "symfony/console", &funding);

        let symfony = fundings.get("symfony").unwrap();
        assert_eq!(symfony.len(), 2);
        assert!(symfony.contains_key("https://github.com/sponsors/fabpot"));
        assert!(
            symfony.contains_key("https://tidelift.com/funding/github/packagist/symfony/symfony")
        );
    }

    #[test]
    fn rewrite_github_url_profile() {
        let result = rewrite_github_url("https://github.com/Seldaek", Some("github"));
        assert_eq!(result, "https://github.com/sponsors/Seldaek");
    }

    #[test]
    fn rewrite_github_url_already_sponsors() {
        let result = rewrite_github_url("https://github.com/sponsors/Seldaek", Some("github"));
        assert_eq!(result, "https://github.com/sponsors/Seldaek");
    }

    #[test]
    fn rewrite_github_url_non_github_type() {
        let result = rewrite_github_url("https://github.com/fabpot", Some("tidelift"));
        assert_eq!(result, "https://github.com/fabpot");
    }

    #[test]
    fn rewrite_github_url_deep_path() {
        let result = rewrite_github_url("https://github.com/user/repo", Some("github"));
        assert_eq!(result, "https://github.com/user/repo");
    }

    #[test]
    fn rewrite_github_url_missing_type() {
        let result = rewrite_github_url("https://github.com/user", None);
        assert_eq!(result, "https://github.com/user");
    }

    #[test]
    fn render_json_empty_emits_array() {
        // Composer's `JsonFile::encode([])` emits `[]`; ensure Mozart matches
        // rather than serializing the empty BTreeMap to `{}`.
        let console = Console::new(0, false, false, false, true);
        let fundings: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();

        let buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
        let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
        if fundings.is_empty() {
            let empty: Vec<()> = Vec::new();
            empty.serialize(&mut ser).unwrap();
        } else {
            fundings.serialize(&mut ser).unwrap();
        }
        let out = String::from_utf8(ser.into_inner()).unwrap();
        assert_eq!(out, "[]");
        let _ = console;
    }

    #[test]
    fn render_json_non_empty_is_object() {
        let mut fundings: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        let funding = make_funding_json(&[("github", "https://github.com/Seldaek")]);
        insert_funding_data(&mut fundings, "monolog/monolog", &funding);

        let buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
        let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
        fundings.serialize(&mut ser).unwrap();
        let out = String::from_utf8(ser.into_inner()).unwrap();
        assert!(out.starts_with('{'));
        assert!(out.contains("monolog"));
        assert!(out.contains("https://github.com/sponsors/Seldaek"));
    }
}
