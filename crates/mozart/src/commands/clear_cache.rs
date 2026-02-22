use clap::Args;
use mozart_registry::cache::{Cache, build_cache_config};

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
    let config = build_cache_config(cli.no_cache);

    if args.gc {
        // Run GC only (probabilistic under normal circumstances, but forced here)
        let repo_cache = Cache::repo(&config);
        let files_cache = Cache::files(&config);

        // Composer enforces a 1 GB cap on the repo cache during GC
        repo_cache.gc(config.cache_ttl, 1024 * 1024 * 1024)?;
        files_cache.gc(config.cache_files_ttl, config.cache_files_maxsize)?;

        console.info("Cache garbage collection complete.");
        console.info(&format!("Cache directory: {}", config.cache_dir.display()));
    } else {
        // Full clear of all cache directories
        let repo_cache = Cache::repo(&config);
        let files_cache = Cache::files(&config);
        repo_cache.clear()?;
        files_cache.clear()?;
        // Clear anything else at the root that isn't covered by sub-caches
        if config.cache_dir.exists() {
            for entry in std::fs::read_dir(&config.cache_dir)? {
                let entry = entry?;
                let path = entry.path();
                // Skip repo/files subdirs (already cleared above)
                if path == config.cache_files_dir || path == config.cache_repo_dir {
                    continue;
                }
                if path.is_file() {
                    std::fs::remove_file(&path)?;
                } else if path.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                }
            }
        }

        console.info("Cache cleared.");
        console.info(&format!("Cache directory: {}", config.cache_dir.display()));
    }

    Ok(())
}
