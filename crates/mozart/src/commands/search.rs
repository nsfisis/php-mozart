use clap::Args;
use mozart_core::console::{Console, hyperlink};
use mozart_core::console_format;
use mozart_core::console_writeln;
use mozart_registry::packagist::SearchResult;
use mozart_registry::repository::{RepositorySet, SearchMode};
use serde::Serialize;

/// JSON output structure matching Composer's search result schema.
///
/// Composer outputs only `name`, `description`, `url`, and optionally `abandoned`.
#[derive(Serialize)]
struct SearchResultOutput {
    name: String,
    description: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    abandoned: Option<serde_json::Value>,
}

impl From<&SearchResult> for SearchResultOutput {
    fn from(r: &SearchResult) -> Self {
        Self {
            name: r.name.clone(),
            description: r.description.clone(),
            url: r.url.clone(),
            abandoned: r.abandoned.clone(),
        }
    }
}

#[derive(Args)]
pub struct SearchArgs {
    /// Search tokens
    #[arg(required = true)]
    pub tokens: Vec<String>,

    /// Search only in name
    #[arg(short = 'N', long)]
    pub only_name: bool,

    /// Search only for vendor / organization
    #[arg(short = 'O', long)]
    pub only_vendor: bool,

    /// Filter by package type
    #[arg(short, long, value_name = "TYPE")]
    pub r#type: Option<String>,

    /// Output format (text, json)
    #[arg(short, long)]
    pub format: Option<String>,
}

