use std::collections::HashMap;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/spdx_data.rs"));

/// Information about an SPDX license.
#[derive(Debug, Clone)]
pub struct LicenseInfo {
    pub identifier: &'static str,
    pub full_name: &'static str,
    pub osi_approved: bool,
    pub deprecated: bool,
}

/// Information about an SPDX license exception.
#[derive(Debug, Clone)]
pub struct ExceptionInfo {
    pub identifier: &'static str,
    pub full_name: &'static str,
}

/// SPDX license database with expression validation.
pub struct SpdxLicenses {
    licenses: HashMap<&'static str, LicenseInfo>,
    exceptions: HashMap<&'static str, ExceptionInfo>,
    name_to_id: HashMap<&'static str, &'static str>,
}

impl SpdxLicenses {
    /// Build the license database from generated data.
    pub fn new() -> Self {
        let mut licenses = HashMap::with_capacity(LICENSES.len());
        let mut name_to_id = HashMap::with_capacity(LICENSES.len());
        for &(lower, id, full_name, osi, deprecated) in LICENSES {
            licenses.insert(
                lower,
                LicenseInfo {
                    identifier: id,
                    full_name,
                    osi_approved: osi,
                    deprecated,
                },
            );
            name_to_id.insert(full_name, id);
        }

        let mut exceptions = HashMap::with_capacity(EXCEPTIONS.len());
        for &(lower, id, full_name) in EXCEPTIONS {
            exceptions.insert(
                lower,
                ExceptionInfo {
                    identifier: id,
                    full_name,
                },
            );
        }

        Self {
            licenses,
            exceptions,
            name_to_id,
        }
    }

    /// Look up a license by its SPDX identifier (case-insensitive).
    pub fn get_license_by_identifier(&self, id: &str) -> Option<&LicenseInfo> {
        self.licenses.get(id.to_lowercase().as_str())
    }

    /// Look up an exception by its SPDX identifier (case-insensitive).
    pub fn get_exception_by_identifier(&self, id: &str) -> Option<&ExceptionInfo> {
        self.exceptions.get(id.to_lowercase().as_str())
    }

    /// Look up a license identifier by its full name.
    pub fn get_identifier_by_name(&self, name: &str) -> Option<&str> {
        self.name_to_id.get(name).copied()
    }

    /// Check if a license is OSI-approved.
    pub fn is_osi_approved(&self, id: &str) -> bool {
        self.get_license_by_identifier(id)
            .is_some_and(|l| l.osi_approved)
    }

    /// Check if a license is deprecated.
    pub fn is_deprecated(&self, id: &str) -> bool {
        self.get_license_by_identifier(id)
            .is_some_and(|l| l.deprecated)
    }

    /// Validate an SPDX license expression.
    ///
    /// Supports compound expressions with AND/OR, the WITH operator for
    /// exceptions, the `+` (or-later) operator, LicenseRef, and the special
    /// values `NONE` and `NOASSERTION`.
    pub fn validate(&self, license: &str) -> bool {
        let license = license.trim();
        if license.is_empty() {
            return false;
        }

        // Special values
        if license.eq_ignore_ascii_case("NONE") || license.eq_ignore_ascii_case("NOASSERTION") {
            return true;
        }

        let mut parser = Parser::new(license, self);
        parser.parse_expression() && parser.is_at_end()
    }

    /// Validate a list of SPDX license identifiers (joined with OR).
    pub fn validate_list(&self, licenses: &[&str]) -> bool {
        if licenses.is_empty() {
            return false;
        }
        let expr = licenses.join(" OR ");
        self.validate(&expr)
    }

    fn is_valid_license_id(&self, id: &str) -> bool {
        self.licenses.contains_key(id.to_lowercase().as_str())
    }

    fn is_valid_exception_id(&self, id: &str) -> bool {
        self.exceptions.contains_key(id.to_lowercase().as_str())
    }
}

impl Default for SpdxLicenses {
    fn default() -> Self {
        Self::new()
    }
}

/// Global static SPDX license database.
static SPDX: LazyLock<SpdxLicenses> = LazyLock::new(SpdxLicenses::new);

