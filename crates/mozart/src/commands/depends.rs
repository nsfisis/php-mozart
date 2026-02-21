use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct DependsArgs {
    /// Package to inspect
    pub package: String,

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

pub fn execute(args: &DependsArgs, cli: &super::Cli) -> anyhow::Result<()> {
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

    let target = args.package.to_lowercase();

    // Verify the target package is known
    let target_known = packages.iter().any(|p| p.name.to_lowercase() == target);
    if !target_known {
        anyhow::bail!(
            "Package '{}' not found in the dependency graph.",
            args.package
        );
    }

    let recursive = args.tree || args.recursive;
    let needles = vec![target];
    let results = super::dependency::get_dependents(&packages, &needles, None, false, recursive)?;

    if args.tree {
        super::dependency::print_tree(&results, 0);
    } else {
        super::dependency::print_table(&results);
    }

    Ok(())
}