/// Returns true if the search result represents an abandoned package.
///
/// The `abandoned` field from the Packagist API can be:
/// - absent (`None`) — not abandoned
/// - a non-empty string — abandoned, with a replacement package name
/// - `true` — abandoned, no replacement
/// - an empty string or `false` — not abandoned
fn is_abandoned(result: &SearchResult) -> bool {
    match &result.abandoned {
        None => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub async fn execute(args: &SearchArgs, cli: &super::Cli, console: &Console) -> anyhow::Result<()> {
    // 1. Format check first — matches Composer's `SearchCommand::execute`
    //    L61-66 ordering.
    let format = args.format.as_deref().unwrap_or("text");
    if !matches!(format, "text" | "json") {
        console.error(&console_format!(
            "<error>Unsupported format \"{format}\". See help for supported formats.</error>"
        ));
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    // 2. Mutex check on the two scoping flags. Composer's
    //    `RepositoryFactory::generateRepositoryManager` precedes this with
    //    `tryComposer`; we skip until configured-repos support lands.
    if args.only_name && args.only_vendor {
        anyhow::bail!("--only-name and --only-vendor cannot be used together");
    }

    // 3. Mode resolution. Composer checks `--only-vendor` before `--only-name`
    //    (`SearchCommand::execute` L78-86), so vendor wins if both are set —
    //    but the mutex check above already guards that.
    let mode = if args.only_vendor {
        SearchMode::Vendor
    } else if args.only_name {
        SearchMode::Name
    } else {
        SearchMode::Fulltext
    };

    // 4. Build the query string. Composer joins tokens with a single space
    //    and `preg_quote`s the result for non-fulltext modes so that user
    //    input like `c++` is matched literally rather than as regex
    //    metacharacters.
    let mut query = args.tokens.join(" ");
    if !matches!(mode, SearchMode::Fulltext) {
        query = regex::escape(&query);
    }

    // 5. Build the repository set. Configured remote repositories from
    //    `composer.json` are not yet wired up; this is a known divergence
    //    from Composer's full `CompositeRepository`.
    let cache_config = mozart_registry::cache::build_cache_config(cli.no_cache);
    let repo_cache = mozart_registry::cache::Cache::repo(&cache_config);
    let repos = RepositorySet::with_packagist(repo_cache);

    // 6. Dispatch.
    let results = repos.search(&query, mode, args.r#type.as_deref()).await?;

    // 7. Render. Empty results emit nothing in text mode (matches Composer)
    //    and `[]` in JSON mode.
    match format {
        "json" => render_json(&results, console)?,
        _ => render_text(&results, console),
    }

    Ok(())
}

/// Render results as JSON with 4-space indent, matching Composer's
/// `JsonFile::encode` output (`JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES |
/// JSON_UNESCAPED_UNICODE`). `serde_json` does not escape forward slashes
/// or non-ASCII Unicode by default, so the encoder configuration alone
/// covers the latter two flags.
fn render_json(results: &[SearchResult], console: &Console) -> anyhow::Result<()> {
    let output: Vec<SearchResultOutput> = results.iter().map(SearchResultOutput::from).collect();
    let buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
    output.serialize(&mut ser)?;
    console_writeln!(console, "{}", &String::from_utf8(ser.into_inner())?);
    Ok(())
}

/// Render results in Composer's text format. For each row:
/// - `<href=URL>name</>` (terminal hyperlink) when `url` is non-empty,
///   else plain `name`, padded to the longest-name column.
/// - `<warning>! Abandoned !</warning> ` prefix when abandoned.
/// - Description, truncated with `...` to fit the terminal width.
fn render_text(results: &[SearchResult], console: &Console) {
    if results.is_empty() {
        return;
    }

    let width = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);
    let name_length = results.iter().map(|r| r.name.len()).max().unwrap_or(0) + 1;

    for result in results {
        let warning = if is_abandoned(result) {
            console_format!("<warning>! Abandoned !</warning> ")
        } else {
            String::new()
        };

        // Composer uses `Console::strlen` on the warning fragment which
        // strips formatter tags before measuring; here we count the visible
        // chars manually since the styled string contains ANSI bytes.
        let visible_warning_len = if warning.is_empty() { 0 } else { 14 };
        let remaining = width.saturating_sub(name_length + visible_warning_len);
        let description = result.description.as_str();
        let desc_display = if description.chars().count() > remaining && remaining > 3 {
            let cutoff: String = description.chars().take(remaining - 3).collect();
            format!("{cutoff}...")
        } else {
            description.to_string()
        };

        let padding_width = name_length.saturating_sub(result.name.len());
        let padded_name = if !result.url.is_empty() {
            format!(
                "{}{}",
                hyperlink(&result.url, &result.name, console.decorated),
                " ".repeat(padding_width)
            )
        } else {
            format!("{}{}", result.name, " ".repeat(padding_width))
        };

        console_writeln!(console, "{padded_name}{warning}{desc_display}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_search_response() {
        use mozart_registry::packagist::SearchResponse;

        let json = r#"{
            "results": [
                {
                    "name": "monolog/monolog",
                    "description": "Sends your logs to files, sockets, inboxes, databases and various web services",
                    "url": "https://packagist.org/packages/monolog/monolog",
                    "repository": "https://github.com/Seldaek/monolog",
                    "downloads": 500000000,
                    "favers": 20000
                },
                {
                    "name": "psr/log",
                    "description": "Common interface for logging libraries",
                    "url": "https://packagist.org/packages/psr/log",
                    "repository": null,
                    "downloads": 800000000,
                    "favers": 10000
                }
            ],
            "total": 2,
            "next": null
        }"#;

        let response: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.total, 2);
        assert!(response.next.is_none());

        let first = &response.results[0];
        assert_eq!(first.name, "monolog/monolog");
        assert_eq!(first.downloads, 500_000_000);
        assert_eq!(first.favers, 20_000);
        assert_eq!(
            first.repository.as_deref(),
            Some("https://github.com/Seldaek/monolog")
        );

        let second = &response.results[1];
        assert_eq!(second.name, "psr/log");
        assert!(second.repository.is_none());
    }

    #[test]
    fn test_parse_search_response_with_abandoned() {
        use mozart_registry::packagist::SearchResponse;

        let json = r#"{
            "results": [
                {
                    "name": "old/abandoned-pkg",
                    "description": "An abandoned package",
                    "url": "https://packagist.org/packages/old/abandoned-pkg",
                    "repository": "https://github.com/old/abandoned-pkg",
                    "downloads": 1000,
                    "favers": 10,
                    "abandoned": "new/replacement-pkg"
                },
                {
                    "name": "active/pkg",
                    "description": "An active package",
                    "url": "https://packagist.org/packages/active/pkg",
                    "repository": null,
                    "downloads": 5000,
                    "favers": 100
                }
            ],
            "total": 2,
            "next": null
        }"#;

        let response: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.results.len(), 2);

        let first = &response.results[0];
        assert_eq!(first.name, "old/abandoned-pkg");
        assert_eq!(
            first.abandoned.as_ref().and_then(|v| v.as_str()),
            Some("new/replacement-pkg")
        );

        let second = &response.results[1];
        assert_eq!(second.name, "active/pkg");
        assert!(second.abandoned.is_none());
    }

    #[test]
    fn test_parse_search_response_with_next() {
        use mozart_registry::packagist::SearchResponse;

        let json = r#"{
            "results": [],
            "total": 100,
            "next": "https://packagist.org/search.json?q=monolog&page=2"
        }"#;

        let response: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.total, 100);
        assert_eq!(
            response.next.as_deref(),
            Some("https://packagist.org/search.json?q=monolog&page=2")
        );
    }

    #[test]
    fn test_is_abandoned_none() {
        let result = make_result("vendor/pkg");
        assert!(!is_abandoned(&result));
    }

    #[test]
    fn test_is_abandoned_true() {
        let mut result = make_result("vendor/pkg");
        result.abandoned = Some(serde_json::Value::Bool(true));
        assert!(is_abandoned(&result));
    }

    #[test]
    fn test_is_abandoned_false() {
        let mut result = make_result("vendor/pkg");
        result.abandoned = Some(serde_json::Value::Bool(false));
        assert!(!is_abandoned(&result));
    }

    #[test]
    fn test_is_abandoned_replacement_string() {
        let mut result = make_result("vendor/pkg");
        result.abandoned = Some(serde_json::Value::String("other/pkg".to_string()));
        assert!(is_abandoned(&result));
    }

    #[test]
    fn test_is_abandoned_empty_string() {
        let mut result = make_result("vendor/pkg");
        result.abandoned = Some(serde_json::Value::String(String::new()));
        assert!(!is_abandoned(&result));
    }

    #[test]
    fn test_search_result_output_matches_composer_schema() {
        let result = SearchResult {
            name: "test/pkg".to_string(),
            description: "A test package".to_string(),
            url: "https://packagist.org/packages/test/pkg".to_string(),
            repository: Some("https://github.com/test/pkg".to_string()),
            downloads: 1000,
            favers: 50,
            abandoned: None,
        };

        let output = SearchResultOutput::from(&result);
        let json = serde_json::to_string(&output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["name"], "test/pkg");
        assert_eq!(parsed["description"], "A test package");
        assert_eq!(parsed["url"], "https://packagist.org/packages/test/pkg");
        // Composer schema does not include repository, downloads, or favers
        assert!(parsed.get("repository").is_none());
        assert!(parsed.get("downloads").is_none());
        assert!(parsed.get("favers").is_none());
        // abandoned is skipped when None
        assert!(parsed.get("abandoned").is_none());
    }

    #[test]
    fn test_search_result_output_with_abandoned() {
        let result = SearchResult {
            name: "old/pkg".to_string(),
            description: "Old package".to_string(),
            url: "https://packagist.org/packages/old/pkg".to_string(),
            repository: None,
            downloads: 0,
            favers: 0,
            abandoned: Some(serde_json::Value::String("new/pkg".to_string())),
        };

        let output = SearchResultOutput::from(&result);
        let json = serde_json::to_string(&output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["abandoned"], "new/pkg");
    }

    fn make_result(name: &str) -> SearchResult {
        SearchResult {
            name: name.to_string(),
            description: String::new(),
            url: format!("https://packagist.org/packages/{name}"),
            repository: None,
            downloads: 0,
            favers: 0,
            abandoned: None,
        }
    }
}
