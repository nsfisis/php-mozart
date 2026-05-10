use clap::Args;
use mozart_core::console::IoInterface;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::installer::{InstalledCandidate, InstalledRepoLite};
use std::path::Path;

#[derive(Args)]
pub struct CheckPlatformReqsArgs {
    /// Disables checking of require-dev packages requirements
    #[arg(long)]
    pub no_dev: bool,

    /// Check packages from the lock file
    #[arg(long)]
    pub lock: bool,

    /// Output format (text, json)
    #[arg(short, long, value_parser = ["text", "json"], default_value = "text")]
    pub format: String,
}

/// One `require` link, mirroring `Composer\Package\Link`.
///
/// Composer's `Link` carries the requiring package's name (`source`), the
/// target package name, the link description (`requires` / `provides` /
/// `replaces` / etc.), and a `Constraint` object plus its pretty-printed
/// form. `check-platform-reqs` only ever produces "requires" links, but the
/// `description` field is kept for parity with the JSON shape that exposes
/// `link->getDescription()` to consumers.
#[derive(Debug, Clone)]
struct Link {
    source: String,
    target: String,
    description: &'static str,
    constraint: String,
    pretty_constraint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Success,
    Failed,
    Missing,
}

/// Mirrors PHP's per-row tuple
/// `[$platformPackage, $version, $link, $status, $provider]`.
#[derive(Debug, Clone)]
struct CheckRow {
    platform_package: String,
    version: String,
    link: Option<Link>,
    status: Status,
    provider: String,
}

pub async fn execute(
    args: &CheckPlatformReqsArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        anyhow::bail!("No composer.json found in {}", working_dir.display());
    }

    let format = args.format.as_str();
    let dev_text = if args.no_dev { "non-dev " } else { "" };

    let lock_path = working_dir.join("composer.lock");
    let vendor_dir = working_dir.join("vendor");
    let installed_path = vendor_dir.join("composer/installed.json");

    let mut installed_repo = InstalledRepoLite::new();
    let mut requires = indexmap::IndexMap::new();

    if args.lock {
        if !lock_path.exists() {
            anyhow::bail!("No composer.lock found. Run `mozart install` or `mozart update` first.");
        }
        console_writeln_error!(
            io,
            "<info>Checking {}platform requirements using the lock file</info>",
            dev_text,
        );
        load_lock(&lock_path, args.no_dev, &mut installed_repo, &mut requires)?;
    } else {
        let installed_packages_present = installed_path.exists()
            && !mozart_core::repository::installed::InstalledPackages::read(&vendor_dir)?
                .packages
                .is_empty();

        if installed_packages_present {
            let installed =
                mozart_core::repository::installed::InstalledPackages::read(&vendor_dir)?;
            console_writeln_error!(
                io,
                "<info>Checking {}platform requirements for packages in the vendor dir</info>",
                dev_text,
            );
            load_installed(&installed, args.no_dev, &mut installed_repo, &mut requires);
        } else {
            console_writeln_error!(
                io,
                "<warning>No vendor dir present, checking {}platform requirements from the lock file</warning>",
                dev_text,
            );
            if lock_path.exists() {
                load_lock(&lock_path, args.no_dev, &mut installed_repo, &mut requires)?;
            }
            // No lock either → proceed with the root package only; final output
            // will reflect just the root's requires (possibly empty).
        }
    }

    // RootPackageRepository — Composer's `getDevRequires()` is appended to
    // `$requires` directly, then `$installedRepo->getPackages()` walks the
    // root via the `RootPackageRepository` and adds `getRequires()` (which is
    // the root's `require`, NOT `require-dev`).
    let root = mozart_core::package::read_from_file(&composer_json_path)?;

    if !args.no_dev {
        for (target, constraint) in &root.require_dev {
            push_platform_link(&mut requires, &root.name, target, constraint);
        }
    }
    for (target, constraint) in &root.require {
        push_platform_link(&mut requires, &root.name, target, constraint);
    }

    add_root_as_candidate(&root, &mut installed_repo);

    // PlatformRepository([], []) — empty overrides means "use the real
    // platform". Mirrors Composer's bypass of `config.platform`.
    for pkg in mozart_core::platform::detect_platform() {
        installed_repo.add_candidate(InstalledCandidate {
            name: pkg.name.to_lowercase(),
            pretty_name: pkg.name,
            version: pkg.version.clone(),
            pretty_version: pkg.version,
            provides: indexmap::IndexMap::new(),
            replaces: indexmap::IndexMap::new(),
        });
    }

    let mut results: Vec<CheckRow> = Vec::new();
    let mut exit_code: i32 = 0;

    'requirement: for (require_lc, links) in &requires {
        if !mozart_core::platform::is_platform_package(require_lc) {
            continue;
        }
        let candidates = installed_repo.find_with_replacers_and_providers(require_lc);
        if candidates.is_empty() {
            results.push(CheckRow {
                platform_package: require_lc.clone(),
                version: "n/a".to_string(),
                link: links.first().cloned(),
                status: Status::Missing,
                provider: String::new(),
            });
            exit_code = exit_code.max(2);
            continue;
        }

        let mut req_results: Vec<CheckRow> = Vec::new();

        'candidate: for candidate in &candidates {
            let direct = candidate.name == *require_lc;
            let (candidate_constraint_str, candidate_pretty) = if direct {
                (
                    format!("={}", candidate.version),
                    candidate.pretty_version.clone(),
                )
            } else {
                let cs = candidate
                    .provides
                    .get(require_lc)
                    .or_else(|| candidate.replaces.get(require_lc))
                    .cloned()
                    .unwrap_or_else(|| "*".to_string());
                (cs.clone(), cs)
            };

            let candidate_constraint =
                match mozart_semver::VersionConstraint::parse(&candidate_constraint_str) {
                    Ok(c) => c,
                    Err(_) => {
                        mozart_semver::VersionConstraint::Single(mozart_semver::Constraint::Any)
                    }
                };

            let display_name = if direct {
                candidate.pretty_name.clone()
            } else {
                require_lc.clone()
            };
            let provider = if direct {
                String::new()
            } else {
                format!("provided by {}", candidate.pretty_name)
            };

            for link in links {
                let link_constraint =
                    match mozart_semver::VersionConstraint::parse(&link.constraint) {
                        Ok(c) => c,
                        Err(_) => continue, // skip unparseable user input
                    };
                if !link_constraint.intersects(&candidate_constraint) {
                    req_results.push(CheckRow {
                        platform_package: display_name.clone(),
                        version: candidate_pretty.clone(),
                        link: Some(link.clone()),
                        status: Status::Failed,
                        provider: provider.clone(),
                    });
                    continue 'candidate;
                }
            }

            // Every link's constraint intersects the candidate's — success.
            results.push(CheckRow {
                platform_package: display_name,
                version: candidate_pretty,
                link: None,
                status: Status::Success,
                provider,
            });
            continue 'requirement;
        }

        // No candidate satisfied every link.
        results.extend(req_results);
        exit_code = exit_code.max(1);
    }

    print_table(&results, format, io.clone())?;

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code));
    }

    Ok(())
}

