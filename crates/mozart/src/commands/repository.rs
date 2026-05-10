use anyhow::anyhow;
use clap::Args;
use mozart_core::console::IoInterface;
use mozart_core::console_writeln;

use super::base_config::BaseConfigContext;
use super::config_helpers::{normalize_repositories, render_value};

#[derive(Args)]
pub struct RepositoryArgs {
    /// Action (list, add, remove, set-url, get-url, enable, disable)
    pub action: Option<String>,

    /// Repository name
    pub name: Option<String>,

    /// Argument 1 (URL or type depending on action)
    pub arg1: Option<String>,

    /// Argument 2
    pub arg2: Option<String>,

    /// Apply to the global config file
    #[arg(short, long)]
    pub global: bool,

    /// Use a specific config file
    #[arg(short, long)]
    pub file: Option<String>,

    /// Append the repository instead of prepending it
    #[arg(long)]
    pub append: bool,

    /// Add before a specific repository
    #[arg(long)]
    pub before: Option<String>,

    /// Add after a specific repository
    #[arg(long)]
    pub after: Option<String>,
}

pub async fn execute(
    args: &RepositoryArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let action = args.action.as_deref().unwrap_or("list");
    let ctx = BaseConfigContext::initialize(args.global, args.file.as_deref(), cli)?;

    match action {
        "list" | "ls" | "show" => list_repositories(&ctx, io.clone()),
        "add" => execute_add(&ctx, args),
        "remove" | "rm" | "delete" => execute_remove(&ctx, args),
        "set-url" | "seturl" => execute_set_url(&ctx, args),
        "get-url" | "geturl" => execute_get_url(&ctx, args, io.clone()),
        "disable" => execute_disable(&ctx, args),
        "enable" => execute_enable(&ctx, args),
        _ => Err(anyhow!(
            "Unknown action \"{action}\". Use list, add, remove, set-url, get-url, enable, disable"
        )),
    }
}

/// Mirror of Composer's `RepositoryCommand::listRepositories`.
///
/// Synthesises a `[packagist.org] <disabled>` line only when no `composer`-type
/// repository with a host ending in `packagist.org` is already in the list.
fn list_repositories(
    ctx: &BaseConfigContext,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let json = ctx.config_source.read()?;
    let repos_raw = &json["repositories"];
    let repos = normalize_repositories(repos_raw);

    let packagist_present = repos.iter().any(|entry| {
        entry.get("type").and_then(|t| t.as_str()) == Some("composer")
            && entry
                .get("url")
                .and_then(|u| u.as_str())
                .map(host_ends_with_packagist_org)
                .unwrap_or(false)
    });

    // When no packagist.org-hosted composer repo is present, synthesise the
    // disabled-packagist line exactly as Composer does (appending it to the list
    // for display purposes only — not written to disk).
    let mut display_repos = repos;
    if !packagist_present {
        let mut m = serde_json::Map::new();
        m.insert("packagist.org".to_string(), serde_json::Value::Bool(false));
        display_repos.push(serde_json::Value::Object(m));
    }

    if display_repos.is_empty() {
        console_writeln!(io, "No repositories configured");
        return Ok(());
    }

    for entry in &display_repos {
        if let Some(obj) = entry.as_object()
            && obj.len() == 1
            && let Some((key, val)) = obj.iter().next()
            && val == &serde_json::Value::Bool(false)
        {
            console_writeln!(io, "[{key}] disabled");
            continue;
        }

        let name = entry
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("unnamed");
        let repo_type = entry
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        let url = entry.get("url").map(render_value).unwrap_or_default();

        console_writeln!(io, "[{name}] {repo_type} {url}");
    }

    Ok(())
}

fn host_ends_with_packagist_org(url: &str) -> bool {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_port = after_scheme.split('/').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    host == "packagist.org" || host.ends_with(".packagist.org")
}

