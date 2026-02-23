use anyhow::anyhow;
use clap::Args;
use std::path::PathBuf;

use super::config_helpers::{
    add_repository, composer_home, ensure_repositories_array, find_repo_by_name, insert_repository,
    normalize_repositories, read_json_file, remove_repository, render_value, working_dir,
    write_json_file,
};

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

fn resolve_file_path(args: &RepositoryArgs, cli: &super::Cli) -> anyhow::Result<PathBuf> {
    if args.global && args.file.is_some() {
        anyhow::bail!("Cannot combine --global and --file");
    }
    if args.global {
        return Ok(composer_home().join("config.json"));
    }
    if let Some(ref file) = args.file {
        return Ok(PathBuf::from(file));
    }
    Ok(working_dir(cli)?.join("composer.json"))
}

pub async fn execute(
    args: &RepositoryArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let action = args.action.as_deref().unwrap_or("list");

    match action {
        "list" | "ls" | "show" => execute_list(args, cli, console),
        "add" => execute_add(args, cli),
        "remove" | "rm" | "delete" => execute_remove(args, cli),
        "set-url" | "seturl" => execute_set_url(args, cli),
        "get-url" | "geturl" => execute_get_url(args, cli, console),
        "disable" => execute_disable(args, cli),
        "enable" => execute_enable(args, cli),
        _ => Err(anyhow!(
            "Unknown action \"{action}\". Expected one of: list, add, remove, set-url, get-url, enable, disable"
        )),
    }
}

// ─── list ─────────────────────────────────────────────────────────────────────

fn execute_list(
    args: &RepositoryArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let file_path = resolve_file_path(args, cli)?;
    let json = read_json_file(&file_path, args.global)?;

    let mut has_packagist_disable = false;

    let repos = normalize_repositories(&json["repositories"]);
    for entry in &repos {
        if let Some(obj) = entry.as_object() {
            // Check for disabled repo entry like {"packagist.org": false}
            if let Some((key, _)) = obj.iter().find(|(_, v)| v == &&serde_json::json!(false)) {
                console.write_stdout(
                    &format!("[{key}] disabled"),
                    mozart_core::console::Verbosity::Normal,
                );
                if key == "packagist.org" {
                    has_packagist_disable = true;
                }
                continue;
            }
        }

        let name = entry
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("unnamed");
        let repo_type = entry
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        let url = entry.get("url").and_then(|u| u.as_str()).unwrap_or("");

        console.write_stdout(
            &format!("[{name}] {repo_type} {url}"),
            mozart_core::console::Verbosity::Normal,
        );
    }

    if !has_packagist_disable {
        console.write_stdout(
            "[packagist.org] composer https://repo.packagist.org",
            mozart_core::console::Verbosity::Normal,
        );
    }

    Ok(())
}

// ─── add ──────────────────────────────────────────────────────────────────────

fn execute_add(args: &RepositoryArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Repository name is required for \"add\""))?;

    if args.before.is_some() && args.after.is_some() {
        anyhow::bail!("Cannot combine --before and --after");
    }

    let entry = match (&args.arg1, &args.arg2) {
        (Some(type_or_json), Some(url)) => {
            // type + url
            serde_json::json!({
                "name": name,
                "type": type_or_json,
                "url": url,
            })
        }
        (Some(json_str), None) => {
            // Try to parse as JSON
            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(mut parsed) => {
                    // Inject the name if not already present
                    if let Some(obj) = parsed.as_object_mut()
                        && !obj.contains_key("name")
                    {
                        obj.insert("name".to_string(), serde_json::json!(name));
                    }
                    parsed
                }
                Err(_) => {
                    anyhow::bail!(
                        "Invalid JSON for repository config. Expected: <type> <url> or a JSON string"
                    );
                }
            }
        }
        _ => {
            anyhow::bail!(
                "Missing arguments for \"add\". Expected: <name> <type> <url> or <name> <json>"
            );
        }
    };

    let file_path = resolve_file_path(args, cli)?;
    let mut json = read_json_file(&file_path, args.global)?;

    if let Some(ref target) = args.before {
        insert_repository(&mut json, name, entry, target, true)?;
    } else if let Some(ref target) = args.after {
        insert_repository(&mut json, name, entry, target, false)?;
    } else {
        add_repository(&mut json, name, entry, args.append);
    }

    write_json_file(&file_path, &json)?;
    Ok(())
}

// ─── remove ───────────────────────────────────────────────────────────────────

fn execute_remove(args: &RepositoryArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Repository name is required for \"remove\""))?;

    let file_path = resolve_file_path(args, cli)?;
    let mut json = read_json_file(&file_path, args.global)?;

    ensure_repositories_array(&mut json);

    if name == "packagist.org" || name == "packagist" {
        // Removing packagist.org means disabling it
        remove_repository(&mut json, "packagist.org");
        let disable_entry = serde_json::json!({"packagist.org": false});
        add_repository(&mut json, "packagist.org", disable_entry, args.append);
    } else {
        remove_repository(&mut json, name);
    }

    // Clean up empty repositories array
    if json["repositories"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(false)
        && let Some(obj) = json.as_object_mut()
    {
        obj.remove("repositories");
    }

    write_json_file(&file_path, &json)?;
    Ok(())
}

// ─── set-url ──────────────────────────────────────────────────────────────────

