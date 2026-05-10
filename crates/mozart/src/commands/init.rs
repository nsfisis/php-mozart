use anyhow::{Context as _, bail};
use clap::Args;
use colored::Colorize as _;
use mozart_core::console::IoInterface;
use mozart_core::console_format;
use mozart_core::package::{
    self, RawAuthor, RawAutoload, RawPackageData, RawRepository, Stability,
};
use mozart_core::repository::{packagist, version};
use mozart_core::validation;
use std::collections::BTreeMap;
use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

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
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let cache_config = mozart_core::repository::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_core::repository::cache::Cache::repo(&cache_config);

    let working_dir = cli.working_dir()?;

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

    let composer = if io.lock().unwrap().is_interactive() {
        build_interactive(args, &io, &working_dir, &repo_cache).await?
    } else {
        build_non_interactive(args, &working_dir)?
    };

    let json = package::to_json_pretty(&composer)?;

    if io.lock().unwrap().is_interactive() {
        io.lock().unwrap().info("");
        io.lock().unwrap().info(&json);
        io.lock().unwrap().info("");

        if !io.lock().unwrap().confirm(&console_format!(
            "Do you confirm generation [<comment>yes</comment>]?"
        )) {
            io.lock().unwrap().error("Command aborted");
            return Err(mozart_core::exit_code::bail_silent(
                mozart_core::exit_code::GENERAL_ERROR,
            ));
        }
    } else {
        io.lock()
            .unwrap()
            .info(&format!("Writing {}", composer_file.display()));
    }

    package::write_to_file(&composer, &composer_file).context("Failed to write composer.json")?;

    let has_dependencies = !composer.require.is_empty() || !composer.require_dev.is_empty();

    // --autoload — create the source folder. When the project has no
    // dependencies, Composer also runs `dump-autoload` so the autoloader is
    // immediately usable; failures are downgraded to a warning to mirror
    // Composer's try/catch around `runDumpAutoloadCommand`.
    if let Some(ref autoload) = composer.autoload {
        for path in autoload.psr4.values() {
            let dir = working_dir.join(path);
            if !dir.exists() {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("Failed to create directory {}", dir.display()))?;
            }
        }

        if !has_dependencies {
            let dump_args = super::dump_autoload::DumpAutoloadArgs::default();
            if let Err(e) = super::dump_autoload::execute(&dump_args, cli, io.clone()).await {
                io.lock()
                    .unwrap()
                    .error(&format!("Could not run dump-autoload. ({e})"));
            }
        }
    }

    // Offer to add /vendor/ to .gitignore
    if io.lock().unwrap().is_interactive() && working_dir.join(".git").is_dir() {
        let gitignore_path = working_dir.join(".gitignore");
        if !has_vendor_ignore(&gitignore_path)
            && io.lock().unwrap().confirm(&console_format!(
                "Would you like the <info>vendor</info> directory added to your <info>.gitignore</info> [<comment>yes</comment>]?"
            ))
        {
            add_vendor_ignore(&gitignore_path)?;
        }
    }

    // Run `composer update` after init when the new project has dependencies
    // and the user confirms — Composer's L190-193.
    if io.lock().unwrap().is_interactive()
        && has_dependencies
        && io.lock().unwrap().confirm(&console_format!(
            "Would you like to install dependencies now [<comment>yes</comment>]?"
        ))
    {
        let update_args = super::update::UpdateArgs::default();
        if let Err(e) = super::update::execute(&update_args, cli, io.clone()).await {
            io.lock().unwrap().error(&format!(
                "Could not update dependencies. Run `composer update` to see more information. ({e})"
            ));
        }
    }

    // Show autoload info
    if let Some(ref autoload) = composer.autoload
        && let Some((ns, path)) = autoload.psr4.iter().next()
    {
        io.lock().unwrap().info(&console_format!(
            "PSR-4 autoloading configured. Use \"<comment>namespace {ns};</comment>\" in {path}"
        ));
        io.lock().unwrap().info(&console_format!(
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
    io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
    working_dir: &Path,
    repo_cache: &mozart_core::repository::cache::Cache,
) -> anyhow::Result<RawPackageData> {
    io.lock().unwrap().info("");
    io.lock().unwrap().info(&format!(
        "  {}  ",
        "Welcome to the Mozart config generator".white().on_blue()
    ));
    io.lock().unwrap().info("");
    io.lock()
        .unwrap()
        .info("This command will guide you through creating your composer.json config.");
    io.lock().unwrap().info("");

    // Package name
    let default_name = args
        .name
        .clone()
        .unwrap_or_else(|| get_default_package_name(working_dir));
    let name = io.lock().unwrap().ask_validated(
        &console_format!(
            "Package name (<vendor>/<name>) [<comment>{}</comment>]",
            &default_name,
        ),
        &default_name,
        Box::new(|val| {
            if validation::validate_package_name(val) {
                Ok(())
            } else {
                Err(format!(
                    "The package name {val} is invalid, it should be lowercase and have a vendor name, a forward slash, and a package name"
                ))
            }
        }),
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    // Description
    let default_desc = args.description.clone().unwrap_or_default();
    let description = io.lock().unwrap().ask(
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
    let author_input = io.lock().unwrap().ask(
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

    // Minimum Stability — Composer's askAndValidate loops until valid (the
    // validator throws InvalidArgumentException, Symfony's QuestionHelper
    // catches it and re-prompts when maxAttempts is null).
    let default_stability = args.stability.clone().unwrap_or_default();
    let stability_input = io.lock().unwrap()
        .ask_validated(
            &console_format!(
                "Minimum Stability [<comment>{}</comment>]",
                &default_stability
            ),
            &default_stability,
            Box::new(|val| {
                if val.is_empty() || validation::validate_stability(val) {
                    Ok(())
                } else {
                    Err(format!(
                        "Invalid minimum stability \"{val}\". Must be empty or one of: dev, alpha, beta, rc, stable"
                    ))
                }
            }),
        )
        .map_err(|e| anyhow::anyhow!(e))?;
    let minimum_stability = if stability_input.is_empty() {
        None
    } else {
        Some(stability_input.to_lowercase())
    };

    // Package Type
    let default_type = args.r#type.clone().unwrap_or_default();
    let type_input = io.lock().unwrap().ask(
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

    // License — Composer prompts once, then validates outside the prompt and
    // throws on invalid (no retry loop). See InitCommand::interact L364-372.
    let default_license = args
        .license
        .clone()
        .or_else(|| std::env::var("COMPOSER_DEFAULT_LICENSE").ok())
        .unwrap_or_default();
    let license_input = io.lock().unwrap().ask(
        &console_format!("License [<comment>{}</comment>]", &default_license),
        &default_license,
    );
    let license = if license_input.is_empty() {
        None
    } else if validation::validate_license(&license_input)
        || license_input.eq_ignore_ascii_case("proprietary")
    {
        Some(license_input)
    } else {
        bail!(
            "Invalid license provided: {license_input}. Only SPDX license identifiers (https://spdx.org/licenses/) or \"proprietary\" are accepted."
        );
    };

    // Dependencies
    let preferred_stability = minimum_stability
        .as_deref()
        .map(Stability::parse)
        .unwrap_or(Stability::Stable);

    io.lock().unwrap().info("");
    io.lock()
        .unwrap()
        .info(&console_format!("<info>Define your dependencies.</info>"));
    io.lock().unwrap().info("");

    // Composer (InitCommand::interact L389-403): if --require was passed,
    // skip the confirmation; otherwise ask before entering the discovery loop.
    let mut require = parse_requirements(&args.require)?;
    if !require.is_empty()
        || io.lock().unwrap().confirm(&console_format!(
            "Would you like to define your dependencies (require) interactively [<comment>yes</comment>]?"
        ))
    {
        let interactive_require = interactive_search_packages(
            "require",
            &require,
            preferred_stability,
            repo_cache,
            io,
        )
        .await?;
        for (name, constraint) in interactive_require {
            require.insert(name, constraint);
        }
    }

    // Dev Dependencies
    io.lock().unwrap().info("");
    io.lock().unwrap().info(&console_format!(
        "<info>Define your dev dependencies.</info>"
    ));
    io.lock().unwrap().info("");

    let mut require_dev = parse_requirements(&args.require_dev)?;
    if !require_dev.is_empty()
        || io.lock().unwrap().confirm(&console_format!(
            "Would you like to define your dev dependencies (require-dev) interactively [<comment>yes</comment>]?"
        ))
    {
        let all_required: BTreeMap<String, String> = require
            .iter()
            .chain(require_dev.iter())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let interactive_dev = interactive_search_packages(
            "require-dev",
            &all_required,
            preferred_stability,
            repo_cache,
            io,
        )
        .await?;
        for (name, constraint) in interactive_dev {
            require_dev.insert(name, constraint);
        }
    }

    // PSR-4 Autoload — Composer validates with regex `^[^/][A-Za-z0-9\-_/]+/$`
    // via askAndValidate (loops until valid). `n`/`no` skips.
    let default_autoload = args.autoload.clone().unwrap_or_else(|| "src/".to_string());
    let namespace = validation::namespace_from_package_name(&name).unwrap_or_default();
    let autoload_input = io.lock().unwrap()
        .ask_validated(
            &console_format!(
                "Add PSR-4 autoload mapping? Maps namespace \"{namespace}\" to the entered relative path. [<comment>{}</comment>, n to skip]",
                &default_autoload,
            ),
            &default_autoload,
            Box::new(|val| {
                if val == "n" || val == "no" || validation::validate_autoload_path(val) {
                    Ok(())
                } else {
                    Err(format!(
                        "The src folder name \"{val}\" is invalid. Please add a relative path with tailing forward slash. [A-Za-z0-9_-/]+/"
                    ))
                }
            }),
        )
        .map_err(|e| anyhow::anyhow!(e))?;
    let autoload = if autoload_input == "n" || autoload_input == "no" {
        None
    } else {
        build_autoload(&autoload_input, &name)
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
    repo_cache: &mozart_core::repository::cache::Cache,
    io: &std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
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
                io.lock().unwrap().info(&console_format!(
                    "<warning>Search failed: {e}. Try again.</warning>"
                ));
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
            io.lock().unwrap().info(&console_format!(
                "<warning>No new packages found for \"{query}\" (total: {total}).</warning>"
            ));
            continue;
        }

        io.lock().unwrap().info(&format!(
            "\nFound {} package{} for \"{}\":",
            filtered.len(),
            if filtered.len() == 1 { "" } else { "s" },
            query,
        ));

        let name_width = filtered.iter().map(|r| r.name.len()).max().unwrap_or(0);
        for (idx, result) in filtered.iter().enumerate() {
            let desc = if result.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", result.description)
            };
            io.lock().unwrap().info(&format!(
                "  [{idx}] {:<width$}{desc}",
                result.name,
                idx = idx + 1,
                width = name_width,
            ));
        }
        io.lock()
            .unwrap()
            .info("  [0] Search again / enter full package name");
        io.lock().unwrap().info("");

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
                io.lock().unwrap().info(&console_format!(
                    "<warning>Invalid selection: {num}</warning>"
                ));
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
                    io.lock()
                        .unwrap()
                        .info(&console_format!("<warning>Invalid: {e}</warning>"));
                    continue;
                }
            }
        } else {
            if !validation::validate_package_name(&package_name) {
                io.lock().unwrap().info(&console_format!(
                    "<warning>Invalid package name: \"{package_name}\"</warning>"
                ));
                continue;
            }

            io.lock().unwrap().info(&console_format!(
                "<info>Using version constraint for {package_name} from Packagist...</info>"
            ));

            match packagist::fetch_package_versions(&package_name, repo_cache).await {
                Ok(versions) => {
                    match version::find_best_candidate(&versions, preferred_stability) {
                        Some(best) => {
                            let stability = version::stability_of(&best.version_normalized);
                            let c = version::find_recommended_require_version(
                                &best.version,
                                &best.version_normalized,
                                stability,
                            );
                            io.lock().unwrap().info(&console_format!(
                                "<info>Using version {c} for {package_name}</info>"
                            ));
                            (package_name, c)
                        }
                        None => {
                            io.lock().unwrap().info(&console_format!(
                                "<warning>Could not find a version of \"{package_name}\" matching \
                                 your minimum-stability. Try specifying it explicitly.</warning>"
                            ));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    io.lock().unwrap().info(&console_format!(
                        "<warning>Could not fetch versions for \"{package_name}\": {e}</warning>"
                    ));
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

    let vendor = std::env::var("COMPOSER_DEFAULT_VENDOR")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| get_git_config_value("github.user"))
        .or_else(|| std::env::var("USERNAME").ok().filter(|v| !v.is_empty()))
        .or_else(|| std::env::var("USER").ok().filter(|v| !v.is_empty()))
        .map(|v| validation::sanitize_package_name_component(&v))
        .unwrap_or_else(|| name.clone());

    format!("{vendor}/{name}")
}

fn get_default_author() -> Option<String> {
    let name = std::env::var("COMPOSER_DEFAULT_AUTHOR")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| get_git_config_value("user.name"));

    let email = std::env::var("COMPOSER_DEFAULT_EMAIL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| get_git_config_value("user.email"));

    match (name, email) {
        (Some(n), Some(e)) => Some(format!("{n} <{e}>")),
        _ => None,
    }
}

/// `git config -l` parsed into `key=value` pairs and cached for the life of
/// the process. Mirrors Composer's `InitCommand::getGitConfig`, which runs
/// the command once and memoises the parsed map.
fn get_git_config() -> &'static BTreeMap<String, String> {
    static GIT_CONFIG: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    GIT_CONFIG.get_or_init(|| {
        let mut map = BTreeMap::new();
        let Ok(output) = Command::new("git").args(["config", "-l"]).output() else {
            return map;
        };
        if !output.status.success() {
            return map;
        }
        let Ok(text) = String::from_utf8(output.stdout) else {
            return map;
        };
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                map.insert(key.to_string(), value.to_string());
            }
        }
        map
    })
}

fn get_git_config_value(key: &str) -> Option<String> {
    get_git_config().get(key).cloned().filter(|v| !v.is_empty())
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

/// Parse `--repository` arguments. Mirrors
/// `Composer\Repository\RepositoryFactory::configFromString`:
///
/// * `http(s)://...` → `{type: composer, url: $repo}`.
/// * `{...}` → JSON object parsed verbatim into a repository config.
/// * `*.json` file path → composer-type repo file (deferred; not yet supported).
/// * anything else → reject with an error matching Composer's wording.
fn parse_repositories(repos: &[String]) -> anyhow::Result<Vec<RawRepository>> {
    let mut result = Vec::new();
    for repo in repos {
        let parsed: serde_json::Value = if repo.starts_with("http") {
            serde_json::json!({ "type": "composer", "url": repo })
        } else if repo.starts_with('{') {
            serde_json::from_str(repo).context("Invalid repository JSON")?
        } else {
            bail!(
                "Invalid repository url ({repo}) given. Has to be a .json file, an http url or a JSON object."
            );
        };

        let repo_type = parsed
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Repository JSON must contain a 'type' field"))?
            .to_string();
        let url = parsed
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let package = parsed.get("package").cloned();
        let only = parsed.get("only").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        });
        let exclude = parsed.get("exclude").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        });
        let canonical = parsed.get("canonical").and_then(|v| v.as_bool());
        let security_advisories = parsed.get("security-advisories").cloned();

        result.push(RawRepository {
            repo_type,
            url,
            package,
            only,
            exclude,
            canonical,
            security_advisories,
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repositories_http_url_yields_composer_type() {
        let repos = parse_repositories(&["https://repo.example.com".to_string()]).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].repo_type, "composer");
        assert_eq!(repos[0].url.as_deref(), Some("https://repo.example.com"));
    }

    #[test]
    fn parse_repositories_http_scheme_also_matches() {
        let repos = parse_repositories(&["http://example.com".to_string()]).unwrap();
        assert_eq!(repos[0].repo_type, "composer");
    }

    #[test]
    fn parse_repositories_json_object_preserved() {
        let repos = parse_repositories(&[
            r#"{"type":"vcs","url":"https://github.com/acme/repo"}"#.to_string()
        ])
        .unwrap();
        assert_eq!(repos[0].repo_type, "vcs");
        assert_eq!(
            repos[0].url.as_deref(),
            Some("https://github.com/acme/repo")
        );
    }

    #[test]
    fn parse_repositories_unknown_form_is_error() {
        let err = parse_repositories(&["not-a-url-or-json".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("Has to be a .json file, an http url or a JSON object"),
            "{err}",
        );
    }

    #[test]
    fn parse_repositories_json_without_type_is_error() {
        let err =
            parse_repositories(&[r#"{"url":"https://example.com"}"#.to_string()]).unwrap_err();
        assert!(err.to_string().contains("'type'"), "{err}");
    }
}
