use anyhow::{Context as _, Result, bail};
use indexmap::IndexMap;
use std::fs;
use std::path::Path;

const VALID_SECTIONS: &[&str] = &[
    "TEST",
    "CONDITION",
    "COMPOSER",
    "LOCK",
    "INSTALLED",
    "RUN",
    "EXPECT-LOCK",
    "EXPECT-INSTALLED",
    "EXPECT-OUTPUT",
    "EXPECT-OUTPUT-OPTIMIZED",
    "EXPECT-EXIT-CODE",
    "EXPECT-EXCEPTION",
    "EXPECT",
];

const REQUIRED_SECTIONS: &[&str] = &["TEST", "COMPOSER", "RUN", "EXPECT"];

#[derive(Debug, Clone)]
pub struct ParsedTest {
    pub test: String,
    pub condition: Option<String>,
    pub composer: String,
    pub lock: Option<String>,
    pub installed: Option<String>,
    pub run: String,
    pub expect_lock: Option<String>,
    pub expect_installed: Option<String>,
    pub expect_output: Option<String>,
    pub expect_output_optimized: Option<String>,
    pub expect_exit_code: Option<i32>,
    pub expect_exception: Option<String>,
    pub expect: String,
}

pub fn parse_test_file(path: &Path) -> Result<ParsedTest> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_test_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn parse_test_str(content: &str) -> Result<ParsedTest> {
    let mut sections = split_sections(content, VALID_SECTIONS)?;

    for required in REQUIRED_SECTIONS {
        if !sections.contains_key(*required) {
            bail!("missing required section: --{required}--");
        }
    }

    let mut take = |key: &str| sections.shift_remove(key);

    let test = take("TEST").unwrap();
    let composer = take("COMPOSER").unwrap();
    let run = take("RUN").unwrap();
    let expect = take("EXPECT").unwrap();

    let expect_exit_code = match take("EXPECT-EXIT-CODE") {
        Some(s) => Some(
            s.trim()
                .parse::<i32>()
                .with_context(|| format!("invalid EXPECT-EXIT-CODE: {s:?}"))?,
        ),
        None => None,
    };

    Ok(ParsedTest {
        test,
        condition: take("CONDITION"),
        composer,
        lock: take("LOCK"),
        installed: take("INSTALLED"),
        run,
        expect_lock: take("EXPECT-LOCK"),
        expect_installed: take("EXPECT-INSTALLED"),
        expect_output: take("EXPECT-OUTPUT"),
        expect_output_optimized: take("EXPECT-OUTPUT-OPTIMIZED"),
        expect_exit_code,
        expect_exception: take("EXPECT-EXCEPTION"),
        expect,
    })
}

/// Split a `.test` fixture into its `--SECTION--` blocks.
///
/// Shared helper for both [`parse_test_str`] and the sibling pool-builder
/// parser; each caller passes its own allowed-section list so unknown
/// headers still surface as parse errors rather than silently ignored.
pub(crate) fn split_sections(
    content: &str,
    valid_sections: &[&str],
) -> Result<IndexMap<String, String>> {
    let header_re = regex::Regex::new(r"^--([A-Z][A-Z-]*)--$").unwrap();

    let mut sections: IndexMap<String, String> = IndexMap::new();
    let mut current_section: Option<String> = None;
    let mut current_body = String::new();

    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if let Some(caps) = header_re.captures(trimmed) {
            let name = caps[1].to_string();
            if !valid_sections.contains(&name.as_str()) {
                bail!("unknown section: --{name}--");
            }
            if let Some(prev) = current_section.take() {
                let body = trim_trailing_newlines(&current_body).to_string();
                if sections.insert(prev.clone(), body).is_some() {
                    bail!("duplicate section: --{prev}--");
                }
                current_body.clear();
            }
            current_section = Some(name);
        } else if current_section.is_some() {
            current_body.push_str(line);
        }
    }

    if let Some(name) = current_section.take() {
        let body = trim_trailing_newlines(&current_body).to_string();
        if sections.insert(name.clone(), body).is_some() {
            bail!("duplicate section: --{name}--");
        }
    }

    Ok(sections)
}

