use anyhow::{Context, bail};
use clap::Args;
use colored::Colorize;
use mozart_core::console;
use mozart_core::package::{self, RawAuthor, RawAutoload, RawPackageData, RawRepository};
use mozart_core::validation;
use std::collections::BTreeMap;
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
        build_interactive(args, console, &working_dir)?
    } else {
        build_non_interactive(args, &working_dir)?
    };

    let json = package::to_json_pretty(&composer)?;

    if console.interactive {
        console.info("");
        console.info(&json);
        console.info("");

        if !console.confirm(&format!(
            "Do you confirm generation [{}]?",
            console::comment("yes")
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
            && console.confirm(&format!(
                "Would you like the {} directory added to your {} [{}]?",
                console::info("vendor"),
                console::info(".gitignore"),
                console::comment("yes"),
            ))
        {
            add_vendor_ignore(&gitignore_path)?;
        }
    }

    // Show autoload info
    if let Some(ref autoload) = composer.autoload
        && let Some((ns, path)) = autoload.psr4.iter().next()
    {
        console.info(&format!(
            "PSR-4 autoloading configured. Use \"{}\" in {path}",
            console::comment(&format!("namespace {ns};")),
        ));
        console.info(&format!(
            "Include the Composer autoloader with: {}",
            console::comment("require 'vendor/autoload.php';"),
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
    composer.license = args.license.clone();

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

fn build_interactive(
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
        &format!(
            "Package name (<vendor>/<name>) [{}]",
            mozart_core::console::comment(&default_name),
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
        &format!(
            "Description [{}]",
            mozart_core::console::comment(&default_desc)
        ),
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
        &format!(
            "Author [{}n to skip]",
            if !default_author.is_empty() {
                format!("{}, ", mozart_core::console::comment(&default_author))
            } else {
                String::new()
            }
        ),
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
        &format!(
            "Minimum Stability [{}]",
            mozart_core::console::comment(&default_stability),
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
        &format!(
            "Package Type (e.g. library, project, metapackage, composer-plugin) [{}]",
            mozart_core::console::comment(&default_type),
        ),
        &default_type,
    );
    let package_type = if type_input.is_empty() {
        None
    } else {
        Some(type_input)
    };

    // License
    let default_license = args.license.clone().unwrap_or_default();
    let license_input = console.ask(
        &format!(
            "License [{}]",
            mozart_core::console::comment(&default_license),
        ),
        &default_license,
    );
    let license = if license_input.is_empty() {
        None
    } else {
        Some(license_input)
    };

    // Dependencies
    // TODO: support selecting dependencies interactively
    console.info("");
    console.info(&format!(
        "{}",
        mozart_core::console::info("Define your dependencies.")
    ));
    console.info("");
    let require = parse_requirements(&args.require)?;

    // Dev Dependencies
    // TODO: support selecting dependencies interactively
    let require_dev = parse_requirements(&args.require_dev)?;

    // PSR-4 Autoload
    let default_autoload = args.autoload.clone().unwrap_or_else(|| "src/".to_string());
    let namespace = validation::namespace_from_package_name(&name).unwrap_or_default();
    let autoload_input = console.ask(
        &format!(
            "Add PSR-4 autoload mapping? Maps namespace \"{}\" to the entered relative path. [{}, n to skip]",
            namespace,
            mozart_core::console::comment(&default_autoload),
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
