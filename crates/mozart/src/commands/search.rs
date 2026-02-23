use clap::Args;
use mozart_core::console::Verbosity;
use mozart_core::console_format;
use mozart_registry::packagist::SearchResult;
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

/// Format a large count as a human-readable string (e.g. 1500 -> "1.5K", 2500000 -> "2.5M").
#[allow(dead_code)]
fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        // Show one decimal place only when needed
        if (m - m.floor()).abs() < 0.05 {
            format!("{}M", m.floor() as u64)
        } else {
            format!("{:.1}M", m)
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if (k - k.floor()).abs() < 0.05 {
            format!("{}K", k.floor() as u64)
        } else {
            format!("{:.1}K", k)
        }
    } else {
        n.to_string()
    }
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

/// Returns true if the result passes the `--only-name` filter: the package name must contain
/// the query string (case-insensitive).
fn passes_only_name(result: &SearchResult, query: &str) -> bool {
    result.name.to_lowercase().contains(&query.to_lowercase())
}

/// Returns true if the result passes the `--only-vendor` filter: the vendor portion of the
/// package name (before the `/`) must equal the query (case-insensitive).
fn passes_only_vendor(result: &SearchResult, query: &str) -> bool {
    let vendor = result.name.split('/').next().unwrap_or("");
    vendor.eq_ignore_ascii_case(query)
}

