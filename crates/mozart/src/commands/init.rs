use anyhow::{Context, bail};
use clap::Args;
use colored::Colorize;
use mozart_core::console;
use mozart_core::console_format;
use mozart_core::package::{
    self, RawAuthor, RawAutoload, RawPackageData, RawRepository, Stability,
};
use mozart_core::validation;
use mozart_registry::{packagist, version};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct InitArgs {
    /// Name of the package (vendor/name)
    #[arg(long)]
    pub name: Option<String>,

    /// Description of the package
    #[arg(long)]
    pub description: Option<String>,

    /// Author name of the package
    #[arg(long)]
    pub author: Option<String>,

    /// Type of the package
    #[arg(long, value_name = "TYPE")]
    pub r#type: Option<String>,

    /// Homepage of the package
    #[arg(long)]
    pub homepage: Option<String>,

    /// Package(s) to require
    #[arg(long)]
    pub require: Vec<String>,

    /// Package(s) to require for development
    #[arg(long)]
    pub require_dev: Vec<String>,

    /// Minimum stability (stable, RC, beta, alpha, dev)
    #[arg(short, long)]
    pub stability: Option<String>,

    /// License of the package
    #[arg(short, long)]
    pub license: Option<String>,

    /// Add a custom repository
    #[arg(long)]
    pub repository: Vec<String>,

    /// Define a PSR-4 autoload namespace
    #[arg(short, long)]
    pub autoload: Option<String>,
}

pub async fn execute(
    args: &InitArgs,
    cli: &super::Cli,
    console: &console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().context("Failed to get current directory")?,
    };

    let composer_file = working_dir.join("composer.json");
    if composer_file.exists() {
        bail!("composer.json already exists in {}", working_dir.display());
    }

    // Validate --name if provided via CLI
    if let Some(ref name) = args.name
        && !validation::validate_package_name(name)
    {
        bail!(
            "The package name {name} is invalid, it should be lowercase and have a vendor name, a forward slash, and a package name, matching: [a-z0-9_.-]+/[a-z0-9_.-]+"
        );
    }

    let composer = if console.interactive {
        build_interactive(args, console, &working_dir).await?
    } else {
        build_non_interactive(args, &working_dir)?
    };

    let json = package::to_json_pretty(&composer)?;

    if console.interactive {
        console.info("");
        console.info(&json);
        console.info("");

        if !console.confirm(&console_format!(
            "Do you confirm generation [<comment>yes</comment>]?"
        )) {
            console.error("Command aborted");
            bail!("Command aborted");
        }
    } else {
        console.info(&format!("Writing {}", composer_file.display()));
    }

    package::write_to_file(&composer, &composer_file).context("Failed to write composer.json")?;

    // Create autoload directory if specified
    if let Some(ref autoload) = composer.autoload {
        for path in autoload.psr4.values() {
            let dir = working_dir.join(path);
            if !dir.exists() {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("Failed to create directory {}", dir.display()))?;
            }
        }
    }

    // Offer to add /vendor/ to .gitignore
    if console.interactive && working_dir.join(".git").is_dir() {
        let gitignore_path = working_dir.join(".gitignore");
        if !has_vendor_ignore(&gitignore_path)
            && console.confirm(&console_format!(
                "Would you like the <info>vendor</info> directory added to your <info>.gitignore</info> [<comment>yes</comment>]?"
            ))
        {
            add_vendor_ignore(&gitignore_path)?;
        }
    }

    // Show autoload info
    if let Some(ref autoload) = composer.autoload
        && let Some((ns, path)) = autoload.psr4.iter().next()
    {
        console.info(&console_format!(
            "PSR-4 autoloading configured. Use \"<comment>namespace {ns};</comment>\" in {path}"
        ));
        console.info(&console_format!(
            "Include the Composer autoloader with: <comment>require 'vendor/autoload.php';</comment>"
        ));
    }

    Ok(())
}