fn load_lock(
    lock_path: &Path,
    no_dev: bool,
    repo: &mut InstalledRepoLite,
    requires: &mut indexmap::IndexMap<String, Vec<Link>>,
) -> anyhow::Result<()> {
    let lock = mozart_core::repository::lockfile::LockFile::read_from_file(lock_path)?;

    let mut all: Vec<&mozart_core::repository::lockfile::LockedPackage> =
        lock.packages.iter().collect();
    if !no_dev && let Some(ref pkgs_dev) = lock.packages_dev {
        all.extend(pkgs_dev.iter());
    }

    for pkg in all {
        repo.add_candidate(InstalledCandidate {
            name: pkg.name.to_lowercase(),
            pretty_name: pkg.name.clone(),
            version: pkg.version.clone(),
            pretty_version: pkg.version.clone(),
            provides: pkg.provide.clone(),
            replaces: pkg.replace.clone(),
        });
        for (target, constraint) in &pkg.require {
            push_platform_link(requires, &pkg.name, target, constraint);
        }
    }

    Ok(())
}

fn load_installed(
    installed: &mozart_core::repository::installed::InstalledPackages,
    no_dev: bool,
    repo: &mut InstalledRepoLite,
    requires: &mut indexmap::IndexMap<String, Vec<Link>>,
) {
    let dev_names: indexmap::IndexSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    for pkg in &installed.packages {
        if no_dev && dev_names.contains(&pkg.name.to_lowercase()) {
            continue;
        }

        let provides = string_map_from_extra(&pkg.extra_fields, "provide");
        let replaces = string_map_from_extra(&pkg.extra_fields, "replace");

        repo.add_candidate(InstalledCandidate {
            name: pkg.name.to_lowercase(),
            pretty_name: pkg.name.clone(),
            version: pkg.version.clone(),
            pretty_version: pkg.version.clone(),
            provides,
            replaces,
        });

        if let Some(require_val) = pkg.extra_fields.get("require")
            && let Some(require_obj) = require_val.as_object()
        {
            for (target, constraint_val) in require_obj {
                let constraint = constraint_val.as_str().unwrap_or("*").to_string();
                push_platform_link(requires, &pkg.name, target, &constraint);
            }
        }
    }
}

