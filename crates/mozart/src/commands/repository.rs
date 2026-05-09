use anyhow::anyhow;
use clap::Args;
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
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let action = args.action.as_deref().unwrap_or("list");
    let ctx = BaseConfigContext::initialize(args.global, args.file.as_deref(), cli)?;

    match action {
        "list" | "ls" | "show" => list_repositories(&ctx, console),
        "add" => execute_add(&ctx, args),
        "remove" | "rm" | "delete" => execute_remove(&ctx, args),
        "set-url" | "seturl" => execute_set_url(&ctx, args),
        "get-url" | "geturl" => execute_get_url(&ctx, args, console),
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
    console: &mozart_core::console::Console,
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
        console_writeln!(console, "No repositories configured");
        return Ok(());
    }

    for entry in &display_repos {
        if let Some(obj) = entry.as_object()
            && obj.len() == 1
            && let Some((key, val)) = obj.iter().next()
            && val == &serde_json::Value::Bool(false)
        {
            console_writeln!(console, "[{key}] disabled");
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

        console_writeln!(console, "[{name}] {repo_type} {url}");
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
    console: &mozart_core::console::Console,
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
            console_writeln!(console, "{}", url);
            return Ok(());
        }
        anyhow::bail!("The {} repository does not have a URL", name);
    }

    // List-format scan (mirrors Composer's fallback `foreach ($repos as $val)`).
    let repos = normalize_repositories(repos_raw);
    for repo in &repos {
        if repo.get("name").and_then(|n| n.as_str()) == Some(name) {
            if let Some(url) = repo.get("url").and_then(|u| u.as_str()) {
                console_writeln!(console, "{}", url);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_args(
        action: Option<&str>,
        name: Option<&str>,
        arg1: Option<&str>,
        arg2: Option<&str>,
    ) -> RepositoryArgs {
        RepositoryArgs {
            action: action.map(|s| s.to_string()),
            name: name.map(|s| s.to_string()),
            arg1: arg1.map(|s| s.to_string()),
            arg2: arg2.map(|s| s.to_string()),
            global: false,
            file: None,
            append: false,
            before: None,
            after: None,
        }
    }

    fn make_cli() -> super::super::Cli {
        use clap::Parser;
        super::super::Cli::parse_from(["mozart", "repository", "list"])
    }

    #[tokio::test]
    async fn test_list_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("list"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        // Empty repos → synthesises [packagist.org] disabled
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_with_repos() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "my-repo", "type": "vcs", "url": "https://example.com"}]}"#,
        ).unwrap();

        let mut args = make_args(Some("list"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_with_disabled_packagist() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, r#"{"repositories": [{"packagist.org": false}]}"#).unwrap();

        let mut args = make_args(Some("list"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_no_packagist_synth_when_composer_type_present() {
        // When a composer-type repo pointing at packagist.org is present,
        // no synthesised [packagist.org] disabled line should appear.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "packagist.org", "type": "composer", "url": "https://repo.packagist.org"}]}"#,
        )
        .unwrap();

        let mut args = make_args(Some("list"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_type_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(
            Some("add"),
            Some("my-repo"),
            Some("vcs"),
            Some("https://example.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["name"], "my-repo");
        assert_eq!(repos[0]["type"], "vcs");
        assert_eq!(repos[0]["url"], "https://example.com");
    }

    #[tokio::test]
    async fn test_add_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(
            Some("add"),
            Some("my-repo"),
            Some(r#"{"type":"path","url":"../local-pkg"}"#),
            None,
        );
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["type"], "path");
        assert_eq!(repos[0]["name"], "my-repo");
    }

    #[tokio::test]
    async fn test_add_prepend_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "existing", "type": "vcs", "url": "https://existing.com"}]}"#,
        ).unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new-repo"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "new-repo");
        assert_eq!(repos[1]["name"], "existing");
    }

    #[tokio::test]
    async fn test_add_append() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "existing", "type": "vcs", "url": "https://existing.com"}]}"#,
        ).unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new-repo"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());
        args.append = true;

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "existing");
        assert_eq!(repos[1]["name"], "new-repo");
    }

    #[tokio::test]
    async fn test_add_before() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "a", "type": "vcs", "url": "https://a.com"}, {"name": "b", "type": "vcs", "url": "https://b.com"}]}"#,
        ).unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());
        args.before = Some("b".to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "a");
        assert_eq!(repos[1]["name"], "new");
        assert_eq!(repos[2]["name"], "b");
    }

    #[tokio::test]
    async fn test_add_after() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "a", "type": "vcs", "url": "https://a.com"}, {"name": "b", "type": "vcs", "url": "https://b.com"}]}"#,
        ).unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());
        args.after = Some("a".to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "a");
        assert_eq!(repos[1]["name"], "new");
        assert_eq!(repos[2]["name"], "b");
    }

    #[tokio::test]
    async fn test_add_before_and_after_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());
        args.before = Some("a".to_string());
        args.after = Some("b".to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_missing_args() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("add"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_missing_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("add"), None, Some("vcs"), Some("https://url.com"));
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_remove() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "my-repo", "type": "vcs", "url": "https://example.com"}]}"#,
        ).unwrap();

        let mut args = make_args(Some("remove"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(json.get("repositories").is_none());
    }

    #[tokio::test]
    async fn test_remove_packagist_disables() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("remove"), Some("packagist.org"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["packagist.org"], false);
    }

    #[tokio::test]
    async fn test_remove_alias_rm() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "my-repo", "type": "vcs", "url": "https://example.com"}]}"#,
        ).unwrap();

        let mut args = make_args(Some("rm"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(json.get("repositories").is_none());
    }

    #[tokio::test]
    async fn test_remove_missing_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("remove"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "my-repo", "type": "vcs", "url": "https://old.com"}]}"#,
        )
        .unwrap();

        let mut args = make_args(
            Some("set-url"),
            Some("my-repo"),
            Some("https://new.com"),
            None,
        );
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["repositories"][0]["url"], "https://new.com");
    }

    #[tokio::test]
    async fn test_set_url_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, r#"{"repositories": []}"#).unwrap();

        let mut args = make_args(
            Some("set-url"),
            Some("missing"),
            Some("https://new.com"),
            None,
        );
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_url_alias() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "my-repo", "type": "vcs", "url": "https://old.com"}]}"#,
        )
        .unwrap();

        let mut args = make_args(
            Some("seturl"),
            Some("my-repo"),
            Some("https://new.com"),
            None,
        );
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["repositories"][0]["url"], "https://new.com");
    }

    #[tokio::test]
    async fn test_get_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "my-repo", "type": "vcs", "url": "https://example.com"}]}"#,
        ).unwrap();

        let mut args = make_args(Some("get-url"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_url_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, r#"{"repositories": []}"#).unwrap();

        let mut args = make_args(Some("get-url"), Some("missing"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_disable_packagist() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("disable"), Some("packagist.org"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["packagist.org"], false);
    }

    #[tokio::test]
    async fn test_disable_packagist_idempotent() {
        // Calling disable twice should not create a duplicate entry.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, r#"{"repositories": [{"packagist.org": false}]}"#).unwrap();

        let mut args = make_args(Some("disable"), Some("packagist.org"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 1, "should still be just one disable entry");
        assert_eq!(repos[0]["packagist.org"], false);
    }

    #[tokio::test]
    async fn test_disable_non_packagist_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("disable"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_disable_without_name_error() {
        // Composer requires a name for disable; Mozart mirrors that.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("disable"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_enable_packagist() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, r#"{"repositories": [{"packagist.org": false}]}"#).unwrap();

        let mut args = make_args(Some("enable"), Some("packagist.org"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(json.get("repositories").is_none());
    }

    #[tokio::test]
    async fn test_enable_non_packagist_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("enable"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_composer_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": {"my-repo": {"type": "vcs", "url": "https://example.com"}}}"#,
        )
        .unwrap();

        let mut args = make_args(Some("list"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_url_composer_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": {"my-repo": {"type": "vcs", "url": "https://example.com"}}}"#,
        )
        .unwrap();

        let mut args = make_args(Some("get-url"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_url_no_url_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": [{"name": "my-repo", "type": "artifact"}]}"#,
        )
        .unwrap();

        let mut args = make_args(Some("get-url"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("does not have a URL"),
            "unexpected message: {msg}"
        );
    }

    #[tokio::test]
    async fn test_get_url_not_found_message() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, r#"{"repositories": []}"#).unwrap();

        let mut args = make_args(Some("get-url"), Some("missing"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("There is no"), "unexpected message: {msg}");
    }

    #[tokio::test]
    async fn test_set_url_composer_format_keeps_assoc_shape() {
        // Composer's setRepositoryUrl mutates in place without converting assoc → list.
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": {"my-repo": {"type": "vcs", "url": "https://old.com"}}}"#,
        )
        .unwrap();

        let mut args = make_args(
            Some("set-url"),
            Some("my-repo"),
            Some("https://new.com"),
            None,
        );
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        // Format is preserved: still an assoc object.
        let repos = json["repositories"].as_object().unwrap();
        assert_eq!(repos["my-repo"]["url"], "https://new.com");
    }

    #[tokio::test]
    async fn test_remove_composer_format_converts_and_removes() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories": {"my-repo": {"type": "vcs", "url": "https://example.com"}}}"#,
        )
        .unwrap();

        let mut args = make_args(Some("remove"), Some("my-repo"), None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(json.get("repositories").is_none());
    }

    #[test]
    fn test_normalize_repositories_array_passthrough() {
        use super::super::config_helpers::normalize_repositories;
        let val = serde_json::json!([
            {"name": "foo", "type": "vcs", "url": "https://foo.com"}
        ]);
        let result = normalize_repositories(&val);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "foo");
    }

    #[test]
    fn test_normalize_repositories_object_injects_name() {
        use super::super::config_helpers::normalize_repositories;
        let val = serde_json::json!({
            "foo": {"type": "vcs", "url": "https://foo.com"},
            "bar": {"type": "composer", "url": "https://bar.com"}
        });
        let result = normalize_repositories(&val);
        assert_eq!(result.len(), 2);
        let names: indexmap::IndexSet<&str> = result
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains("foo"));
        assert!(names.contains("bar"));
    }

    #[test]
    fn test_normalize_repositories_object_boolean_entry() {
        use super::super::config_helpers::normalize_repositories;
        let val = serde_json::json!({"packagist.org": false});
        let result = normalize_repositories(&val);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["packagist.org"], false);
    }

    #[test]
    fn test_normalize_repositories_empty() {
        use super::super::config_helpers::normalize_repositories;
        let val = serde_json::json!(null);
        let result = normalize_repositories(&val);
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_unknown_action() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("invalid"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_insert_before() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories":[{"name":"a","type":"vcs","url":"https://a.com"},{"name":"b","type":"vcs","url":"https://b.com"}]}"#,
        )
        .unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());
        args.before = Some("b".to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "a");
        assert_eq!(repos[1]["name"], "new");
        assert_eq!(repos[2]["name"], "b");
    }

    #[tokio::test]
    async fn test_insert_after() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(
            &file,
            r#"{"repositories":[{"name":"a","type":"vcs","url":"https://a.com"},{"name":"b","type":"vcs","url":"https://b.com"}]}"#,
        )
        .unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());
        args.after = Some("a".to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        execute(&args, &cli, &console).await.unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "a");
        assert_eq!(repos[1]["name"], "new");
        assert_eq!(repos[2]["name"], "b");
    }

    #[tokio::test]
    async fn test_insert_target_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, r#"{"repositories": []}"#).unwrap();

        let mut args = make_args(
            Some("add"),
            Some("new"),
            Some("vcs"),
            Some("https://new.com"),
        );
        args.file = Some(file.to_str().unwrap().to_string());
        args.before = Some("nonexistent".to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        let result = execute(&args, &cli, &console).await;
        assert!(result.is_err());
    }
}
