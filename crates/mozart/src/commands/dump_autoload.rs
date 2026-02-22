use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct DumpAutoloadArgs {
    /// Optimizes PSR-0 and PSR-4 packages to be loaded with classmaps
    #[arg(short, long)]
    pub optimize: bool,

    /// Autoload classes from the classmap only
    #[arg(short = 'a', long)]
    pub classmap_authoritative: bool,

    /// Use APCu to cache found/not-found classes
    #[arg(long)]
    pub apcu: bool,

    /// Use a custom prefix for the APCu autoloader cache
    #[arg(long)]
    pub apcu_prefix: Option<String>,

    /// Only output what would be changed, do not modify files
    #[arg(long)]
    pub dry_run: bool,

    /// Enables autoload-dev rules
    #[arg(long)]
    pub dev: bool,

    /// Disables autoload-dev rules
    #[arg(long)]
    pub no_dev: bool,

    /// Ignore a specific platform requirement
    #[arg(long)]
    pub ignore_platform_req: Vec<String>,

    /// Ignore all platform requirements
    #[arg(long)]
    pub ignore_platform_reqs: bool,

    /// Return a failed status code if there are PSR mapping errors
    #[arg(long)]
    pub strict_psr: bool,

    /// Return a failed status code if there are ambiguous class mappings
    #[arg(long)]
    pub strict_ambiguous: bool,
}

pub async fn execute(
    args: &DumpAutoloadArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    // A: --dev / --no-dev conflict detection
    if args.dev && args.no_dev {
        anyhow::bail!("You can not use both --no-dev and --dev as they conflict with each other.");
    }

    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let vendor_dir = working_dir.join("vendor");
    let dev_mode = !args.no_dev;

    // B: Read config-driven defaults from composer.json
    let composer_json_path = working_dir.join("composer.json");
    let mut composer_config = super::config::ComposerConfig::defaults();
    if composer_json_path.exists()
        && let Ok(content) = std::fs::read_to_string(&composer_json_path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(cfg_obj) = value.get("config").and_then(|v| v.as_object()) {
                    let overrides: std::collections::BTreeMap<String, serde_json::Value> = cfg_obj
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    composer_config.merge(&overrides);
                }

    let optimize = args.optimize
        || composer_config
            .get("optimize-autoloader")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    let classmap_authoritative = args.classmap_authoritative
        || composer_config
            .get("classmap-authoritative")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    let apcu = args.apcu
        || args.apcu_prefix.is_some()
        || composer_config
            .get("apcu-autoloader")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    // C: Validate --strict-psr and --strict-ambiguous require optimize
    let effective_optimize = optimize || classmap_authoritative;
    if args.strict_psr && !effective_optimize {
        anyhow::bail!(
            "--strict-psr mode only works with optimized autoloader, use --optimize or --classmap-authoritative."
        );
    }
    if args.strict_ambiguous && !effective_optimize {
        anyhow::bail!(
            "--strict-ambiguous mode only works with optimized autoloader, use --optimize or --classmap-authoritative."
        );
    }

    // D: Pre-generation output message
    if classmap_authoritative {
        console.info("Generating optimized autoload files (authoritative)");
    } else if optimize {
        console.info("Generating optimized autoload files");
    } else {
        console.info("Generating autoload files");
    }

    // Determine suffix: read from existing autoload.php, or from lock file, or generate
    let suffix = mozart_autoload::autoload::determine_suffix(&working_dir, &vendor_dir)?;

    if args.dry_run {
        console.info("Dry run: would generate autoload files");
        return Ok(());
    }

    // E: AutoloadConfig construction using config-merged values
    let autoload_config = mozart_autoload::autoload::AutoloadConfig {
        project_dir: working_dir,
        vendor_dir,
        dev_mode,
        suffix,
        classmap_authoritative,
        optimize,
        apcu,
        apcu_prefix: args.apcu_prefix.clone(),
        strict_psr: args.strict_psr,
        strict_ambiguous: args.strict_ambiguous,
        platform_check: mozart_autoload::autoload::PlatformCheckMode::Full,
        ignore_platform_reqs: args.ignore_platform_reqs,
    };

    // F: Handle GenerateResult and post-generation messages
    let result = mozart_autoload::autoload::generate(&autoload_config)?;

    if effective_optimize || classmap_authoritative {
        console.info(&format!(
            "Generated optimized autoload files containing {} classes",
            result.class_count
        ));
    } else {
        console.info("Generated autoload files");
    }

    if args.strict_ambiguous && result.has_ambiguous_classes {
        std::process::exit(2);
    }

    Ok(())
}
