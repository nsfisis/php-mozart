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

    // Build the list of (key, path) pairs to process.
    // cache-dir is only included in full clear mode, not GC mode.
    let mut cache_paths: Vec<(&str, &std::path::PathBuf)> = vec![
        ("cache-repo-dir", &config.cache_repo_dir),
        ("cache-files-dir", &config.cache_files_dir),
    ];
    if !args.gc {
        cache_paths.push(("cache-dir", &config.cache_dir));
    }

    for (key, path) in &cache_paths {
        // Read-only guard: skip with informational message
        if config.read_only {
            console.info(&format!("Cache is not enabled ({key}): {}", path.display()));
            continue;
        }

        // Non-existent directory: skip with informational message
        if !path.exists() {
            console.info(&format!(
                "Cache directory does not exist ({key}): {}",
                path.display()
            ));
            continue;
        }

        if args.gc {
            console.info(&format!(
                "Garbage-collecting cache ({key}): {}",
                path.display()
            ));
            let cache = Cache::new((*path).clone(), !config.no_cache);
            let result = if *key == "cache-files-dir" {
                cache.gc(config.cache_files_ttl, config.cache_files_maxsize)
            } else {
                // cache-repo-dir: 1 GB cap (matches Composer)
                cache.gc(config.cache_ttl, 1024 * 1024 * 1024)
            };
            if let Err(e) = result {
                console.error(&format!("Error during GC of {key}: {e}"));
            }
        } else {
            console.info(&format!("Clearing cache ({key}): {}", path.display()));
            if *key == "cache-dir" {
                // Clear anything at the root that isn't covered by sub-caches
                let result = (|| -> anyhow::Result<()> {
                    for entry in std::fs::read_dir(path)? {
                        let entry = entry?;
                        let entry_path = entry.path();
                        // Skip repo/files subdirs (cleared by their own iterations)
                        if entry_path == config.cache_files_dir
                            || entry_path == config.cache_repo_dir
                        {
                            continue;
                        }
                        if entry_path.is_file() {
                            std::fs::remove_file(&entry_path)?;
                        } else if entry_path.is_dir() {
                            std::fs::remove_dir_all(&entry_path)?;
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    console.error(&format!("Error clearing {key}: {e}"));
                }
            } else {
                let cache = Cache::new((*path).clone(), !config.no_cache);
                if let Err(e) = cache.clear() {
                    console.error(&format!("Error clearing {key}: {e}"));
                }
            }
        }
    }

    if args.gc {
        console.info("All caches garbage-collected.");
    } else {
        console.info("All caches cleared.");
    }

    Ok(())
}
