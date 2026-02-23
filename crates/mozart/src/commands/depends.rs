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

pub async fn execute(
    args: &DependsArgs,
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

    // Verify the target package is known
    let target_known = packages.iter().any(|p| p.name.to_lowercase() == target);
    if !target_known && mozart_core::platform::is_platform_package(&target) {
        anyhow::bail!(
            "Could not find platform package \"{}\". Is PHP available?",
            args.package
        );
    }
    if !target_known {
        anyhow::bail!(
            "Could not find package \"{}\" in your project",
            args.package
        );
    }

    let recursive = args.tree || args.recursive;
    let needles = vec![target];
    let results = super::dependency::get_dependents(&packages, &needles, None, false, recursive)?;

    if results.is_empty() {
        console.info(&format!(
            "There is no installed package depending on \"{}\"",
            args.package
        ));
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    if args.tree {
        super::dependency::print_tree(&results, 0, console);
    } else {
        super::dependency::print_table(&results, console);
    }

    Ok(())
}
