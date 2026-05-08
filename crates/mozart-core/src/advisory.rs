use indexmap::IndexMap;

use crate::config::Config;

pub const FORMAT_TABLE: &str = "table";
pub const FORMAT_PLAIN: &str = "plain";
pub const FORMAT_JSON: &str = "json";
pub const FORMAT_SUMMARY: &str = "summary";
pub const FORMATS: [&str; 4] = [FORMAT_TABLE, FORMAT_PLAIN, FORMAT_JSON, FORMAT_SUMMARY];

pub const ABANDONED_IGNORE: &str = "ignore";
pub const ABANDONED_REPORT: &str = "report";
pub const ABANDONED_FAIL: &str = "fail";
pub const ABANDONEDS: [&str; 3] = [ABANDONED_IGNORE, ABANDONED_REPORT, ABANDONED_FAIL];

/// Mirrors `Auditor::FORMAT_*` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuditFormat {
    #[default]
    Table,
    Plain,
    Json,
    Summary,
}

impl AuditFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => FORMAT_TABLE,
            Self::Plain => FORMAT_PLAIN,
            Self::Json => FORMAT_JSON,
            Self::Summary => FORMAT_SUMMARY,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            FORMAT_TABLE => Some(Self::Table),
            FORMAT_PLAIN => Some(Self::Plain),
            FORMAT_JSON => Some(Self::Json),
            FORMAT_SUMMARY => Some(Self::Summary),
            _ => None,
        }
    }
}

/// Mirrors `Auditor::ABANDONED_*` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AbandonedHandling {
    Ignore,
    Report,
    #[default]
    Fail,
}

impl AbandonedHandling {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => ABANDONED_IGNORE,
            Self::Report => ABANDONED_REPORT,
            Self::Fail => ABANDONED_FAIL,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            ABANDONED_IGNORE => Some(Self::Ignore),
            ABANDONED_REPORT => Some(Self::Report),
            ABANDONED_FAIL => Some(Self::Fail),
            _ => None,
        }
    }
}

/// Mirrors `Composer\Advisory\AuditConfig`.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    pub audit: bool,
    pub audit_format: AuditFormat,
    pub audit_abandoned: AbandonedHandling,
    pub block_insecure: bool,
    pub block_abandoned: bool,
    pub ignore_unreachable: bool,
    pub ignore_list_for_audit: IndexMap<String, Option<String>>,
    pub ignore_list_for_blocking: IndexMap<String, Option<String>>,
    pub ignore_severity_for_audit: IndexMap<String, Option<String>>,
    pub ignore_severity_for_blocking: IndexMap<String, Option<String>>,
    pub ignore_abandoned_for_audit: IndexMap<String, Option<String>>,
    pub ignore_abandoned_for_blocking: IndexMap<String, Option<String>>,
}

struct ParsedIgnore {
    audit: IndexMap<String, Option<String>>,
    block: IndexMap<String, Option<String>>,
}

/// Mirrors `AuditConfig::parseIgnoreWithApply()`.
///
/// Supports these JSON shapes:
/// - `["CVE-1"]` — simple list, apply=all, reason=null
/// - `{"CVE-1": "reason"}` — with reason, apply=all
/// - `{"CVE-1": null}` — null reason, apply=all
/// - `{"CVE-1": {"apply": "audit|block|all", "reason": "..."}}` — detailed
fn parse_ignore_with_apply(config: &serde_json::Value) -> ParsedIgnore {
    let mut for_audit: IndexMap<String, Option<String>> = IndexMap::new();
    let mut for_block: IndexMap<String, Option<String>> = IndexMap::new();

    match config {
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    for_audit.insert(s.to_string(), None);
                    for_block.insert(s.to_string(), None);
                }
            }
        }
        serde_json::Value::Object(obj) => {
            for (key, value) in obj {
                let (apply, reason) = match value {
                    serde_json::Value::String(r) => ("all", Some(r.clone())),
                    serde_json::Value::Null => ("all", None),
                    serde_json::Value::Object(detail) => {
                        let apply = detail
                            .get("apply")
                            .and_then(|v| v.as_str())
                            .unwrap_or("all");
                        if !matches!(apply, "audit" | "block" | "all") {
                            continue;
                        }
                        let reason = detail
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        (apply, reason)
                    }
                    _ => continue,
                };

                if apply == "audit" || apply == "all" {
                    for_audit.insert(key.clone(), reason.clone());
                }
                if apply == "block" || apply == "all" {
                    for_block.insert(key.clone(), reason);
                }
            }
        }
        _ => {}
    }

    ParsedIgnore {
        audit: for_audit,
        block: for_block,
    }
}