pub async fn execute(
    args: &SearchArgs,
    _cli: &super::Cli,
    console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    if args.only_name && args.only_vendor {
        anyhow::bail!("--only-name and --only-vendor cannot be used together");
    }

    let query = args.tokens.join(" ");

    let format = args.format.as_deref().unwrap_or("text");

    if !matches!(format, "text" | "json") {
        console.error(&console_format!(
            "<error>Unsupported format \"{format}\". See help for supported formats.</error>"
        ));
        return Err(mozart_core::exit_code::bail_silent(
            mozart_core::exit_code::GENERAL_ERROR,
        ));
    }

    let (all_results, _total) =
        mozart_registry::packagist::search_packages(&query, args.r#type.as_deref()).await?;

    // Apply client-side filters
    let mut results: Vec<&SearchResult> = all_results.iter().collect();

    if args.only_name {
        results.retain(|r| passes_only_name(r, &query));
    }

    if args.only_vendor {
        results.retain(|r| passes_only_vendor(r, &query));

        // Deduplicate to unique vendor names (Composer returns vendor-only names
        // for SEARCH_VENDOR mode).
        let mut seen = std::collections::HashSet::new();
        let mut vendor_names: Vec<String> = Vec::new();
        for r in &results {
            let vendor = r.name.split('/').next().unwrap_or("").to_string();
            if seen.insert(vendor.clone()) {
                vendor_names.push(vendor);
            }
        }

        match format {
            "json" => {
                let json = serde_json::to_string_pretty(&vendor_names)?;
                console.write_stdout(&json, Verbosity::Normal);
            }
            _ => {
                if vendor_names.is_empty() {
                    console.info(&console_format!(
                        "<warning>No packages found for \"{query}\"</warning>"
                    ));
                } else {
                    for vendor in &vendor_names {
                        console.write_stdout(
                            &console_format!("<info>{vendor}</info>"),
                            Verbosity::Normal,
                        );
                    }
                }
            }
        }
        return Ok(());
    }

    // Output
    match format {
        "json" => {
            let output: Vec<SearchResultOutput> = results
                .iter()
                .map(|r| SearchResultOutput::from(*r))
                .collect();
            let json = serde_json::to_string_pretty(&output)?;
            console.write_stdout(&json, Verbosity::Normal);
        }
        _ => {
            if results.is_empty() {
                console.info(&console_format!(
                    "<warning>No packages found for \"{query}\"</warning>"
                ));
                return Ok(());
            }

            let width = terminal_size::terminal_size()
                .map(|(w, _)| w.0 as usize)
                .unwrap_or(80);
            let name_width = results.iter().map(|r| r.name.len()).max().unwrap_or(0) + 1;

            for result in &results {
                let warning = if is_abandoned(result) {
                    "! Abandoned ! "
                } else {
                    ""
                };

                let remaining = width.saturating_sub(name_width + warning.len());
                let description = result.description.as_str();
                let desc_display = if description.len() > remaining && remaining > 3 {
                    format!("{}...", &description[..remaining.saturating_sub(3)])
                } else {
                    description.to_string()
                };

                let padding = " ".repeat(name_width.saturating_sub(result.name.len()));
                console.write_stdout(
                    &format!("{}{}{}{}", result.name, padding, warning, desc_display),
                    Verbosity::Normal,
                );
            }
        }
    }

    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_count ────────────────────────────────────────────────────────

    #[test]
    fn test_format_count_small() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(42), "42");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn test_format_count_thousands() {
        assert_eq!(format_count(1_000), "1K");
        assert_eq!(format_count(1_500), "1.5K");
        assert_eq!(format_count(2_500), "2.5K");
        assert_eq!(format_count(10_000), "10K");
    }

    #[test]
    fn test_format_count_millions() {
        assert_eq!(format_count(1_000_000), "1M");
        assert_eq!(format_count(1_500_000), "1.5M");
        assert_eq!(format_count(2_500_000), "2.5M");
    }

    // ── SearchResponse parsing ───────────────────────────────────────────────

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

    // ── only_name filter ─────────────────────────────────────────────────────

    #[test]
    fn test_passes_only_name_match() {
        let result = make_result("monolog/monolog");
        assert!(passes_only_name(&result, "monolog"));
    }

    #[test]
    fn test_passes_only_name_partial_match() {
        let result = make_result("monolog/monolog");
        assert!(passes_only_name(&result, "mono"));
    }

    #[test]
    fn test_passes_only_name_case_insensitive() {
        let result = make_result("Monolog/Monolog");
        assert!(passes_only_name(&result, "monolog"));
    }

    #[test]
    fn test_passes_only_name_no_match() {
        let result = make_result("symfony/console");
        assert!(!passes_only_name(&result, "monolog"));
    }

    #[test]
    fn test_passes_only_name_vendor_part_matches() {
        let result = make_result("monolog/handler");
        assert!(passes_only_name(&result, "monolog"));
    }

    // ── only_vendor filter ───────────────────────────────────────────────────

    #[test]
    fn test_passes_only_vendor_match() {
        let result = make_result("monolog/monolog");
        assert!(passes_only_vendor(&result, "monolog"));
    }

    #[test]
    fn test_passes_only_vendor_case_insensitive() {
        let result = make_result("Monolog/SomePackage");
        assert!(passes_only_vendor(&result, "monolog"));
    }

    #[test]
    fn test_passes_only_vendor_no_match() {
        // query "monolog" as vendor but package vendor is "symfony"
        let result = make_result("symfony/console");
        assert!(!passes_only_vendor(&result, "monolog"));
    }

    #[test]
    fn test_passes_only_vendor_partial_does_not_match() {
        // only_vendor requires exact vendor match, not substring
        let result = make_result("monolog/monolog");
        assert!(!passes_only_vendor(&result, "mono"));
    }

    // ── is_abandoned ─────────────────────────────────────────────────────────

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

    // ── serialization ────────────────────────────────────────────────────────

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

    // ── only_vendor deduplication ───────────────────────────────────────────

    #[test]
    fn test_only_vendor_deduplicates_vendor_names() {
        let results = vec![
            make_result("monolog/monolog"),
            make_result("monolog/handler"),
            make_result("monolog/formatter"),
        ];
        let refs: Vec<&SearchResult> = results.iter().collect();

        let mut seen = std::collections::HashSet::new();
        let mut vendor_names: Vec<String> = Vec::new();
        for r in &refs {
            let vendor = r.name.split('/').next().unwrap_or("").to_string();
            if seen.insert(vendor.clone()) {
                vendor_names.push(vendor);
            }
        }

        assert_eq!(vendor_names, vec!["monolog"]);
    }

    // ── helper ───────────────────────────────────────────────────────────────

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
