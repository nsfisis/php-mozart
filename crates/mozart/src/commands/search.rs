use clap::Args;
use serde::{Deserialize, Serialize};

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

/// Maximum number of pages to fetch from the Packagist search API.
const MAX_PAGES: usize = 20;

#[derive(Debug, Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
    total: u64,
    next: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct SearchResult {
    name: String,
    description: String,
    url: String,
    repository: Option<String>,
    downloads: u64,
    favers: u64,
}

/// Format a large count as a human-readable string (e.g. 1500 -> "1.5K", 2500000 -> "2.5M").
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

/// Percent-encode a string for use in a URL query parameter value.
/// Encodes spaces as `%20` and other reserved/non-ASCII characters.
fn url_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push_str("%20"),
            other => {
                encoded.push_str(&format!("%{other:02X}"));
            }
        }
    }
    encoded
}

pub fn execute(args: &SearchArgs, _cli: &super::Cli) -> anyhow::Result<()> {
    let query = args.tokens.join(" ");

    let client = reqwest::blocking::Client::builder()
        .user_agent("mozart/0.1.0")
        .build()?;

    let mut all_results: Vec<SearchResult> = Vec::new();
    let mut page = 1usize;
    let mut next_url: Option<String> = None;
    let mut total: u64 = 0;

    loop {
        let response: SearchResponse = if let Some(ref url) = next_url {
            // Packagist gives us the full next URL; just fetch it
            let resp = client.get(url).send()?;
            if !resp.status().is_success() {
                anyhow::bail!("Packagist search request failed (HTTP {})", resp.status());
            }
            resp.json()?
        } else {
            // Build the first request URL with query parameters encoded manually
            let encoded_query = url_encode(&query);
            let mut url = format!("https://packagist.org/search.json?q={encoded_query}");
            if let Some(ref t) = args.r#type {
                url.push_str("&type=");
                url.push_str(&url_encode(t));
            }

            let resp = client.get(&url).send()?;

            if !resp.status().is_success() {
                anyhow::bail!("Packagist search request failed (HTTP {})", resp.status());
            }
            resp.json()?
        };

        if page == 1 {
            total = response.total;
        }

        all_results.extend(response.results);
        next_url = response.next;
        page += 1;

        if next_url.is_none() || page > MAX_PAGES {
            break;
        }
    }

    // Apply client-side filters
    let mut results: Vec<&SearchResult> = all_results.iter().collect();

    if args.only_name {
        results.retain(|r| passes_only_name(r, &query));
    }

    if args.only_vendor {
        results.retain(|r| passes_only_vendor(r, &query));
    }

    // Output
    let format = args.format.as_deref().unwrap_or("text");

    match format {
        "json" => {
            let owned: Vec<SearchResult> = results.into_iter().cloned().collect();
            let json = serde_json::to_string_pretty(&owned)?;
            println!("{json}");
        }
        _ => {
            if results.is_empty() {
                eprintln!(
                    "{}",
                    crate::console::warning(&format!("No packages found for \"{query}\""))
                );
                return Ok(());
            }

            eprintln!(
                "Found {} packages matching \"{}\" (showing {} result{})",
                total,
                query,
                results.len(),
                if results.len() == 1 { "" } else { "s" }
            );
            eprintln!();

            // Calculate alignment widths
            let name_width = results.iter().map(|r| r.name.len()).max().unwrap_or(0);

            for result in &results {
                let dl_str = format!("Downloads: {}", format_count(result.downloads));
                let fav_str = format!("Favers: {}", format_count(result.favers));

                println!(
                    "{} {}  {}",
                    crate::console::info(&format!("{:<width$}", result.name, width = name_width)),
                    crate::console::comment(&dl_str),
                    crate::console::comment(&fav_str),
                );
                if !result.description.is_empty() {
                    println!("  {}", result.description);
                }
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
    fn test_parse_search_response_with_next() {
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

    // ── serialization ────────────────────────────────────────────────────────

    #[test]
    fn test_search_result_serializes_to_json() {
        let result = SearchResult {
            name: "test/pkg".to_string(),
            description: "A test package".to_string(),
            url: "https://packagist.org/packages/test/pkg".to_string(),
            repository: Some("https://github.com/test/pkg".to_string()),
            downloads: 1000,
            favers: 50,
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["name"], "test/pkg");
        assert_eq!(parsed["downloads"], 1000);
        assert_eq!(parsed["favers"], 50);
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
        }
    }
}