/// Get a reference to the global SPDX license database.
pub fn spdx() -> &'static SpdxLicenses {
    &SPDX
}

// ---------------------------------------------------------------------------
// SPDX expression parser (recursive descent)
// ---------------------------------------------------------------------------
//
// Grammar:
//   expression     = compound_expr
//   compound_expr  = head_expr (("AND" | "OR") compound_expr)?
//   head_expr      = simple_expr ("WITH" exception_id)?
//                  | "(" compound_expr ")"
//   simple_expr    = license_id "+"?
//                  | license_ref
//   license_ref    = ("DocumentRef-" idstring ":")? "LicenseRef-" idstring
//   idstring       = [a-zA-Z0-9-.]+

struct Parser<'a> {
    tokens: Vec<&'a str>,
    pos: usize,
    db: &'a SpdxLicenses,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, db: &'a SpdxLicenses) -> Self {
        let tokens = Self::tokenize(input);
        Self {
            tokens,
            pos: 0,
            db,
        }
    }

    fn tokenize(input: &str) -> Vec<&str> {
        let mut tokens = Vec::new();
        let mut chars = input.char_indices().peekable();

        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
                continue;
            }
            if c == '(' || c == ')' || c == '+' {
                tokens.push(&input[i..i + 1]);
                chars.next();
                continue;
            }
            // Identifier or keyword: consume until whitespace or special char
            let start = i;
            loop {
                chars.next();
                match chars.peek() {
                    Some(&(_, ch)) if !ch.is_whitespace() && ch != '(' && ch != ')' => {
                        // '+' only breaks if it's right after an identifier
                        if ch == '+' {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            let end = chars.peek().map_or(input.len(), |&(j, _)| j);
            tokens.push(&input[start..end]);
        }

        tokens
    }

    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<&'a str> {
        let tok = self.tokens.get(self.pos).copied();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn expect(&mut self, expected: &str) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Parse the top-level expression.
    fn parse_expression(&mut self) -> bool {
        self.parse_compound_expr()
    }

    /// compound_expr = head_expr (("AND" | "OR") compound_expr)?
    fn parse_compound_expr(&mut self) -> bool {
        if !self.parse_head_expr() {
            return false;
        }

        if let Some(tok) = self.peek()
            && (tok == "AND" || tok == "OR")
        {
            self.advance();
            return self.parse_compound_expr();
        }

        true
    }

    /// head_expr = "(" compound_expr ")" | simple_expr ("WITH" exception_id)?
    fn parse_head_expr(&mut self) -> bool {
        if self.expect("(") {
            if !self.parse_compound_expr() {
                return false;
            }
            return self.expect(")");
        }

        if !self.parse_simple_expr() {
            return false;
        }

        // Optional WITH clause
        if self.peek() == Some("WITH") {
            self.advance();
            return self.parse_exception_id();
        }

        true
    }

    /// simple_expr = license_ref | license_id "+"?
    fn parse_simple_expr(&mut self) -> bool {
        let tok = match self.peek() {
            Some(t) => t,
            None => return false,
        };

        // LicenseRef / DocumentRef
        if tok.starts_with("LicenseRef-") || tok.starts_with("DocumentRef-") {
            return self.parse_license_ref();
        }

        // Regular license identifier — could be multi-token with "-"
        // We just consume the current token and check
        self.advance();

        // Handle '+' (or-later) operator
        if self.peek() == Some("+") {
            self.advance();
        }

        self.db.is_valid_license_id(tok)
    }

    /// license_ref = ("DocumentRef-" idstring ":")? "LicenseRef-" idstring
    fn parse_license_ref(&mut self) -> bool {
        let tok = match self.advance() {
            Some(t) => t,
            None => return false,
        };

        if let Some(rest) = tok.strip_prefix("DocumentRef-") {
            // Must contain ":LicenseRef-" within
            if let Some(colon_pos) = rest.find(":LicenseRef-") {
                let doc_id = &rest[..colon_pos];
                let license_ref_id = &rest[colon_pos + ":LicenseRef-".len()..];
                return is_valid_idstring(doc_id) && is_valid_idstring(license_ref_id);
            }
            return false;
        }

        if let Some(id) = tok.strip_prefix("LicenseRef-") {
            return is_valid_idstring(id);
        }

        false
    }

    fn parse_exception_id(&mut self) -> bool {
        match self.advance() {
            Some(id) => self.db.is_valid_exception_id(id),
            None => false,
        }
    }
}

/// Check that a string matches `[a-zA-Z0-9.-]+`.
fn is_valid_idstring(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_identifiers() {
        let db = spdx();
        assert!(db.validate("MIT"));
        assert!(db.validate("Apache-2.0"));
        assert!(db.validate("GPL-3.0-only"));
        assert!(db.validate("0BSD"));
    }

    #[test]
    fn case_insensitive() {
        let db = spdx();
        assert!(db.validate("mit"));
        assert!(db.validate("apache-2.0"));
        assert!(db.validate("Mit"));
    }

    #[test]
    fn or_expression() {
        let db = spdx();
        assert!(db.validate("MIT OR Apache-2.0"));
    }

    #[test]
    fn and_expression() {
        let db = spdx();
        assert!(db.validate("MIT AND Apache-2.0"));
    }

    #[test]
    fn with_exception() {
        let db = spdx();
        assert!(db.validate("GPL-2.0-only WITH Classpath-exception-2.0"));
    }

    #[test]
    fn complex_expression() {
        let db = spdx();
        assert!(db.validate("(MIT AND Apache-2.0) OR GPL-3.0-only"));
        assert!(db.validate("(MIT OR Apache-2.0) AND (GPL-2.0-only OR BSD-2-Clause)"));
    }

    #[test]
    fn special_values() {
        let db = spdx();
        assert!(db.validate("NONE"));
        assert!(db.validate("NOASSERTION"));
        assert!(db.validate("none"));
        assert!(db.validate("noassertion"));
    }

    #[test]
    fn or_later_operator() {
        let db = spdx();
        assert!(db.validate("Apache-2.0+"));
        assert!(db.validate("GPL-2.0-only+"));
    }

    #[test]
    fn license_ref() {
        let db = spdx();
        assert!(db.validate("LicenseRef-custom"));
        assert!(db.validate("LicenseRef-my-license.1"));
        assert!(db.validate("DocumentRef-spdx-tool-1.2:LicenseRef-MIT-Style-2"));
    }

    #[test]
    fn invalid_expressions() {
        let db = spdx();
        assert!(!db.validate(""));
        assert!(!db.validate("totally-not-a-license"));
        assert!(!db.validate("MIT AND"));
        assert!(!db.validate("AND MIT"));
        assert!(!db.validate("MIT OR"));
        assert!(!db.validate("(MIT"));
        assert!(!db.validate("MIT)"));
        assert!(!db.validate("MIT WITH"));
        assert!(!db.validate("MIT WITH not-an-exception"));
    }

    #[test]
    fn validate_list() {
        let db = spdx();
        assert!(db.validate_list(&["MIT", "Apache-2.0"]));
        assert!(!db.validate_list(&[]));
        assert!(!db.validate_list(&["not-valid"]));
    }

    #[test]
    fn license_lookup() {
        let db = spdx();
        let mit = db.get_license_by_identifier("MIT").unwrap();
        assert_eq!(mit.identifier, "MIT");
        assert!(mit.osi_approved);
        assert!(!mit.deprecated);

        assert!(db.get_license_by_identifier("mit").is_some());
        assert!(db.get_license_by_identifier("nonexistent").is_none());
    }

    #[test]
    fn exception_lookup() {
        let db = spdx();
        let exc = db
            .get_exception_by_identifier("Classpath-exception-2.0")
            .unwrap();
        assert_eq!(exc.identifier, "Classpath-exception-2.0");
    }

    #[test]
    fn name_lookup() {
        let db = spdx();
        assert_eq!(
            db.get_identifier_by_name("MIT License"),
            Some("MIT")
        );
    }

    #[test]
    fn osi_and_deprecated() {
        let db = spdx();
        assert!(db.is_osi_approved("MIT"));
        assert!(!db.is_osi_approved("nonexistent"));
        assert!(!db.is_deprecated("MIT"));
    }
}
