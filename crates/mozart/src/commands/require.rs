use crate::console;
use crate::package::{self, Stability};
use crate::packagist;
use crate::validation;
use crate::version;
use clap::Args;

#[derive(Args)]
pub struct RequireArgs {
    /// Package(s) to require
    pub packages: Vec<String>,

    /// Add requirement to require-dev
    #[arg(long)]
    pub dev: bool,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Forces installation from package sources when possible
    #[arg(long)]
    pub prefer_source: bool,

    /// Forces installation from package dist
    #[arg(long)]
    pub prefer_dist: bool,

    /// Forces usage of a specific install method (dist, source, auto)
    #[arg(long)]
    pub prefer_install: Option<String>,

    /// Pin the exact version instead of a range
    #[arg(long)]
    pub fixed: bool,

    /// [Deprecated] Do not show install suggestions
    #[arg(long)]
    pub no_suggest: bool,

    /// Do not output download progress
    #[arg(long)]
    pub no_progress: bool,

    /// Disables the automatic update of the lock file
    #[arg(long)]
    pub no_update: bool,

    /// Skip the install step
    #[arg(long)]
    pub no_install: bool,

    /// Skip the audit step
    #[arg(long)]
    pub no_audit: bool,

    /// Audit output format
    #[arg(long)]
    pub audit_format: Option<String>,

    /// Do not block on security advisories
    #[arg(long)]
    pub no_security_blocking: bool,

    /// Run the dependency update with the --no-dev option
    #[arg(long)]
    pub update_no_dev: bool,

    /// [Deprecated] Use --with-dependencies instead
    #[arg(short = 'w', long)]
    pub update_with_dependencies: bool,

    /// [Deprecated] Use --with-all-dependencies instead
    #[arg(short = 'W', long)]
    pub update_with_all_dependencies: bool,

    /// Update also dependencies of newly required packages
    #[arg(long)]
    pub with_dependencies: bool,

    /// Update all dependencies including root requirements
    #[arg(long)]
    pub with_all_dependencies: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

    /// Prefer stable versions of dependencies
    #[arg(long)]
    pub prefer_stable: bool,

    /// Prefer lowest versions of dependencies
    #[arg(long)]
    pub prefer_lowest: bool,

    /// Prefer minimal restriction updates
    #[arg(short = 'm', long)]
    pub minimal_changes: bool,

    /// Sort packages in composer.json
    #[arg(long)]
    pub sort_packages: bool,

    /// Optimizes PSR-0 and PSR-4 packages to be loaded with classmaps
    #[arg(short, long)]
    pub optimize_autoloader: bool,

    /// Autoload classes from the classmap only
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,

    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu_autoloader: bool,

    /// Use a custom prefix for the APCu autoloader cache
    #[arg(long)]
    pub apcu_autoloader_prefix: Option<String>,
}

pub fn execute(args: &RequireArgs, cli: &super::Cli) -> anyhow::Result<()> {
    if args.packages.is_empty() {
        anyhow::bail!("Not enough arguments (missing: \"packages\").");
    }

    // Resolve working directory
    let working_dir = if let Some(ref dir) = cli.working_dir {
        std::path::PathBuf::from(dir)
    } else {
        std::env::current_dir()?
    };

    let composer_path = working_dir.join("composer.json");
    if !composer_path.exists() {
        anyhow::bail!(
            "composer.json not found in {}. Run `mozart init` to create one.",
            working_dir.display()
        );
    }

    // Read existing composer.json
    let mut raw = package::read_from_file(&composer_path)?;

    // Determine preferred stability from composer.json's minimum-stability
    let preferred_stability = raw
        .minimum_stability
        .as_deref()
        .map(|s| match s.to_lowercase().as_str() {
            "dev" => Stability::Dev,
            "alpha" => Stability::Alpha,
            "beta" => Stability::Beta,
            "rc" | "RC" => Stability::RC,
            _ => Stability::Stable,
        })
        .unwrap_or(Stability::Stable);

    // Process each package argument
    let mut additions: Vec<(String, String, bool)> = Vec::new(); // (name, constraint, is_dev)

    for pkg_arg in &args.packages {
        // Try to parse as "vendor/package:constraint"
        let (name, constraint) = match validation::parse_require_string(pkg_arg) {
            Ok((n, v)) => (n.to_lowercase(), v),
            Err(_) => {
                // No version specified — resolve from Packagist
                let name = pkg_arg.trim().to_lowercase();
                if !validation::validate_package_name(&name) {
                    anyhow::bail!("Invalid package name: \"{name}\"");
                }

                println!(
                    "{}",
                    console::info(&format!(
                        "Using version constraint for {name} from Packagist..."
                    ))
                );

                let versions = packagist::fetch_package_versions(&name)?;
                let best = version::find_best_candidate(&versions, preferred_stability)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Could not find a version of package \"{name}\" matching your minimum-stability ({preferred_stability:?}). \
                             Try requiring it with an explicit version constraint."
                        )
                    })?;

                let stability = version::stability_of(&best.version_normalized);
                let constraint = if args.fixed {
                    best.version.clone()
                } else {
                    version::find_recommended_require_version(
                        &best.version,
                        &best.version_normalized,
                        stability,
                    )
                };

                println!(
                    "{}",
                    console::info(&format!("Using version {constraint} for {name}"))
                );

                (name, constraint)
            }
        };

        additions.push((name, constraint, args.dev));
    }

    // Apply changes
    for (name, constraint, is_dev) in &additions {
        let section_name = if *is_dev { "require-dev" } else { "require" };
        let target = if *is_dev {
            &mut raw.require_dev
        } else {
            &mut raw.require
        };

        if let Some(existing) = target.get(name) {
            println!(
                "{}",
                console::comment(&format!(
                    "Updating {name} from {existing} to {constraint} in {section_name}"
                ))
            );
        } else {
            println!(
                "{}",
                console::info(&format!("Adding {name} ({constraint}) to {section_name}"))
            );
        }

        target.insert(name.clone(), constraint.clone());
    }

    // Sort packages if requested
    if args.sort_packages {
        let sorted_require: std::collections::BTreeMap<_, _> = raw.require.clone();
        raw.require = sorted_require;
        let sorted_dev: std::collections::BTreeMap<_, _> = raw.require_dev.clone();
        raw.require_dev = sorted_dev;
    }

    // Write back
    if args.dry_run {
        println!(
            "{}",
            console::comment("Dry run: composer.json not modified.")
        );
    } else {
        package::write_to_file(&raw, &composer_path)?;
    }

    // Dependency resolution / install notice
    if !args.no_update && !args.no_install {
        println!(
            "{}",
            console::comment(
                "Dependency resolution and installation are not yet implemented. \
                 The composer.json has been updated."
            )
        );
    }

    Ok(())
}