fn build_non_interactive(args: &InitArgs, working_dir: &Path) -> anyhow::Result<RawPackageData> {
    let name = match &args.name {
        Some(n) => n.clone(),
        None => get_default_package_name(working_dir),
    };

    let mut composer = RawPackageData::new(name.clone());
    composer.description = args.description.clone();
    composer.package_type = args.r#type.clone();
    composer.homepage = args.homepage.clone();
    let resolved_license = args
        .license
        .clone()
        .or_else(|| std::env::var("COMPOSER_DEFAULT_LICENSE").ok());
    if let Some(ref license) = resolved_license
        && !validation::validate_license(license)
        && !license.eq_ignore_ascii_case("proprietary")
    {
        bail!(
            "Invalid license provided: {license}. Only SPDX license identifiers (https://spdx.org/licenses/) or \"proprietary\" are accepted."
        );
    }
    composer.license = resolved_license;

    if let Some(ref stability) = args.stability {
        if !validation::validate_stability(stability) {
            bail!(
                "Invalid minimum stability \"{stability}\". Must be one of: dev, alpha, beta, rc, stable"
            );
        }
        composer.minimum_stability = Some(stability.to_lowercase());
    }

    let author_str = args.author.clone().or_else(get_default_author);
    if let Some(ref a) = author_str {
        let parsed = validation::parse_author(a).map_err(|e| anyhow::anyhow!(e))?;
        composer.authors = vec![RawAuthor {
            name: parsed.name,
            email: parsed.email,
        }];
    }

    composer.require = parse_requirements(&args.require)?;
    composer.require_dev = parse_requirements(&args.require_dev)?;
    composer.repositories = parse_repositories(&args.repository)?;

    if let Some(ref autoload_path) = args.autoload {
        composer.autoload = build_autoload(autoload_path, &name);
    }

    Ok(composer)
}

