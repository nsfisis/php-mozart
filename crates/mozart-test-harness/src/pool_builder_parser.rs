//! Parser for Composer's `PoolBuilderTest` `.test` fixture format.
//!
//! Mirrors `composer/tests/Composer/Test/DependencyResolver/PoolBuilderTest.php::readTestFile`.
//! Section bodies are stored as raw strings (typically JSON); the runner is
//! responsible for interpreting them.

use anyhow::{Context as _, Result, bail};
use std::fs;
use std::path::Path;

use crate::parser::split_sections;

const VALID_SECTIONS: &[&str] = &[
    "TEST",
    "ROOT",
    "REQUEST",
    "FIXED",
    "PACKAGE-REPOS",
    "EXPECT",
    "EXPECT-OPTIMIZED",
];

const REQUIRED_SECTIONS: &[&str] = &["TEST", "REQUEST", "PACKAGE-REPOS", "EXPECT"];

#[derive(Debug, Clone)]
pub struct ParsedPoolBuilderTest {
    pub test: String,
    pub root: Option<String>,
    pub request: String,
    pub fixed: Option<String>,
    pub package_repos: String,
    pub expect: String,
    pub expect_optimized: Option<String>,
}

pub fn parse_pool_builder_test_file(path: &Path) -> Result<ParsedPoolBuilderTest> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_pool_builder_test_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))
}

pub fn parse_pool_builder_test_str(content: &str) -> Result<ParsedPoolBuilderTest> {
    let mut sections = split_sections(content, VALID_SECTIONS)?;

    for required in REQUIRED_SECTIONS {
        if !sections.contains_key(*required) {
            bail!("missing required section: --{required}--");
        }
    }

    let mut take = |key: &str| sections.shift_remove(key);

    let test = take("TEST").unwrap();
    let request = take("REQUEST").unwrap();
    let package_repos = take("PACKAGE-REPOS").unwrap();
    let expect = take("EXPECT").unwrap();

    Ok(ParsedPoolBuilderTest {
        test,
        root: take("ROOT"),
        request,
        fixed: take("FIXED"),
        package_repos,
        expect,
        expect_optimized: take("EXPECT-OPTIMIZED"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_required_sections() {
        let input = "\
--TEST--
A pool builder test
--REQUEST--
{\"require\": {\"a/a\": \"*\"}}
--PACKAGE-REPOS--
[[{\"name\": \"a/a\", \"version\": \"1.0.0\"}]]
--EXPECT--
[\"a/a-1.0.0.0\"]
";
        let t = parse_pool_builder_test_str(input).unwrap();
        assert_eq!(t.test, "A pool builder test");
        assert_eq!(t.request, "{\"require\": {\"a/a\": \"*\"}}");
        assert_eq!(
            t.package_repos,
            "[[{\"name\": \"a/a\", \"version\": \"1.0.0\"}]]"
        );
        assert_eq!(t.expect, "[\"a/a-1.0.0.0\"]");
        assert!(t.root.is_none());
        assert!(t.fixed.is_none());
        assert!(t.expect_optimized.is_none());
    }

    #[test]
    fn parses_all_optional_sections() {
        let input = "\
--TEST--
desc
--ROOT--
{\"minimum-stability\": \"dev\"}
--REQUEST--
{\"require\": {}}
--FIXED--
[{\"name\": \"x/x\", \"version\": \"1.0.0\", \"id\": 1}]
--PACKAGE-REPOS--
[]
--EXPECT--
[1]
--EXPECT-OPTIMIZED--
[1]
";
        let t = parse_pool_builder_test_str(input).unwrap();
        assert_eq!(t.test, "desc");
        assert_eq!(t.root.as_deref(), Some("{\"minimum-stability\": \"dev\"}"));
        assert_eq!(
            t.fixed.as_deref(),
            Some("[{\"name\": \"x/x\", \"version\": \"1.0.0\", \"id\": 1}]")
        );
        assert_eq!(t.expect_optimized.as_deref(), Some("[1]"));
    }

    #[test]
    fn rejects_unknown_section() {
        let input = "\
--TEST--
x
--MYSTERY--
y
--REQUEST--
{}
--PACKAGE-REPOS--
[]
--EXPECT--
[]
";
        let err = parse_pool_builder_test_str(input).unwrap_err();
        assert!(err.to_string().contains("unknown section"), "{err}");
    }

    #[test]
    fn rejects_missing_required_section() {
        let input = "\
--TEST--
x
--REQUEST--
{}
--EXPECT--
[]
";
        let err = parse_pool_builder_test_str(input).unwrap_err();
        assert!(err.to_string().contains("PACKAGE-REPOS"), "{err}");
    }

    #[test]
    fn rejects_installer_only_section() {
        // `--RUN--` is part of InstallerTest fixtures; PoolBuilder fixtures
        // have no such section, so it must be flagged as unknown here.
        let input = "\
--TEST--
x
--REQUEST--
{}
--PACKAGE-REPOS--
[]
--RUN--
install
--EXPECT--
[]
";
        let err = parse_pool_builder_test_str(input).unwrap_err();
        assert!(err.to_string().contains("RUN"), "{err}");
    }
}
