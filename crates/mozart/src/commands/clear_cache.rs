use std::{borrow::Cow, path::Path};

use clap::Args;
use mozart_core::composer::Composer;
use mozart_core::console_writeln_error;
use mozart_core::factory::create_config;
use mozart_registry::cache::Cache;

#[derive(Args)]
pub struct ClearCacheArgs {
    /// Only run garbage collection, not a full cache clear
    #[arg(long)]
    pub gc: bool,
}

pub async fn execute(
    args: &ClearCacheArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let composer = Composer::try_load(cli.working_dir()?)?;
    let config = if let Some(composer) = &composer {
        Cow::Borrowed(composer.config())
    } else {
        Cow::Owned(create_config()?)
    };

    let cache_paths = [
        ("cache-vcs-dir", &config.cache_vcs_dir),
        ("cache-repo-dir", &config.cache_repo_dir),
        ("cache-files-dir", &config.cache_files_dir),
        ("cache-dir", &config.cache_dir),
    ];

    for (key, path) in cache_paths {
        // only individual dirs get garbage collected
        if key == "cache-dir" && args.gc {
            continue;
        }

        let path = Path::new(path);

        if !path.exists() {
            console_writeln_error!(
                console,
                "<info>Cache directory does not exist ({key}): {}</info>",
                path.display(),
            );
            continue;
        }

        let cache = Cache::new(path.to_owned(), config.cache_read_only);
        if !cache.is_enabled() {
            console_writeln_error!(
                console,
                "<info>Cache is not enabled ({key}): {}</info>",
                path.display(),
            );
            continue;
        }

        if args.gc {
            console_writeln_error!(
                console,
                "<info>Garbage-collecting cache ({key}): {}</info>",
                path.display(),
            );
            match key {
                "cache-files-dir" => cache.gc(config.cache_files_ttl, config.cache_files_maxsize)?,
                "cache-repo-dir" => cache.gc(config.cache_files_ttl, 1024 * 1024 * 1024 /* 1GB, this should almost never clear anything that is not outdated */)?,
                "cache-vcs-dir" => cache.gc_vcs(config.cache_files_ttl)?,
                _ => unreachable!(),
            };
        } else {
            console_writeln_error!(
                console,
                "<info>Clearing cache ({key}): {}</info>",
                path.display(),
            );
            cache.clear()?;
        }
    }

    if args.gc {
        console_writeln_error!(console, "<info>All caches garbage-collected.</info>");
    } else {
        console_writeln_error!(console, "<info>All caches cleared.</info>");
    }

    Ok(())
}
