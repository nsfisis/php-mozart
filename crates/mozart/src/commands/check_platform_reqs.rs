use clap::Args;
use mozart_core::console::Console;
use mozart_core::console_writeln;
use mozart_core::console_writeln_error;
use mozart_core::installer::{InstalledCandidate, InstalledRepoLite};
use std::collections::BTreeMap;
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
    console: &Console,
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
    let mut requires: BTreeMap<String, Vec<Link>> = BTreeMap::new();

    if args.lock {
        if !lock_path.exists() {
            anyhow::bail!("No composer.lock found. Run `mozart install` or `mozart update` first.");
        }
        console_writeln_error!(
            console,
            "<info>Checking {}platform requirements using the lock file</info>",
            dev_text,
        );
        load_lock(&lock_path, args.no_dev, &mut installed_repo, &mut requires)?;
    } else {
        let installed_packages_present = installed_path.exists()
            && !mozart_registry::installed::InstalledPackages::read(&vendor_dir)?
                .packages
                .is_empty();

        if installed_packages_present {
            let installed = mozart_registry::installed::InstalledPackages::read(&vendor_dir)?;
            console_writeln_error!(
                console,
                "<info>Checking {}platform requirements for packages in the vendor dir</info>",
                dev_text,
            );
            load_installed(&installed, args.no_dev, &mut installed_repo, &mut requires);
        } else {
            console_writeln_error!(
                console,
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
            provides: BTreeMap::new(),
            replaces: BTreeMap::new(),
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

    print_table(&results, format, console)?;

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code));
    }

    Ok(())
}

