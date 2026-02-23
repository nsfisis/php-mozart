use clap::Args;
use mozart_core::console_format;
use std::path::PathBuf;

#[derive(Args)]
pub struct ProhibitsArgs {
    /// Package to inspect
    pub package: String,

    /// Version constraint
    pub version: String,

    /// Recursively resolve up to the root package
    #[arg(short, long)]
    pub recursive: bool,

    /// Prints the results as a nested tree
    #[arg(short, long)]
    pub tree: bool,

    /// Read dependency information from the lock file
    #[arg(long)]
    pub locked: bool,
}

pub async fn execute(
    args: &ProhibitsArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let packages = super::dependency::load_packages(&working_dir, args.locked)?;

    if packages.is_empty() {
        console.write_error(
            "No dependencies installed. Try running mozart install or update, or use --locked.",
        );
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    let target = args.package.to_lowercase();

    // Fix #2: Verify the target package is known
    let target_known = packages.iter().any(|p| p.name.to_lowercase() == target);
    if !target_known {
        anyhow::bail!(
            "Could not find package \"{}\" in your project",
            args.package
        );
    }

    // Parse the version constraint the user is asking about
    let version_constraint = mozart_semver::VersionConstraint::parse(&args.version)
        .map_err(|e| anyhow::anyhow!("Invalid version constraint '{}': {}", args.version, e))?;

    let recursive = args.tree || args.recursive;
    let needles = vec![target];

    let results = super::dependency::get_dependents(
        &packages,
        &needles,
        Some(&version_constraint),
        true, // inverted = prohibits mode
        recursive,
    )?;

    if results.is_empty() {
        console.write_stdout(
            &console_format!(
                "<info>{} {} can be installed.</info>",
                args.package,
                args.version
            ),
            mozart_core::console::Verbosity::Normal,
        );
        return Ok(());
    }

    if args.tree {
        super::dependency::print_tree(&results, 0, console);
    } else {
        super::dependency::print_table(&results, console);
    }

    // Fix #5: Print resolution hint message
    // Determine the appropriate composer command based on whether the needle
    // is in root's require or require-dev.
    let needle_lower = args.package.to_lowercase();
    let composer_command = packages
        .iter()
        .find(|p| p.is_root)
        .map(|root| {
            if root
                .require
                .keys()
                .any(|k| k.to_lowercase() == needle_lower)
            {
                "require"
            } else if root
                .require_dev
                .keys()
                .any(|k| k.to_lowercase() == needle_lower)
            {
                "require --dev"
            } else {
                "update"
            }
        })
        .unwrap_or("update");

    console.info(&format!(
        "Not finding what you were looking for? Try calling `composer {} \"{}:{}\" --dry-run` to get another view on the problem.",
        composer_command, args.package, args.version
    ));

    // Fix #3: Return exit code 1 when prohibitors are found
    Err(mozart_core::exit_code::bail_silent(
        mozart_core::exit_code::GENERAL_ERROR,
    ))
}