fn trim_trailing_newlines(s: &str) -> &str {
    s.trim_end_matches(['\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_required_sections() {
        let input = "\
--TEST--
A simple test
--COMPOSER--
{\"require\": {\"a/a\": \"1.0.0\"}}
--RUN--
install
--EXPECT--
Installing a/a (1.0.0)
";
        let t = parse_test_str(input).unwrap();
        assert_eq!(t.test, "A simple test");
        assert_eq!(t.composer, "{\"require\": {\"a/a\": \"1.0.0\"}}");
        assert_eq!(t.run, "install");
        assert_eq!(t.expect, "Installing a/a (1.0.0)");
        assert!(t.lock.is_none());
        assert!(t.installed.is_none());
        assert!(t.expect_output.is_none());
        assert!(t.expect_exit_code.is_none());
    }

    #[test]
    fn parses_all_sections() {
        let input = "\
--TEST--
desc
--CONDITION--
true
--COMPOSER--
{}
--LOCK--
{\"packages\": []}
--INSTALLED--
[]
--RUN--
update --with-dependencies a/a
--EXPECT-LOCK--
{\"packages\": []}
--EXPECT-INSTALLED--
[]
--EXPECT-OUTPUT--
some output
--EXPECT-OUTPUT-OPTIMIZED--
optimized output
--EXPECT-EXIT-CODE--
2
--EXPECT-EXCEPTION--
SomeException
--EXPECT--
op log
";
        let t = parse_test_str(input).unwrap();
        assert_eq!(t.test, "desc");
        assert_eq!(t.condition.as_deref(), Some("true"));
        assert_eq!(t.composer, "{}");
        assert_eq!(t.lock.as_deref(), Some("{\"packages\": []}"));
        assert_eq!(t.installed.as_deref(), Some("[]"));
        assert_eq!(t.run, "update --with-dependencies a/a");
        assert_eq!(t.expect_lock.as_deref(), Some("{\"packages\": []}"));
        assert_eq!(t.expect_installed.as_deref(), Some("[]"));
        assert_eq!(t.expect_output.as_deref(), Some("some output"));
        assert_eq!(
            t.expect_output_optimized.as_deref(),
            Some("optimized output")
        );
        assert_eq!(t.expect_exit_code, Some(2));
        assert_eq!(t.expect_exception.as_deref(), Some("SomeException"));
        assert_eq!(t.expect, "op log");
    }

    #[test]
    fn preserves_internal_newlines_in_body() {
        let input = "\
--TEST--
multi
--COMPOSER--
{
    \"name\": \"a/a\"
}
--RUN--
install
--EXPECT--
line1
line2
line3
";
        let t = parse_test_str(input).unwrap();
        assert_eq!(t.composer, "{\n    \"name\": \"a/a\"\n}");
        assert_eq!(t.expect, "line1\nline2\nline3");
    }

    #[test]
    fn rejects_unknown_section() {
        let input = "\
--TEST--
x
--MYSTERY--
y
--COMPOSER--
{}
--RUN--
install
--EXPECT--
z
";
        let err = parse_test_str(input).unwrap_err();
        assert!(err.to_string().contains("unknown section"), "{err}");
    }

    #[test]
    fn rejects_missing_required_section() {
        let input = "\
--TEST--
x
--COMPOSER--
{}
--EXPECT--
z
";
        let err = parse_test_str(input).unwrap_err();
        assert!(err.to_string().contains("RUN"), "{err}");
    }

    #[test]
    fn rejects_duplicate_section() {
        let input = "\
--TEST--
first
--COMPOSER--
{}
--RUN--
install
--TEST--
second
--EXPECT--
z
";
        let err = parse_test_str(input).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn rejects_invalid_exit_code() {
        let input = "\
--TEST--
x
--COMPOSER--
{}
--RUN--
install
--EXPECT-EXIT-CODE--
not-a-number
--EXPECT--
z
";
        let err = parse_test_str(input).unwrap_err();
        assert!(err.to_string().contains("EXPECT-EXIT-CODE"), "{err}");
    }

    #[test]
    fn skips_text_before_first_section() {
        let input = "\
this is a header comment
that should be ignored
--TEST--
x
--COMPOSER--
{}
--RUN--
install
--EXPECT--
z
";
        let t = parse_test_str(input).unwrap();
        assert_eq!(t.test, "x");
    }

    #[test]
    fn handles_crlf_line_endings() {
        let input =
            "--TEST--\r\nx\r\n--COMPOSER--\r\n{}\r\n--RUN--\r\ninstall\r\n--EXPECT--\r\nz\r\n";
        let t = parse_test_str(input).unwrap();
        assert_eq!(t.test, "x");
        assert_eq!(t.composer, "{}");
        assert_eq!(t.expect, "z");
    }
}
