use regex::Regex;
use std::sync::LazyLock;

static PACKAGE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9]([_.\-]?[a-z0-9]+)*/[a-z0-9](([_.]|\-{1,2})?[a-z0-9]+)*$").unwrap()
});

static AUTHOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<name>[- .,\pL\pN\pM''\x{201C}\x{201D}()]+)(?:\s+<(?P<email>.+?)>)?$").unwrap()
});

static AUTOLOAD_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^/][A-Za-z0-9\-_/]+/$").unwrap());

static CAMEL_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:([a-z])([A-Z])|([A-Z])([A-Z][a-z]))").unwrap());

static SANITIZE_EDGES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[_.\-]+|[_.\-]+$|[^a-z0-9_.\-]").unwrap());

static SANITIZE_REPEATS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([_.\-]){2,}").unwrap());

static NON_ALNUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9]").unwrap());

const VALID_STABILITIES: &[&str] = &["dev", "alpha", "beta", "rc", "stable"];

pub fn validate_package_name(name: &str) -> bool {
    PACKAGE_NAME_RE.is_match(name)
}

pub struct ParsedAuthor {
    pub name: String,
    pub email: Option<String>,
}

pub fn parse_author(input: &str) -> Result<ParsedAuthor, String> {
    if let Some(caps) = AUTHOR_RE.captures(input) {
        let name = caps.name("name").unwrap().as_str().trim().to_string();
        let email = caps.name("email").map(|m| m.as_str().to_string());
        Ok(ParsedAuthor { name, email })
    } else {
        Err(
            "Invalid author string. Must be in the formats: Jane Doe or John Smith <john@example.com>"
                .to_string(),
        )
    }
}

pub fn validate_stability(s: &str) -> bool {
    VALID_STABILITIES.contains(&s.to_lowercase().as_str())
}

pub fn validate_license(s: &str) -> bool {
    // TODO: check SPDX Identifier
    !s.is_empty()
}

pub fn validate_autoload_path(s: &str) -> bool {
    AUTOLOAD_PATH_RE.is_match(s)
}

pub fn namespace_from_package_name(package_name: &str) -> Option<String> {
    if package_name.is_empty() || !package_name.contains('/') {
        return None;
    }

    let parts: Vec<String> = package_name
        .split('/')
        .map(|part| {
            let replaced = NON_ALNUM_RE.replace_all(part, " ");
            let words: Vec<String> = replaced
                .split_whitespace()
                .map(|w| {
                    let mut chars = w.chars();
                    match chars.next() {
                        Some(c) => c.to_uppercase().to_string() + &chars.collect::<String>(),
                        None => String::new(),
                    }
                })
                .collect();
            words.join("")
        })
        .collect();

    Some(parts.join("\\"))
}

pub fn sanitize_package_name_component(name: &str) -> String {
    // CamelCase → kebab-case
    let name = CAMEL_SPLIT_RE.replace_all(name, "${1}${3}-${2}${4}");
    let name = name.to_lowercase();
    // Remove leading/trailing separators and non-alnum chars
    let name = SANITIZE_EDGES_RE.replace_all(&name, "");
    // Collapse repeated separators
    let name = SANITIZE_REPEATS_RE.replace_all(&name, "$1");
    name.to_string()
}

pub fn parse_require_string(s: &str) -> Result<(String, String), String> {
    // Formats: "foo/bar:^1.0", "foo/bar=^1.0", "foo/bar ^1.0"
    let s = s.trim();

    for sep in [':', '=', ' '] {
        if let Some(pos) = s.find(sep) {
            let name = s[..pos].trim();
            let version = s[pos + sep.len_utf8()..].trim();
            if !name.is_empty() && !version.is_empty() {
                return Ok((name.to_string(), version.to_string()));
            }
        }
    }

    Err(format!(
        "Could not parse requirement \"{s}\". Expected format: vendor/package:version"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_package_names() {
        assert!(validate_package_name("vendor/package"));
        assert!(validate_package_name("my-vendor/my-package"));
        assert!(validate_package_name("vendor/pkg123"));
        assert!(validate_package_name("a/b"));
        assert!(validate_package_name("vendor/my_package"));
        assert!(validate_package_name("vendor/my.package"));
        assert!(validate_package_name("vendor/my--package"));
    }

    #[test]
    fn test_invalid_package_names() {
        assert!(!validate_package_name("novendor"));
        assert!(!validate_package_name("/package"));
        assert!(!validate_package_name("vendor/"));
        assert!(!validate_package_name("Vendor/Package"));
        assert!(!validate_package_name("vendor/pack age"));
        assert!(!validate_package_name(""));
    }

    #[test]
    fn test_parse_author_name_and_email() {
        let a = parse_author("John Smith <john@example.com>").unwrap();
        assert_eq!(a.name, "John Smith");
        assert_eq!(a.email.as_deref(), Some("john@example.com"));
    }

    #[test]
    fn test_parse_author_name_only() {
        let a = parse_author("Jane Doe").unwrap();
        assert_eq!(a.name, "Jane Doe");
        assert!(a.email.is_none());
    }

    #[test]
    fn test_parse_author_invalid() {
        assert!(parse_author("").is_err());
    }

    #[test]
    fn test_validate_stability() {
        assert!(validate_stability("dev"));
        assert!(validate_stability("alpha"));
        assert!(validate_stability("beta"));
        assert!(validate_stability("rc"));
        assert!(validate_stability("stable"));
        assert!(validate_stability("Dev"));
        assert!(validate_stability("STABLE"));
        assert!(!validate_stability("invalid"));
        assert!(!validate_stability(""));
    }

    #[test]
    fn test_validate_autoload_path() {
        assert!(validate_autoload_path("src/"));
        assert!(validate_autoload_path("lib/src/"));
        assert!(!validate_autoload_path("/src/"));
        assert!(!validate_autoload_path("src"));
        assert!(!validate_autoload_path(""));
    }

    #[test]
    fn test_namespace_from_package_name() {
        assert_eq!(
            namespace_from_package_name("acme/my-pkg"),
            Some("Acme\\MyPkg".to_string())
        );
        assert_eq!(
            namespace_from_package_name("new_projects.acme-extra/package-name"),
            Some("NewProjectsAcmeExtra\\PackageName".to_string())
        );
        assert_eq!(namespace_from_package_name(""), None);
        assert_eq!(namespace_from_package_name("novendor"), None);
    }

    #[test]
    fn test_sanitize_package_name_component() {
        assert_eq!(sanitize_package_name_component("MyPackage"), "my-package");
        assert_eq!(
            sanitize_package_name_component("CamelCaseTest"),
            "camel-case-test"
        );
        assert_eq!(sanitize_package_name_component("already-ok"), "already-ok");
        assert_eq!(sanitize_package_name_component("__bad__"), "bad");
    }

    #[test]
    fn test_parse_require_string() {
        let (name, ver) = parse_require_string("foo/bar:^1.0").unwrap();
        assert_eq!(name, "foo/bar");
        assert_eq!(ver, "^1.0");

        let (name, ver) = parse_require_string("foo/bar=^1.0").unwrap();
        assert_eq!(name, "foo/bar");
        assert_eq!(ver, "^1.0");

        let (name, ver) = parse_require_string("foo/bar ^1.0").unwrap();
        assert_eq!(name, "foo/bar");
        assert_eq!(ver, "^1.0");

        assert!(parse_require_string("invalid").is_err());
    }
}