impl AuditConfig {
    /// Mirrors `AuditConfig::fromConfig()`.
    pub fn from_config(config: &Config, audit: bool, audit_format: AuditFormat) -> Self {
        let empty_arr = serde_json::Value::Array(vec![]);
        let audit_val = config
            .get("audit")
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

        let ignore_list_val = audit_val
            .get("ignore")
            .cloned()
            .unwrap_or_else(|| empty_arr.clone());
        let ignore_list_parsed = parse_ignore_with_apply(&ignore_list_val);

        let ignore_abandoned_val = audit_val
            .get("ignore-abandoned")
            .cloned()
            .unwrap_or_else(|| empty_arr.clone());
        let ignore_abandoned_parsed = parse_ignore_with_apply(&ignore_abandoned_val);

        let ignore_severity_val = audit_val
            .get("ignore-severity")
            .cloned()
            .unwrap_or_else(|| empty_arr.clone());
        let ignore_severity_parsed = parse_ignore_with_apply(&ignore_severity_val);

        let audit_abandoned = audit_val
            .get("abandoned")
            .and_then(|v| v.as_str())
            .and_then(AbandonedHandling::from_str)
            .unwrap_or_default();

        let block_insecure = audit_val
            .get("block-insecure")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let block_abandoned = audit_val
            .get("block-abandoned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let ignore_unreachable = audit_val
            .get("ignore-unreachable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Self {
            audit,
            audit_format,
            audit_abandoned,
            block_insecure,
            block_abandoned,
            ignore_unreachable,
            ignore_list_for_audit: ignore_list_parsed.audit,
            ignore_list_for_blocking: ignore_list_parsed.block,
            ignore_severity_for_audit: ignore_severity_parsed.audit,
            ignore_severity_for_blocking: ignore_severity_parsed.block,
            ignore_abandoned_for_audit: ignore_abandoned_parsed.audit,
            ignore_abandoned_for_blocking: ignore_abandoned_parsed.block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ignore_simple_array() {
        let val = serde_json::json!(["CVE-2024-1234", "PKSA-0001"]);
        let parsed = parse_ignore_with_apply(&val);
        assert_eq!(parsed.audit.len(), 2);
        assert_eq!(parsed.block.len(), 2);
        assert_eq!(parsed.audit.get("CVE-2024-1234"), Some(&None));
    }

    #[test]
    fn test_parse_ignore_object_with_reasons() {
        let val = serde_json::json!({
            "CVE-2024-1234": "manually patched",
            "PKSA-0001": null
        });
        let parsed = parse_ignore_with_apply(&val);
        assert_eq!(
            parsed.audit.get("CVE-2024-1234"),
            Some(&Some("manually patched".to_string()))
        );
        assert_eq!(parsed.audit.get("PKSA-0001"), Some(&None));
    }

    #[test]
    fn test_parse_ignore_detailed_apply_audit_only() {
        let val = serde_json::json!({
            "CVE-2024-1234": { "apply": "audit", "reason": "test" }
        });
        let parsed = parse_ignore_with_apply(&val);
        assert_eq!(parsed.audit.len(), 1);
        assert_eq!(parsed.block.len(), 0);
    }

    #[test]
    fn test_parse_ignore_detailed_apply_block_only() {
        let val = serde_json::json!({
            "CVE-2024-1234": { "apply": "block", "reason": "test" }
        });
        let parsed = parse_ignore_with_apply(&val);
        assert_eq!(parsed.audit.len(), 0);
        assert_eq!(parsed.block.len(), 1);
    }

    #[test]
    fn test_parse_ignore_detailed_apply_all() {
        let val = serde_json::json!({
            "CVE-2024-1234": { "apply": "all", "reason": "test" }
        });
        let parsed = parse_ignore_with_apply(&val);
        assert_eq!(parsed.audit.len(), 1);
        assert_eq!(parsed.block.len(), 1);
    }

    #[test]
    fn test_audit_config_defaults() {
        let config = Config::default();
        let audit_config = AuditConfig::from_config(&config, true, AuditFormat::Table);
        assert!(audit_config.audit);
        assert_eq!(audit_config.audit_format, AuditFormat::Table);
        assert_eq!(audit_config.audit_abandoned, AbandonedHandling::Fail);
        assert!(audit_config.block_insecure);
        assert!(!audit_config.block_abandoned);
        assert!(!audit_config.ignore_unreachable);
        assert!(audit_config.ignore_list_for_audit.is_empty());
        assert!(audit_config.ignore_severity_for_audit.is_empty());
    }

    #[test]
    fn test_audit_config_from_config_with_audit_section() {
        use std::collections::BTreeMap;
        let mut config = Config::default();
        config
            .merge(&BTreeMap::from([(
                "audit".to_string(),
                serde_json::json!({
                    "ignore": ["CVE-2024-1234"],
                    "ignore-severity": {"low": "low severity not critical"},
                    "abandoned": "report",
                    "block-insecure": false,
                    "ignore-unreachable": true
                }),
            )]))
            .unwrap();

        let audit_config = AuditConfig::from_config(&config, true, AuditFormat::Summary);
        assert_eq!(audit_config.audit_abandoned, AbandonedHandling::Report);
        assert!(!audit_config.block_insecure);
        assert!(audit_config.ignore_unreachable);
        assert_eq!(audit_config.ignore_list_for_audit.len(), 1);
        assert_eq!(audit_config.ignore_severity_for_audit.len(), 1);
    }
}