fn add_root_as_candidate(
    root: &mozart_core::package::RawPackageData,
    repo: &mut InstalledRepoLite,
) {
    let version = root.version.clone().unwrap_or_else(|| "1.0.0".to_string());
    repo.add_candidate(InstalledCandidate {
        name: root.name.to_lowercase(),
        pretty_name: root.name.clone(),
        version: version.clone(),
        pretty_version: version,
        provides: root.provide.clone(),
        replaces: root.replace.clone(),
    });
}

fn string_map_from_extra(
    extra: &indexmap::IndexMap<String, serde_json::Value>,
    key: &str,
) -> indexmap::IndexMap<String, String> {
    let mut out = indexmap::IndexMap::new();
    if let Some(val) = extra.get(key)
        && let Some(obj) = val.as_object()
    {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

fn push_platform_link(
    requires: &mut indexmap::IndexMap<String, Vec<Link>>,
    source: &str,
    target: &str,
    constraint: &str,
) {
    let target_lc = target.to_lowercase();
    if !mozart_core::platform::is_platform_package(&target_lc) {
        return;
    }
    requires.entry(target_lc.clone()).or_default().push(Link {
        source: source.to_string(),
        target: target_lc,
        description: "requires",
        constraint: constraint.to_string(),
        pretty_constraint: constraint.to_string(),
    });
}

fn print_table(
    results: &[CheckRow],
    format: &str,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    if format == "json" {
        let rows: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                let status_str = match r.status {
                    Status::Success => "success",
                    Status::Failed => "failed",
                    Status::Missing => "missing",
                };
                let failed_requirement = r.link.as_ref().map(|l| {
                    serde_json::json!({
                        "source": l.source,
                        "type": l.description,
                        "target": l.target,
                        "constraint": l.pretty_constraint,
                    })
                });
                let provider = if r.provider.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(r.provider.clone())
                };
                serde_json::json!({
                    "name": r.platform_package,
                    "version": r.version,
                    "status": status_str,
                    "failed_requirement": failed_requirement,
                    "provider": provider,
                })
            })
            .collect();
        console_writeln!(io, "{}", &serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if results.is_empty() {
        return Ok(());
    }

    // Mozart renders a padded fixed-column variant of Symfony's
    // renderTable. Byte-for-byte parity with `composer check-platform-reqs`
    // is deferred to a workspace-wide UI follow-up (see plan §6.3).
    let name_width = results
        .iter()
        .map(|r| r.platform_package.len())
        .max()
        .unwrap_or(0);
    let version_width = results.iter().map(|r| r.version.len()).max().unwrap_or(0);

    for r in results {
        let padded_name = format!("{:<nw$}", r.platform_package, nw = name_width);
        let padded_version = format!("{:<vw$}", r.version, vw = version_width);
        let link_text = r
            .link
            .as_ref()
            .map(|l| {
                format!(
                    "{} {} {} ({})",
                    l.source, l.description, l.target, l.pretty_constraint,
                )
            })
            .unwrap_or_default();
        let provider_suffix = if r.provider.is_empty() {
            String::new()
        } else {
            format!(" {}", r.provider)
        };
        match r.status {
            Status::Success => {
                console_writeln!(
                    io,
                    "<info>{padded_name}</info>  <comment>{padded_version}</comment>  {link_text}  <info>success</info>{provider_suffix}",
                );
            }
            Status::Failed => {
                console_writeln!(
                    io,
                    "<comment>{padded_name}</comment>  <comment>{padded_version}</comment>  {link_text}  <error>failed</error>{provider_suffix}",
                );
            }
            Status::Missing => {
                console_writeln!(
                    io,
                    "<comment>{padded_name}</comment>  <comment>{padded_version}</comment>  {link_text}  <error>missing</error>{provider_suffix}",
                );
            }
        }
    }

    Ok(())
}