async fn build_interactive(
    args: &InitArgs,
    console: &console::Console,
    working_dir: &Path,
) -> anyhow::Result<RawPackageData> {
    console.info("");
    console.info(&format!(
        "  {}  ",
        "Welcome to the Mozart config generator".white().on_blue()
    ));
    console.info("");
    console.info("This command will guide you through creating your composer.json config.");
    console.info("");

    // Package name
    let default_name = args
        .name
        .clone()
        .unwrap_or_else(|| get_default_package_name(working_dir));
    let name = console.ask_validated(
        &console_format!(
            "Package name (<vendor>/<name>) [<comment>{}</comment>]",
            &default_name,
        ),
        &default_name,
        |val| {
            if validation::validate_package_name(val) {
                Ok(())
            } else {
                Err(format!(
                    "The package name {val} is invalid, it should be lowercase and have a vendor name, a forward slash, and a package name"
                ))
            }
        },
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    // Description
    let default_desc = args.description.clone().unwrap_or_default();
    let description = console.ask(
        &console_format!("Description [<comment>{}</comment>]", &default_desc),
        &default_desc,
    );
    let description = if description.is_empty() {
        None
    } else {
        Some(description)
    };

    // Author
    let default_author = args
        .author
        .clone()
        .or_else(get_default_author)
        .unwrap_or_default();
    let author_input = console.ask(
        &if !default_author.is_empty() {
            console_format!("Author [<comment>{}</comment>, n to skip]", &default_author)
        } else {
            "Author [n to skip]".to_string()
        },
        &default_author,
    );
    let authors = if author_input == "n" || author_input == "no" || author_input.is_empty() {
        Vec::new()
    } else {
        match validation::parse_author(&author_input) {
            Ok(parsed) => vec![RawAuthor {
                name: parsed.name,
                email: parsed.email,
            }],
            Err(_) => Vec::new(),
        }
    };

    // Minimum Stability
    let default_stability = args.stability.clone().unwrap_or_default();
    let stability_input = console.ask(
        &console_format!(
            "Minimum Stability [<comment>{}</comment>]",
            &default_stability
        ),
        &default_stability,
    );
    let minimum_stability = if stability_input.is_empty() {
        None
    } else if validation::validate_stability(&stability_input) {
        Some(stability_input.to_lowercase())
    } else {
        console.error(&format!(
            "Invalid minimum stability \"{stability_input}\". Using empty."
        ));
        None
    };

    // Package Type
    let default_type = args.r#type.clone().unwrap_or_default();
    let type_input = console.ask(
        &console_format!(
            "Package Type (e.g. library, project, metapackage, composer-plugin) [<comment>{}</comment>]",
            &default_type,
        ),
        &default_type,
    );
    let package_type = if type_input.is_empty() {
        None
    } else {
        Some(type_input)
    };

    // License
    let default_license = args
        .license
        .clone()
        .or_else(|| std::env::var("COMPOSER_DEFAULT_LICENSE").ok())
        .unwrap_or_default();
    let license = loop {
        let license_input = console.ask(
            &console_format!("License [<comment>{}</comment>]", &default_license),
            &default_license,
        );
        if license_input.is_empty() {
            break None;
        } else if validation::validate_license(&license_input)
            || license_input.eq_ignore_ascii_case("proprietary")
        {
            break Some(license_input);
        } else {
            console.error(&format!(
                "Invalid license provided: {license_input}. Only SPDX license identifiers (https://spdx.org/licenses/) or \"proprietary\" are accepted."
            ));
        }
    };

    // Dependencies
    let preferred_stability = minimum_stability
        .as_deref()
        .map(Stability::parse)
        .unwrap_or(Stability::Stable);

    console.info("");
    console.info(&console_format!("<info>Define your dependencies.</info>"));
    console.info("");

    let mut require = parse_requirements(&args.require)?;
    let interactive_require =
        interactive_search_packages("require", &require, preferred_stability).await?;
    for (name, constraint) in interactive_require {
        require.insert(name, constraint);
    }

    // Dev Dependencies
    console.info("");
    console.info(&console_format!(
        "<info>Define your dev dependencies.</info>"
    ));
    console.info("");

    let mut require_dev = parse_requirements(&args.require_dev)?;
    let all_required: BTreeMap<String, String> = require
        .iter()
        .chain(require_dev.iter())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let interactive_dev =
        interactive_search_packages("require-dev", &all_required, preferred_stability).await?;
    for (name, constraint) in interactive_dev {
        require_dev.insert(name, constraint);
    }

    // PSR-4 Autoload
    let default_autoload = args.autoload.clone().unwrap_or_else(|| "src/".to_string());
    let namespace = validation::namespace_from_package_name(&name).unwrap_or_default();
    let autoload_input = console.ask(
        &console_format!(
            "Add PSR-4 autoload mapping? Maps namespace \"{namespace}\" to the entered relative path. [<comment>{}</comment>, n to skip]",
            &default_autoload,
        ),
        &default_autoload,
    );
    let autoload = if autoload_input == "n" || autoload_input == "no" {
        None
    } else {
        let path = if autoload_input.is_empty() {
            default_autoload
        } else {
            autoload_input
        };
        build_autoload(&path, &name)
    };

    let repositories = parse_repositories(&args.repository)?;

    let mut composer = RawPackageData::new(name);
    composer.description = description;
    composer.package_type = package_type;
    composer.homepage = args.homepage.clone();
    composer.license = license;
    composer.authors = authors;
    composer.minimum_stability = minimum_stability;
    composer.require = require;
    composer.require_dev = require_dev;
    composer.repositories = repositories;
    composer.autoload = autoload;

    Ok(composer)
}

/// Interactive search-and-pick loop for dependencies.
///
/// Returns a map of package name → version constraint selected by the user.
async fn interactive_search_packages(
    label: &str,
    already_required: &BTreeMap<String, String>,
    preferred_stability: Stability,
) -> anyhow::Result<BTreeMap<String, String>> {
    let stdin = std::io::stdin();
    let mut selected: BTreeMap<String, String> = BTreeMap::new();

    loop {
        eprint!("Search for a package to {label} (or leave blank to skip): ");
        let _ = std::io::stderr().flush();

        let query = {
            let stdin_locked = stdin.lock();
            let mut lines = stdin_locked.lines();
            match lines.next() {
                Some(Ok(line)) => line.trim().to_string(),
                _ => break,
            }
        };

        if query.is_empty() {
            break;
        }

        // Search Packagist
        let (results, total) = match packagist::search_packages(&query, None).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "{}",
                    console_format!("<warning>Search failed: {e}. Try again.</warning>")
                );
                continue;
            }
        };

        // Filter out packages already required
        let filtered: Vec<&packagist::SearchResult> = results
            .iter()
            .filter(|r| {
                let name = r.name.to_lowercase();
                !already_required.contains_key(&name) && !selected.contains_key(&name)
            })
            .take(15)
            .collect();

        if filtered.is_empty() {
            eprintln!(
                "{}",
                console_format!(
                    "<warning>No new packages found for \"{query}\" (total: {total}).</warning>"
                )
            );
            continue;
        }

        eprintln!(
            "\nFound {} package{} for \"{}\":",
            filtered.len(),
            if filtered.len() == 1 { "" } else { "s" },
            query,
        );

        let name_width = filtered.iter().map(|r| r.name.len()).max().unwrap_or(0);
        for (idx, result) in filtered.iter().enumerate() {
            let desc = if result.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", result.description)
            };
            eprintln!(
                "  [{idx}] {:<width$}{desc}",
                result.name,
                idx = idx + 1,
                width = name_width,
            );
        }
        eprintln!("  [0] Search again / enter full package name");
        eprintln!();

        // Ask user to pick
        eprint!("Enter package # or name (leave empty to finish): ");
        let _ = std::io::stderr().flush();

        let choice = {
            let stdin_locked = stdin.lock();
            let mut lines = stdin_locked.lines();
            match lines.next() {
                Some(Ok(line)) => line.trim().to_string(),
                _ => break,
            }
        };

        if choice.is_empty() {
            break;
        }

        // Resolve chosen package name
        let package_name: String = if let Ok(num) = choice.parse::<usize>() {
            if num == 0 {
                continue;
            } else if num <= filtered.len() {
                filtered[num - 1].name.to_lowercase()
            } else {
                eprintln!(
                    "{}",
                    console_format!("<warning>Invalid selection: {num}</warning>")
                );
                continue;
            }
        } else {
            choice.to_lowercase()
        };

        // Determine constraint
        let (pkg_name, constraint) = if package_name.contains(':') {
            match validation::parse_require_string(&package_name) {
                Ok((n, v)) => (n.to_lowercase(), v),
                Err(e) => {
                    eprintln!("{}", console_format!("<warning>Invalid: {e}</warning>"));
                    continue;
                }
            }
        } else {
            if !validation::validate_package_name(&package_name) {
                eprintln!(
                    "{}",
                    console_format!("<warning>Invalid package name: \"{package_name}\"</warning>")
                );
                continue;
            }

            eprintln!(
                "{}",
                console_format!(
                    "<info>Using version constraint for {package_name} from Packagist...</info>"
                )
            );

            match packagist::fetch_package_versions(&package_name, None).await {
                Ok(versions) => {
                    match version::find_best_candidate(&versions, preferred_stability) {
                        Some(best) => {
                            let stability = version::stability_of(&best.version_normalized);
                            let c = version::find_recommended_require_version(
                                &best.version,
                                &best.version_normalized,
                                stability,
                            );
                            eprintln!(
                                "{}",
                                console_format!(
                                    "<info>Using version {c} for {package_name}</info>"
                                )
                            );
                            (package_name, c)
                        }
                        None => {
                            eprintln!(
                                "{}",
                                console_format!(
                                    "<warning>Could not find a version of \"{package_name}\" matching \
                                     your minimum-stability. Try specifying it explicitly.</warning>"
                                )
                            );
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        console_format!(
                            "<warning>Could not fetch versions for \"{package_name}\": {e}</warning>"
                        )
                    );
                    continue;
                }
            }
        };

        selected.insert(pkg_name, constraint);

        // Ask whether to add more
        eprint!("Search for another package? [y/N] ");
        let _ = std::io::stderr().flush();

        let again = {
            let stdin_locked = stdin.lock();
            let mut lines = stdin_locked.lines();
            match lines.next() {
                Some(Ok(line)) => line.trim().to_lowercase(),
                _ => break,
            }
        };

        if again != "y" && again != "yes" {
            break;
        }
    }

    Ok(selected)
}