fn execute_set_url(args: &RepositoryArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Repository name is required for \"set-url\""))?;
    let new_url = args
        .arg1
        .as_deref()
        .ok_or_else(|| anyhow!("New URL is required for \"set-url\""))?;

    let file_path = resolve_file_path(args, cli)?;
    let mut json = read_json_file(&file_path, args.global)?;

    ensure_repositories_array(&mut json);

    let found = json["repositories"].as_array_mut().and_then(|repos| {
        repos
            .iter_mut()
            .find(|entry| entry.get("name").and_then(|n| n.as_str()) == Some(name))
    });

    match found {
        Some(entry) => {
            entry["url"] = serde_json::json!(new_url);
            write_json_file(&file_path, &json)?;
            Ok(())
        }
        None => Err(anyhow!("Repository \"{name}\" not found")),
    }
}

// ─── get-url ──────────────────────────────────────────────────────────────────

fn execute_get_url(
    args: &RepositoryArgs,
    cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("Repository name is required for \"get-url\""))?;

    let file_path = resolve_file_path(args, cli)?;
    let json = read_json_file(&file_path, args.global)?;

    let repos = normalize_repositories(&json["repositories"]);

    match find_repo_by_name(&repos, name) {
        Some(idx) => {
            let entry = &repos[idx];
            match entry.get("url") {
                Some(url_val) => {
                    console.write_stdout(
                        &render_value(url_val),
                        mozart_core::console::Verbosity::Normal,
                    );
                    Ok(())
                }
                None => Err(anyhow!("The \"{name}\" repository does not have a URL")),
            }
        }
        None => Err(anyhow!("There is no \"{name}\" repository defined")),
    }
}

// ─── disable ──────────────────────────────────────────────────────────────────

fn execute_disable(args: &RepositoryArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let name = args.name.as_deref().unwrap_or("packagist.org");

    if name != "packagist.org" && name != "packagist" {
        anyhow::bail!("Only \"packagist.org\" can be disabled with this action");
    }

    let file_path = resolve_file_path(args, cli)?;
    let mut json = read_json_file(&file_path, args.global)?;

    // Remove any existing packagist.org disable entry first
    remove_repository(&mut json, "packagist.org");

    let disable_entry = serde_json::json!({"packagist.org": false});
    add_repository(&mut json, "packagist.org", disable_entry, args.append);

    write_json_file(&file_path, &json)?;
    Ok(())
}

// ─── enable ───────────────────────────────────────────────────────────────────

fn execute_enable(args: &RepositoryArgs, cli: &super::Cli) -> anyhow::Result<()> {
    let name = args.name.as_deref().unwrap_or("packagist.org");

    if name != "packagist.org" && name != "packagist" {
        anyhow::bail!("Only \"packagist.org\" can be enabled with this action");
    }

    let file_path = resolve_file_path(args, cli)?;
    let mut json = read_json_file(&file_path, args.global)?;

    remove_repository(&mut json, "packagist.org");

    // Clean up empty repositories array
    if json["repositories"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(false)
        && let Some(obj) = json.as_object_mut()
    {
        obj.remove("repositories");
    }

    write_json_file(&file_path, &json)?;
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

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

    // ── list ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("list"), None, None, None);
        args.file = Some(file.to_str().unwrap().to_string());

        let cli = make_cli();
        let console = mozart_core::console::Console::new(0, false, false, false, false);
        // Should succeed and print packagist.org
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

    // ── add ─────────────────────────────────────────────────────────────────

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

    // ── remove ──────────────────────────────────────────────────────────────

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

    // ── set-url ─────────────────────────────────────────────────────────────

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

    // ── get-url ─────────────────────────────────────────────────────────────

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

    // ── disable ─────────────────────────────────────────────────────────────

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
    async fn test_disable_without_name_defaults_to_packagist() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("composer.json");
        std::fs::write(&file, "{}").unwrap();

        let mut args = make_args(Some("disable"), None, None, None);
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

    // ── enable ──────────────────────────────────────────────────────────────

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

    // ── Composer associative-key format ─────────────────────────────────────

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
    async fn test_set_url_composer_format_converts_and_updates() {
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
        // After conversion it should be an array
        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["url"], "https://new.com");
        assert_eq!(repos[0]["name"], "my-repo");
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

    // ── normalize_repositories helper ────────────────────────────────────────

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
        let names: std::collections::HashSet<&str> = result
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

    // ── unknown action ──────────────────────────────────────────────────────

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

    // ── insert_repository helper ────────────────────────────────────────────

    #[test]
    fn test_insert_before() {
        let mut json = serde_json::json!({
            "repositories": [
                {"name": "a", "type": "vcs", "url": "https://a.com"},
                {"name": "b", "type": "vcs", "url": "https://b.com"},
            ]
        });

        let entry = serde_json::json!({"name": "new", "type": "vcs", "url": "https://new.com"});
        insert_repository(&mut json, "new", entry, "b", true).unwrap();

        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "a");
        assert_eq!(repos[1]["name"], "new");
        assert_eq!(repos[2]["name"], "b");
    }

    #[test]
    fn test_insert_after() {
        let mut json = serde_json::json!({
            "repositories": [
                {"name": "a", "type": "vcs", "url": "https://a.com"},
                {"name": "b", "type": "vcs", "url": "https://b.com"},
            ]
        });

        let entry = serde_json::json!({"name": "new", "type": "vcs", "url": "https://new.com"});
        insert_repository(&mut json, "new", entry, "a", false).unwrap();

        let repos = json["repositories"].as_array().unwrap();
        assert_eq!(repos[0]["name"], "a");
        assert_eq!(repos[1]["name"], "new");
        assert_eq!(repos[2]["name"], "b");
    }

    #[test]
    fn test_insert_target_not_found() {
        let mut json = serde_json::json!({"repositories": []});
        let entry = serde_json::json!({"name": "new", "type": "vcs", "url": "https://new.com"});
        let result = insert_repository(&mut json, "new", entry, "nonexistent", true);
        assert!(result.is_err());
    }
}