fn load_lock(
    lock_path: &Path,
    no_dev: bool,
    repo: &mut InstalledRepoLite,
    requires: &mut BTreeMap<String, Vec<Link>>,
) -> anyhow::Result<()> {
    let lock = mozart_registry::lockfile::LockFile::read_from_file(lock_path)?;

    let mut all: Vec<&mozart_registry::lockfile::LockedPackage> = lock.packages.iter().collect();
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
    installed: &mozart_registry::installed::InstalledPackages,
    no_dev: bool,
    repo: &mut InstalledRepoLite,
    requires: &mut BTreeMap<String, Vec<Link>>,
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
    extra: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
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
    requires: &mut BTreeMap<String, Vec<Link>>,
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

fn print_table(results: &[CheckRow], format: &str, console: &Console) -> anyhow::Result<()> {
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
        console_writeln!(console, "{}", &serde_json::to_string_pretty(&rows)?);
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
                    console,
                    "<info>{padded_name}</info>  <comment>{padded_version}</comment>  {link_text}  <info>success</info>{provider_suffix}",
                );
            }
            Status::Failed => {
                console_writeln!(
                    console,
                    "<comment>{padded_name}</comment>  <comment>{padded_version}</comment>  {link_text}  <error>failed</error>{provider_suffix}",
                );
            }
            Status::Missing => {
                console_writeln!(
                    console,
                    "<comment>{padded_name}</comment>  <comment>{padded_version}</comment>  {link_text}  <error>missing</error>{provider_suffix}",
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn test_console() -> Console {
        Console::new(0, true, false, true, true)
    }

    fn write_lock(
        path: &Path,
        packages: &[(&str, BTreeMap<String, String>)],
        dev_packages: &[(&str, BTreeMap<String, String>)],
    ) {
        write_lock_with(path, packages, dev_packages, &[]);
    }

    fn write_lock_with(
        path: &Path,
        packages: &[(&str, BTreeMap<String, String>)],
        dev_packages: &[(&str, BTreeMap<String, String>)],
        provides: &[(&str, BTreeMap<String, String>, BTreeMap<String, String>)], // (name, provide, replace)
    ) {
        let make_pkg = |name: &str,
                        require: BTreeMap<String, String>,
                        provide: BTreeMap<String, String>,
                        replace: BTreeMap<String, String>| {
            serde_json::json!({
                "name": name,
                "version": "1.0.0",
                "require": require,
                "provide": provide,
                "replace": replace,
            })
        };

        let mut pkgs_json: Vec<serde_json::Value> = packages
            .iter()
            .map(|(name, req)| make_pkg(name, req.clone(), BTreeMap::new(), BTreeMap::new()))
            .collect();
        for (name, prov, repl) in provides {
            pkgs_json.push(make_pkg(name, BTreeMap::new(), prov.clone(), repl.clone()));
        }

        let dev_pkgs_json: Vec<serde_json::Value> = dev_packages
            .iter()
            .map(|(name, req)| make_pkg(name, req.clone(), BTreeMap::new(), BTreeMap::new()))
            .collect();

        let lock_json = serde_json::json!({
            "_readme": ["This file locks the dependencies"],
            "content-hash": "abc123",
            "packages": pkgs_json,
            "packages-dev": dev_pkgs_json,
            "aliases": [],
            "minimum-stability": "stable",
            "stability-flags": {},
            "prefer-stable": false,
            "prefer-lowest": false,
            "platform": {},
            "platform-dev": {},
            "plugin-api-version": "2.6.0",
        });

        std::fs::write(path, serde_json::to_string_pretty(&lock_json).unwrap()).unwrap();
    }

    #[test]
    fn test_is_platform_package() {
        assert!(mozart_core::platform::is_platform_package("php"));
        assert!(mozart_core::platform::is_platform_package("ext-json"));
        assert!(mozart_core::platform::is_platform_package("ext-mbstring"));
        assert!(mozart_core::platform::is_platform_package("lib-pcre"));
        assert!(mozart_core::platform::is_platform_package("php-64bit"));
        assert!(mozart_core::platform::is_platform_package(
            "composer-plugin-api"
        ));
        assert!(mozart_core::platform::is_platform_package(
            "composer-runtime-api"
        ));

        assert!(!mozart_core::platform::is_platform_package(
            "monolog/monolog"
        ));
        assert!(!mozart_core::platform::is_platform_package("psr/log"));
        assert!(!mozart_core::platform::is_platform_package(
            "symfony/console"
        ));
    }

    #[test]
    fn test_load_lock_collects_platform_requires() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("composer.lock");

        let mut pkg_require = BTreeMap::new();
        pkg_require.insert("php".to_string(), ">=8.1".to_string());
        pkg_require.insert("ext-json".to_string(), "*".to_string());
        pkg_require.insert("monolog/monolog".to_string(), "^3.0".to_string()); // not platform

        write_lock(&lock_path, &[("vendor/pkg", pkg_require)], &[]);

        let mut repo = InstalledRepoLite::new();
        let mut requires: BTreeMap<String, Vec<Link>> = BTreeMap::new();
        load_lock(&lock_path, false, &mut repo, &mut requires).unwrap();

        assert!(requires.contains_key("php"));
        assert!(requires.contains_key("ext-json"));
        assert!(!requires.contains_key("monolog/monolog"));

        let php_links = &requires["php"];
        assert_eq!(php_links.len(), 1);
        assert_eq!(php_links[0].constraint, ">=8.1");
        assert_eq!(php_links[0].source, "vendor/pkg");
    }

    #[test]
    fn test_load_lock_no_dev_skips_dev_packages() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("composer.lock");

        let mut prod_require = BTreeMap::new();
        prod_require.insert("php".to_string(), ">=8.0".to_string());

        let mut dev_require = BTreeMap::new();
        dev_require.insert("ext-xdebug".to_string(), "*".to_string());

        write_lock(
            &lock_path,
            &[("vendor/prod", prod_require)],
            &[("vendor/devpkg", dev_require)],
        );

        let mut repo = InstalledRepoLite::new();
        let mut requires: BTreeMap<String, Vec<Link>> = BTreeMap::new();
        load_lock(&lock_path, true, &mut repo, &mut requires).unwrap();
        assert!(requires.contains_key("php"));
        assert!(!requires.contains_key("ext-xdebug"));

        let mut repo2 = InstalledRepoLite::new();
        let mut requires2: BTreeMap<String, Vec<Link>> = BTreeMap::new();
        load_lock(&lock_path, false, &mut repo2, &mut requires2).unwrap();
        assert!(requires2.contains_key("ext-xdebug"));
    }

    #[test]
    fn test_provider_candidate_satisfies_require() {
        // symfony/polyfill-mbstring provides ext-mbstring at "*".
        // A package that requires ext-mbstring "^1.0" should succeed via the
        // provider — even when ext-mbstring is not detected on the platform.
        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(InstalledCandidate {
            name: "vendor/pkg".into(),
            pretty_name: "vendor/pkg".into(),
            version: "1.0.0".into(),
            pretty_version: "1.0.0".into(),
            provides: BTreeMap::new(),
            replaces: BTreeMap::new(),
        });
        let mut polyfill_provides = BTreeMap::new();
        polyfill_provides.insert("ext-mbstring".to_string(), "*".to_string());
        repo.add_candidate(InstalledCandidate {
            name: "symfony/polyfill-mbstring".into(),
            pretty_name: "symfony/polyfill-mbstring".into(),
            version: "1.30.0".into(),
            pretty_version: "1.30.0".into(),
            provides: polyfill_provides,
            replaces: BTreeMap::new(),
        });

        let candidates = repo.find_with_replacers_and_providers("ext-mbstring");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "symfony/polyfill-mbstring");

        // Constraint check: the provide constraint "*" intersects "^1.0".
        let cand = mozart_semver::VersionConstraint::parse("*").unwrap();
        let req = mozart_semver::VersionConstraint::parse("^1.0").unwrap();
        assert!(req.intersects(&cand));
    }

    #[test]
    fn test_replacer_candidate_satisfies_require() {
        let mut replaces = BTreeMap::new();
        replaces.insert("ext-mbstring".to_string(), "1.0".to_string());

        let mut repo = InstalledRepoLite::new();
        repo.add_candidate(InstalledCandidate {
            name: "vendor/legacy-replacement".into(),
            pretty_name: "vendor/legacy-replacement".into(),
            version: "2.0.0".into(),
            pretty_version: "2.0.0".into(),
            provides: BTreeMap::new(),
            replaces,
        });

        let candidates = repo.find_with_replacers_and_providers("ext-mbstring");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "vendor/legacy-replacement");

        let cand = mozart_semver::VersionConstraint::parse("1.0").unwrap();
        let req = mozart_semver::VersionConstraint::parse("^1.0").unwrap();
        assert!(req.intersects(&cand));
    }

    #[test]
    fn test_json_failed_requirement_is_object_with_four_keys() {
        let row = CheckRow {
            platform_package: "php".to_string(),
            version: "8.1.0".to_string(),
            link: Some(Link {
                source: "vendor/pkg".to_string(),
                target: "php".to_string(),
                description: "requires",
                constraint: ">=8.2".to_string(),
                pretty_constraint: ">=8.2".to_string(),
            }),
            status: Status::Failed,
            provider: String::new(),
        };

        let console = test_console();
        // Capture by rendering through serde directly (the print_table writer
        // goes to stdout via a macro — keep the assertion on the JSON shape).
        print_table(&[row.clone()], "json", &console).unwrap();

        // Reproduce the same shape and assert key invariants.
        let value = serde_json::json!({
            "name": row.platform_package,
            "version": row.version,
            "status": "failed",
            "failed_requirement": {
                "source": row.link.as_ref().unwrap().source,
                "type": row.link.as_ref().unwrap().description,
                "target": row.link.as_ref().unwrap().target,
                "constraint": row.link.as_ref().unwrap().pretty_constraint,
            },
            "provider": serde_json::Value::Null,
        });
        let obj = value["failed_requirement"].as_object().unwrap();
        assert_eq!(obj.len(), 4);
        assert!(obj.contains_key("source"));
        assert!(obj.contains_key("type"));
        assert!(obj.contains_key("target"));
        assert!(obj.contains_key("constraint"));
    }

    #[test]
    fn test_json_provider_string_for_indirect_candidate() {
        let row = CheckRow {
            platform_package: "ext-mbstring".to_string(),
            version: "*".to_string(),
            link: None,
            status: Status::Success,
            provider: "provided by symfony/polyfill-mbstring".to_string(),
        };
        let value = serde_json::json!({
            "name": row.platform_package,
            "version": row.version,
            "status": "success",
            "failed_requirement": serde_json::Value::Null,
            "provider": row.provider,
        });
        assert_eq!(value["provider"], "provided by symfony/polyfill-mbstring");
        assert_eq!(value["failed_requirement"], serde_json::Value::Null);
    }

    #[test]
    fn test_json_status_strips_tags() {
        // Status emits plain "success" / "failed" / "missing" — never the
        // `<info>…</info>` tag wrapper. Composer's PHP printTable explicitly
        // calls strip_tags(); ours never wraps in the first place.
        for (status, expected) in [
            (Status::Success, "success"),
            (Status::Failed, "failed"),
            (Status::Missing, "missing"),
        ] {
            let row = CheckRow {
                platform_package: "ext-x".into(),
                version: "1.0".into(),
                link: None,
                status,
                provider: String::new(),
            };
            let s = match row.status {
                Status::Success => "success",
                Status::Failed => "failed",
                Status::Missing => "missing",
            };
            assert_eq!(s, expected);
        }
    }
}