fn get_default_package_name(working_dir: &Path) -> String {
    let dir_name = working_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");
    let name = validation::sanitize_package_name_component(dir_name);

    let vendor = get_git_config_value("github.user")
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .map(|v| validation::sanitize_package_name_component(&v))
        .unwrap_or_else(|| name.clone());

    format!("{vendor}/{name}")
}

fn get_default_author() -> Option<String> {
    let name = get_git_config_value("user.name")?;
    let email = get_git_config_value("user.email");

    match email {
        Some(email) => Some(format!("{name} <{email}>")),
        None => Some(name),
    }
}

fn get_git_config_value(key: &str) -> Option<String> {
    Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        })
}

fn parse_requirements(reqs: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for req in reqs {
        let (name, version) =
            validation::parse_require_string(req).map_err(|e| anyhow::anyhow!(e))?;
        map.insert(name, version);
    }
    Ok(map)
}

fn build_autoload(path: &str, package_name: &str) -> Option<RawAutoload> {
    let namespace = validation::namespace_from_package_name(package_name)?;
    let mut psr4 = BTreeMap::new();
    psr4.insert(format!("{namespace}\\"), path.to_string());
    Some(RawAutoload { psr4 })
}

fn parse_repositories(repos: &[String]) -> anyhow::Result<Vec<RawRepository>> {
    let mut result = Vec::new();
    for repo in repos {
        if repo.starts_with('{') {
            // JSON format
            let parsed: serde_json::Value =
                serde_json::from_str(repo).context("Invalid repository JSON")?;
            let repo_type = parsed["type"].as_str().unwrap_or("vcs").to_string();
            let url = parsed["url"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Repository JSON must contain a 'url' field"))?
                .to_string();
            result.push(RawRepository { repo_type, url });
        } else {
            // Plain URL
            result.push(RawRepository {
                repo_type: "vcs".to_string(),
                url: repo.clone(),
            });
        }
    }
    Ok(result)
}

fn has_vendor_ignore(gitignore_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(gitignore_path) else {
        return false;
    };

    let pattern = regex::Regex::new(r"^/?vendor(/\*?)?$").unwrap();
    content.lines().any(|line| pattern.is_match(line.trim()))
}

fn add_vendor_ignore(gitignore_path: &Path) -> anyhow::Result<()> {
    let mut contents = if gitignore_path.exists() {
        std::fs::read_to_string(gitignore_path)?
    } else {
        String::new()
    };

    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }

    contents.push_str("/vendor/\n");
    std::fs::write(gitignore_path, contents)?;
    Ok(())
}
