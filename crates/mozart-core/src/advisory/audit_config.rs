use crate::advisory::{AbandonedHandling, AuditFormat};
use crate::config::Config;

/// ref: \Composer\Advisory\AuditConfig
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Whether to run audit
    pub audit: bool,
    pub audit_format: AuditFormat,
    pub audit_abandoned: AbandonedHandling,
    /// Should insecure versions be blocked during a composer update/required command
    pub block_insecure: bool,
    /// Should abandoned packages be blocked during a composer update/required command
    pub block_abandoned: bool,
    /// Should repositories that are unreachable or return a non-200 status code be ignored.
    pub ignore_unreachable: bool,
    /// List of advisory IDs to ignore during auditing => reason for ignoring
    pub ignore_list_for_audit: indexmap::IndexMap<String, Option<String>>,
    /// List of advisory IDs to ignore during blocking
    pub ignore_list_for_blocking: indexmap::IndexMap<String, Option<String>>,
    /// List of severities to ignore during auditing
    pub ignore_severity_for_audit: indexmap::IndexMap<String, Option<String>>,
    /// List of severities to ignore during blocking
    pub ignore_severity_for_blocking: indexmap::IndexMap<String, Option<String>>,
    /// List of abandoned packages to ignore during auditing
    pub ignore_abandoned_for_audit: indexmap::IndexMap<String, Option<String>>,
    /// List of abandoned packages to ignore during blocking
    pub ignore_abandoned_for_blocking: indexmap::IndexMap<String, Option<String>>,
}

impl AuditConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        audit: bool,
        audit_format: AuditFormat,
        audit_abandoned: AbandonedHandling,
        block_insecure: bool,
        block_abandoned: bool,
        ignore_unreachable: bool,
        ignore_list_for_audit: indexmap::IndexMap<String, Option<String>>,
        ignore_list_for_blocking: indexmap::IndexMap<String, Option<String>>,
        ignore_severity_for_audit: indexmap::IndexMap<String, Option<String>>,
        ignore_severity_for_blocking: indexmap::IndexMap<String, Option<String>>,
        ignore_abandoned_for_audit: indexmap::IndexMap<String, Option<String>>,
        ignore_abandoned_for_blocking: indexmap::IndexMap<String, Option<String>>,
    ) -> Self {
        Self {
            audit,
            audit_format,
            audit_abandoned,
            block_insecure,
            block_abandoned,
            ignore_unreachable,
            ignore_list_for_audit,
            ignore_list_for_blocking,
            ignore_severity_for_audit,
            ignore_severity_for_blocking,
            ignore_abandoned_for_audit,
            ignore_abandoned_for_blocking,
        }
    }

    pub fn from_config(
        config: &Config,
        audit: bool,
        audit_format: AuditFormat,
    ) -> anyhow::Result<Self> {
        let empty_arr = serde_json::Value::Array(vec![]);
        let audit_val = config
            .get("audit")
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

        let ignore_list_val = audit_val
            .get("ignore")
            .cloned()
            .unwrap_or_else(|| empty_arr.clone());
        let ignore_list_parsed = Self::parse_ignore_with_apply(&ignore_list_val)?;

        let ignore_abandoned_val = audit_val
            .get("ignore-abandoned")
            .cloned()
            .unwrap_or_else(|| empty_arr.clone());
        let ignore_abandoned_parsed = Self::parse_ignore_with_apply(&ignore_abandoned_val)?;

        let ignore_severity_val = audit_val
            .get("ignore-severity")
            .cloned()
            .unwrap_or_else(|| empty_arr.clone());
        let ignore_severity_parsed = Self::parse_ignore_with_apply(&ignore_severity_val)?;

        let audit_abandoned = audit_val
            .get("abandoned")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<AbandonedHandling>().ok())
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

        Ok(Self {
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
        })
    }

    /// Parse ignore configuration supporting both simple and detailed formats with apply scopes
    ///
    /// Simple format: ['CVE-123', 'CVE-456'] or ['CVE-123' => 'reason']
    /// Detailed format: ['CVE-123' => ['apply' => 'audit|block|all', 'reason' => '...']]
    fn parse_ignore_with_apply(config: &serde_json::Value) -> anyhow::Result<ParsedIgnore> {
        let mut for_audit = indexmap::IndexMap::new();
        let mut for_block = indexmap::IndexMap::new();

        match config {
            serde_json::Value::Array(arr) => {
                // Simple format: ['CVE-123']
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
                        serde_json::Value::String(r) => {
                            // Simple format with reason: ['CVE-123' => 'reason']
                            ("all", Some(r.clone()))
                        }
                        serde_json::Value::Null => {
                            // Simple format with null: ['CVE-123' => null]
                            ("all", None)
                        }
                        serde_json::Value::Object(detail) => {
                            // Detailed format: ['CVE-123' => ['apply' => '...', 'reason' => '...']]
                            let apply = detail
                                .get("apply")
                                .and_then(|v| v.as_str())
                                .unwrap_or("all");
                            let reason = detail
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());

                            if !matches!(apply, "audit" | "block" | "all") {
                                anyhow::bail!(
                                    "Invalid 'apply' value for '{key}': {apply}. Expected 'audit', 'block', or 'all'."
                                );
                            }
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

        Ok(ParsedIgnore {
            audit: for_audit,
            block: for_block,
        })
    }
}

struct ParsedIgnore {
    audit: indexmap::IndexMap<String, Option<String>>,
    block: indexmap::IndexMap<String, Option<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ignore_simple_array() {
        let val = serde_json::json!(["CVE-2024-1234", "PKSA-0001"]);
        let parsed = AuditConfig::parse_ignore_with_apply(&val).unwrap();
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
        let parsed = AuditConfig::parse_ignore_with_apply(&val).unwrap();
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
        let parsed = AuditConfig::parse_ignore_with_apply(&val).unwrap();
        assert_eq!(parsed.audit.len(), 1);
        assert_eq!(parsed.block.len(), 0);
    }

    #[test]
    fn test_parse_ignore_detailed_apply_block_only() {
        let val = serde_json::json!({
            "CVE-2024-1234": { "apply": "block", "reason": "test" }
        });
        let parsed = AuditConfig::parse_ignore_with_apply(&val).unwrap();
        assert_eq!(parsed.audit.len(), 0);
        assert_eq!(parsed.block.len(), 1);
    }

    #[test]
    fn test_parse_ignore_detailed_apply_all() {
        let val = serde_json::json!({
            "CVE-2024-1234": { "apply": "all", "reason": "test" }
        });
        let parsed = AuditConfig::parse_ignore_with_apply(&val).unwrap();
        assert_eq!(parsed.audit.len(), 1);
        assert_eq!(parsed.block.len(), 1);
    }

    #[test]
    fn test_audit_config_defaults() {
        let config = Config::default();
        let audit_config = AuditConfig::from_config(&config, true, AuditFormat::Table).unwrap();
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

        let audit_config = AuditConfig::from_config(&config, true, AuditFormat::Summary).unwrap();
        assert_eq!(audit_config.audit_abandoned, AbandonedHandling::Report);
        assert!(!audit_config.block_insecure);
        assert!(audit_config.ignore_unreachable);
        assert_eq!(audit_config.ignore_list_for_audit.len(), 1);
        assert_eq!(audit_config.ignore_severity_for_audit.len(), 1);
    }
}
