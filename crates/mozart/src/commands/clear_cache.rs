use crate::cache::{Cache, build_cache_config};
use clap::Args;

#[derive(Args)]
pub struct ClearCacheArgs {
    /// Only run garbage collection, not a full cache clear
    #[arg(long)]
    pub gc: bool,
}

pub fn execute(args: &ClearCacheArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let config = build_cache_config(cli);

    if args.gc {
        // Run GC only (probabilistic under normal circumstances, but forced here)
        let repo_cache = Cache::repo(&config);
        let files_cache = Cache::files(&config);

        repo_cache.gc(config.cache_ttl, u64::MAX)?;
        files_cache.gc(config.cache_files_ttl, config.cache_files_maxsize)?;

        eprintln!("Cache garbage collection complete.");
        eprintln!("Cache directory: {}", config.cache_dir.display());
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

        eprintln!("Cache cleared.");
        eprintln!("Cache directory: {}", config.cache_dir.display());
    }

    Ok(())
}
