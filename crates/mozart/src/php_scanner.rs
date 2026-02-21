use anyhow::Result;
use regex::Regex;
use std::path::Path;

/// File extensions considered PHP source files for class scanning.
const PHP_EXTENSIONS: &[&str] = &["php", "inc", "hh"];

/// Check if a file path has a PHP-like extension.
fn is_php_file(path: &Path) -> bool {
    is_php_ext(path)
}

/// Public version of the PHP extension check, used by the autoload scanner.
pub fn is_php_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| PHP_EXTENSIONS.iter().any(|&e| ext.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Scan a PHP file and return the list of fully-qualified class names declared in it.
///
/// Returns an empty vec if the file has no relevant extension or no class declarations.
pub fn find_classes(path: &Path) -> Result<Vec<String>> {
    if !is_php_file(path) {
        return Ok(vec![]);
    }

    let contents = std::fs::read_to_string(path)?;

    // Quick check: does the file even contain a class-like keyword?
    let quick_re = Regex::new(r"(?i)\b(?:class|interface|trait|enum)\s").unwrap();
    if !quick_re.is_match(&contents) {
        return Ok(vec![]);
    }

    let cleaned = clean_php_content(&contents);
    Ok(extract_declarations(&cleaned))
}

/// State machine that strips strings, comments, and heredocs/nowdocs from PHP code.
///
/// Returns a string of equal byte length where non-PHP content is replaced with spaces
/// so that regex offsets are preserved. Only PHP mode content is kept; everything else
/// is blanked out.
fn clean_php_content(contents: &str) -> String {
    let bytes = contents.as_bytes();
    let len = bytes.len();
    let mut out = vec![b' '; len];
    let mut i = 0;
    let mut in_php = false;

    while i < len {
        if !in_php {
            // Look for `<?`
            if i + 1 < len && bytes[i] == b'<' && bytes[i + 1] == b'?' {
                in_php = true;
                out[i] = b' ';
                out[i + 1] = b' ';
                i += 2;
                // Skip optional "php" or "="
                if i + 3 <= len && bytes[i..i + 3].eq_ignore_ascii_case(b"php") {
                    i += 3;
                } else if i < len && bytes[i] == b'=' {
                    i += 1;
                }
                continue;
            }
            i += 1;
            continue;
        }

        // In PHP mode
        // Check for `?>`
        if i + 1 < len && bytes[i] == b'?' && bytes[i + 1] == b'>' {
            in_php = false;
            i += 2;
            continue;
        }

        // Line comment: // or #
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            // Skip to end of line
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'#' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Block comment: /* ... */
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Single-quoted string
        if bytes[i] == b'\'' {
            out[i] = b'\'';
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    // escaped character — blank both
                    i += 2;
                } else if bytes[i] == b'\'' {
                    out[i] = b'\'';
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Double-quoted string
        if bytes[i] == b'"' {
            out[i] = b'"';
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'"' {
                    out[i] = b'"';
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Heredoc / Nowdoc: <<<
        if i + 2 < len && bytes[i] == b'<' && bytes[i + 1] == b'<' && bytes[i + 2] == b'<' {
            i += 3;
            // Skip whitespace
            while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }

            // Nowdoc uses single quotes around label; heredoc may use double quotes.
            let is_nowdoc = i < len && bytes[i] == b'\'';
            // Skip optional opening quote (single for nowdoc, double for heredoc)
            if i < len && (bytes[i] == b'\'' || bytes[i] == b'"') {
                i += 1;
            }

            // Read label
            let label_start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let label = std::str::from_utf8(&bytes[label_start..i])
                .unwrap_or("")
                .to_string();

            // Skip closing quote of label (must match the opening quote)
            let expected_close = if is_nowdoc { b'\'' } else { b'"' };
            if i < len && bytes[i] == expected_close {
                i += 1;
            }

            // Skip to end of line
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            if i < len {
                i += 1; // consume newline
            }

            // Scan for the terminator label on its own line
            if !label.is_empty() {
                loop {
                    if i >= len {
                        break;
                    }
                    // Check if current line starts with the label
                    let line_start = i;
                    // Skip optional whitespace for indented heredoc (PHP 7.3+)
                    while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') {
                        i += 1;
                    }
                    let remaining = &bytes[i..];
                    let label_bytes = label.as_bytes();
                    if remaining.len() >= label_bytes.len()
                        && &remaining[..label_bytes.len()] == label_bytes
                    {
                        let after = i + label_bytes.len();
                        // Terminator must be followed by ; or newline or EOF
                        if after >= len
                            || bytes[after] == b';'
                            || bytes[after] == b'\n'
                            || bytes[after] == b'\r'
                        {
                            // Skip to end of this line
                            i = after;
                            while i < len && bytes[i] != b'\n' {
                                i += 1;
                            }
                            if i < len {
                                i += 1;
                            }
                            break;
                        }
                    }
                    // Not a terminator line — skip to end of line
                    i = line_start;
                    while i < len && bytes[i] != b'\n' {
                        i += 1;
                    }
                    if i < len {
                        i += 1;
                    }
                }
            }
            continue;
        }

        // Backtick strings (shell exec)
        if bytes[i] == b'`' {
            out[i] = b'`';
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'`' {
                    out[i] = b'`';
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Keep normal PHP content
        out[i] = bytes[i];
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Extract fully-qualified class names from cleaned PHP content.
///
/// Tracks the current namespace and finds class/interface/trait/enum declarations.
fn extract_declarations(cleaned: &str) -> Vec<String> {
    let mut results = Vec::new();

    // Regex for namespace declarations:
    //   namespace Foo\Bar;       — simple
    //   namespace Foo\Bar {     — block
    //   namespace {              — global block
    let ns_re = Regex::new(
        r"(?x)
        \bnamespace\s+
        ((?:[a-zA-Z_\x80-\xff][a-zA-Z0-9_\x80-\xff]*\\)*[a-zA-Z_\x80-\xff][a-zA-Z0-9_\x80-\xff]*)
        \s*[;{]
        |
        \bnamespace\s*\{
        ",
    )
    .unwrap();

    // Regex for class/interface/trait/enum declarations.
    // We need to capture the name; anonymous classes (new class ...) are excluded.
    let decl_re = Regex::new(
        r"(?x)
        \b(?:abstract\s+|final\s+|readonly\s+)*
        (?P<kind>class|interface|trait|enum)\s+
        (?P<name>[a-zA-Z_\x80-\xff][a-zA-Z0-9_\x80-\xff]*)
        ",
    )
    .unwrap();

    let mut current_ns = String::new();

    // We process namespace changes as we walk through the file.
    // Build a list of all namespace and declaration positions.
    #[derive(Debug)]
    enum Event {
        Namespace(usize, String),   // position, namespace
        Declaration(usize, String), // position, simple name
    }

    let mut events: Vec<Event> = Vec::new();

    // Find namespace declarations
    for cap in ns_re.captures_iter(cleaned) {
        let pos = cap.get(0).unwrap().start();
        let ns_name = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        events.push(Event::Namespace(pos, ns_name));
    }

    // Find class/interface/trait/enum declarations
    for cap in decl_re.captures_iter(cleaned) {
        let pos = cap.get(0).unwrap().start();
        let name = cap.name("name").unwrap().as_str().to_string();

        // Skip anonymous classes: check if "new" precedes "class" on the same "expression".
        // A reliable check: look back for "new " before this match.
        let before = &cleaned[..pos];
        let kind = cap.name("kind").unwrap().as_str();
        if kind == "class" {
            // Check if "new" appears right before (with possible whitespace/modifiers).
            // Simple heuristic: scan backwards for non-whitespace token.
            let trimmed = before.trim_end();
            if trimmed.ends_with("new") {
                continue;
            }
        }

        events.push(Event::Declaration(pos, name));
    }

    // Sort all events by position
    events.sort_by_key(|e| match e {
        Event::Namespace(pos, _) => *pos,
        Event::Declaration(pos, _) => *pos,
    });

    // Process events in order
    for event in events {
        match event {
            Event::Namespace(_, ns) => {
                current_ns = ns;
            }
            Event::Declaration(_, name) => {
                let fqn = if current_ns.is_empty() {
                    name
                } else {
                    format!("{}\\{}", current_ns, name)
                };
                results.push(fqn);
            }
        }
    }

    results
}

/// Validate that a class file is correctly placed according to PSR-4.
///
/// - `class`: fully-qualified class name (e.g. `Foo\Bar\Baz`)
/// - `base_namespace`: the PSR-4 namespace prefix (e.g. `Foo\Bar\`)
/// - `file_path`: absolute path to the PHP file
/// - `base_path`: the directory mapped to `base_namespace` (absolute)
///
/// Returns `true` if the file path matches the PSR-4 mapping.
pub fn validate_psr4_class(
    class: &str,
    base_namespace: &str,
    file_path: &str,
    base_path: &str,
) -> bool {
    // Normalize the base namespace: ensure it ends with `\`
    let base_ns = if base_namespace.is_empty() || base_namespace.ends_with('\\') {
        base_namespace.to_string()
    } else {
        format!("{base_namespace}\\")
    };

    // Class must start with the base namespace
    if !class.starts_with(&*base_ns) {
        return false;
    }

    // The relative class name after the base namespace
    let relative_class = &class[base_ns.len()..];

    // Convert relative class to a relative file path: replace `\` with `/`
    let expected_relative = relative_class.replace('\\', "/");
    let expected_file = format!(
        "{}/{}.php",
        base_path.trim_end_matches('/'),
        expected_relative
    );

    // Normalize both paths for comparison (simplistic: just compare strings)
    Path::new(file_path) == Path::new(&expected_file)
}

/// Validate that a class file is correctly placed according to PSR-0.
///
/// - `class`: fully-qualified class name (e.g. `Foo_Bar_Baz` or `Foo\Bar`)
/// - `file_path`: absolute path to the PHP file
/// - `base_path`: the base directory for PSR-0 lookup
///
/// Returns `true` if the file path matches the PSR-0 mapping.
pub fn validate_psr0_class(class: &str, file_path: &str, base_path: &str) -> bool {
    // PSR-0: namespace separators AND underscores (in class part) map to directory separators.
    // Split on `\` first; the last segment may contain underscores that also become `/`.
    let parts: Vec<&str> = class.split('\\').collect();
    let relative = if parts.len() == 1 {
        // No namespace: underscores in class name become dir separators
        parts[0].replace('_', "/")
    } else {
        let ns_part = parts[..parts.len() - 1].join("/");
        let class_part = parts[parts.len() - 1].replace('_', "/");
        format!("{}/{}", ns_part, class_part)
    };

    let expected_file = format!("{}/{}.php", base_path.trim_end_matches('/'), relative);
    Path::new(file_path) == Path::new(&expected_file)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_php(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".php").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // -------------------------------------------------------------------------
    // find_classes tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_find_classes_simple_class() {
        let f = write_php("<?php\nclass Foo {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["Foo"]);
    }

    #[test]
    fn test_find_classes_with_namespace() {
        let f = write_php("<?php\nnamespace Foo\\Bar;\nclass Baz {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["Foo\\Bar\\Baz"]);
    }

    #[test]
    fn test_find_classes_multiple_classes() {
        let f = write_php("<?php\nnamespace App;\nclass Foo {}\nclass Bar {}\ninterface Baz {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["App\\Foo", "App\\Bar", "App\\Baz"]);
    }

    #[test]
    fn test_find_classes_interface() {
        let f = write_php("<?php\ninterface MyInterface {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["MyInterface"]);
    }

    #[test]
    fn test_find_classes_trait() {
        let f = write_php("<?php\ntrait MyTrait {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["MyTrait"]);
    }

    #[test]
    fn test_find_classes_enum() {
        let f = write_php("<?php\nenum Status {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["Status"]);
    }

    #[test]
    fn test_find_classes_enum_with_backing_type() {
        let f = write_php("<?php\nenum Color: string { case Red = 'red'; }\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["Color"]);
    }

    #[test]
    fn test_find_classes_anonymous_class_skipped() {
        let f = write_php("<?php\n$obj = new class {};\n");
        let classes = find_classes(f.path()).unwrap();
        assert!(classes.is_empty(), "anonymous class should not be scanned");
    }

    #[test]
    fn test_find_classes_comments_ignored() {
        let f = write_php(
            "<?php\n// class FakeClass {}\n/* interface FakeInterface {} */\nclass RealClass {}\n",
        );
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["RealClass"]);
    }

    #[test]
    fn test_find_classes_strings_ignored() {
        let f = write_php(
            "<?php\n$s = 'class NotAClass {}';\n$t = \"interface NotAnInterface {}\";\nclass RealClass {}\n",
        );
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["RealClass"]);
    }

    #[test]
    fn test_find_classes_heredoc_ignored() {
        let f = write_php("<?php\n$s = <<<EOT\nclass FakeClass {}\nEOT;\nclass RealClass {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert_eq!(classes, vec!["RealClass"]);
    }

    #[test]
    fn test_find_classes_empty_file() {
        let f = write_php("<?php\n// nothing here\n");
        let classes = find_classes(f.path()).unwrap();
        assert!(classes.is_empty());
    }

    #[test]
    fn test_find_classes_no_classes() {
        let f = write_php("<?php\necho 'hello';\n");
        let classes = find_classes(f.path()).unwrap();
        assert!(classes.is_empty());
    }

    #[test]
    fn test_find_classes_abstract_final() {
        let f = write_php("<?php\nabstract class AbstractFoo {}\nfinal class FinalBar {}\n");
        let classes = find_classes(f.path()).unwrap();
        assert!(classes.contains(&"AbstractFoo".to_string()));
        assert!(classes.contains(&"FinalBar".to_string()));
    }

    #[test]
    fn test_find_classes_non_php_extension() {
        let mut f = NamedTempFile::with_suffix(".txt").unwrap();
        f.write_all(b"<?php\nclass Foo {}\n").unwrap();
        let classes = find_classes(f.path()).unwrap();
        assert!(classes.is_empty(), "non-PHP extension should be skipped");
    }

    // -------------------------------------------------------------------------
    // PSR-4 validation tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_psr4_correct() {
        assert!(validate_psr4_class(
            "Foo\\Bar\\Baz",
            "Foo\\Bar\\",
            "/srv/project/src/Baz.php",
            "/srv/project/src"
        ));
    }

    #[test]
    fn test_validate_psr4_wrong_path() {
        assert!(!validate_psr4_class(
            "Foo\\Bar\\Baz",
            "Foo\\Bar\\",
            "/srv/project/src/Wrong.php",
            "/srv/project/src"
        ));
    }

    #[test]
    fn test_validate_psr4_namespace_mismatch() {
        assert!(!validate_psr4_class(
            "Other\\Baz",
            "Foo\\Bar\\",
            "/srv/project/src/Baz.php",
            "/srv/project/src"
        ));
    }

    #[test]
    fn test_validate_psr4_nested() {
        assert!(validate_psr4_class(
            "App\\Http\\Controllers\\HomeController",
            "App\\",
            "/project/src/Http/Controllers/HomeController.php",
            "/project/src"
        ));
    }

    // -------------------------------------------------------------------------
    // PSR-0 validation tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_psr0_simple() {
        assert!(validate_psr0_class(
            "Foo_Bar_Baz",
            "/srv/project/src/Foo/Bar/Baz.php",
            "/srv/project/src"
        ));
    }

    #[test]
    fn test_validate_psr0_with_namespace() {
        assert!(validate_psr0_class(
            "Foo\\Bar",
            "/srv/project/src/Foo/Bar.php",
            "/srv/project/src"
        ));
    }

    #[test]
    fn test_validate_psr0_wrong_path() {
        assert!(!validate_psr0_class(
            "Foo_Bar",
            "/srv/project/src/Foo/Baz.php",
            "/srv/project/src"
        ));
    }
}
