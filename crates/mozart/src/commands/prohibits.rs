use clap::Args;
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

pub fn execute(args: &ProhibitsArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let packages = super::dependency::load_packages(&working_dir, args.locked)?;

    if packages.is_empty() {
        println!(
            "{}",
            crate::console::info("No packages found. Run `mozart install` first.")
        );
        return Ok(());
    }

    // Parse the version constraint the user is asking about
    let version_constraint = crate::constraint::VersionConstraint::parse(&args.version)
        .map_err(|e| anyhow::anyhow!("Invalid version constraint '{}': {}", args.version, e))?;

    let recursive = args.tree || args.recursive;
    let target = args.package.to_lowercase();
    let needles = vec![target];

    let results = super::dependency::get_dependents(
        &packages,
        &needles,
        Some(&version_constraint),
        true, // inverted = prohibits mode
        recursive,
    )?;

    if results.is_empty() {
        println!(
            "{}",
            crate::console::info(&format!(
                "{} {} can be installed.",
                args.package, args.version
            ))
        );
        return Ok(());
    }

    if args.tree {
        super::dependency::print_tree(&results, 0);
    } else {
        super::dependency::print_table(&results);
    }

    Ok(())
}