fn execute_add(ctx: &BaseConfigContext, args: &RepositoryArgs) -> anyhow::Result<()> {
    let name = args.name.as_deref().ok_or_else(|| {
        anyhow!(
            "You must pass a repository name. Example: mozart repo add foo vcs https://example.org"
        )
    })?;

    let arg1 = args
        .arg1
        .as_deref()
        .ok_or_else(|| anyhow!("You must pass the type and a url, or a JSON string."))?;

    // Mirror Composer's `Preg::isMatch('{^\s*\{}', $arg1)` check.
    let repo_config = if arg1.trim_start().starts_with('{') {
        serde_json::from_str::<serde_json::Value>(arg1)
            .map_err(|e| anyhow!("Invalid JSON: {}", e))?
    } else {
        let url = args.arg2.as_deref().ok_or_else(|| {
            anyhow!("You must pass the type and a url. Example: mozart repo add foo vcs https://example.org")
        })?;
        serde_json::json!({"type": arg1, "url": url})
    };

    if args.before.is_some() && args.after.is_some() {
        anyhow::bail!("You can not combine --before and --after");
    }

    if let Some(ref target) = args.before {
        ctx.config_source
            .insert_repository(name, &repo_config, target, 0)?;
    } else if let Some(ref target) = args.after {
        ctx.config_source
            .insert_repository(name, &repo_config, target, 1)?;
    } else {
        ctx.config_source
            .add_repository(name, &repo_config, args.append)?;
    }

    Ok(())
}

fn execute_remove(ctx: &BaseConfigContext, args: &RepositoryArgs) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("You must pass the repository name to remove."))?;

    ctx.config_source.remove_repository(name)?;
    if name == "packagist.org" || name == "packagist" {
        // Removing packagist means disabling it (Composer behaviour).
        // Default append=false so the disable entry goes to the front when
        // the user didn't pass --append.
        ctx.config_source.add_repository(
            "packagist.org",
            &serde_json::Value::Bool(false),
            args.append,
        )?;
    }

    Ok(())
}

fn execute_set_url(ctx: &BaseConfigContext, args: &RepositoryArgs) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Usage: mozart repo set-url <name> <new-url>"))?;
    let new_url = args
        .arg1
        .as_deref()
        .ok_or_else(|| anyhow!("Usage: mozart repo set-url <name> <new-url>"))?;

    ctx.config_source.set_repository_url(name, new_url)?;
    Ok(())
}

fn execute_get_url(
    ctx: &BaseConfigContext,
    args: &RepositoryArgs,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Usage: mozart repo get-url <name>"))?;

    let json = ctx.config_source.read()?;
    let repos_raw = &json["repositories"];

    // Assoc-keyed fast path (mirrors Composer's `isset($repos[$name])` check).
    if let Some(repo) = repos_raw.as_object().and_then(|obj| obj.get(name)) {
        if let Some(url) = repo.get("url").and_then(|u| u.as_str()) {
            console_writeln!(io, "{}", url);
            return Ok(());
        }
        anyhow::bail!("The {} repository does not have a URL", name);
    }

    // List-format scan (mirrors Composer's fallback `foreach ($repos as $val)`).
    let repos = normalize_repositories(repos_raw);
    for repo in &repos {
        if repo.get("name").and_then(|n| n.as_str()) == Some(name) {
            if let Some(url) = repo.get("url").and_then(|u| u.as_str()) {
                console_writeln!(io, "{}", url);
                return Ok(());
            }
            anyhow::bail!("The {} repository does not have a URL", name);
        }
    }

    Err(anyhow!("There is no {} repository defined", name))
}

fn execute_disable(ctx: &BaseConfigContext, args: &RepositoryArgs) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Usage: mozart repo disable packagist.org"))?;

    if name == "packagist.org" || name == "packagist" {
        ctx.config_source.add_repository(
            "packagist.org",
            &serde_json::Value::Bool(false),
            args.append,
        )?;
        return Ok(());
    }

    anyhow::bail!(
        "Only packagist.org can be enabled/disabled using this command. Use add/remove for other repositories."
    );
}

fn execute_enable(ctx: &BaseConfigContext, args: &RepositoryArgs) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Usage: mozart repo enable packagist.org"))?;

    if name == "packagist.org" || name == "packagist" {
        // Just remove the disable override; Composer does nothing else here.
        ctx.config_source.remove_repository("packagist.org")?;
        return Ok(());
    }

    anyhow::bail!("Only packagist.org can be enabled/disabled using this command.");
}
