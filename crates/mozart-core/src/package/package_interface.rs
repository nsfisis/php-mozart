use crate::package::{AutoloadRules, Link, Stability};

/// ref: \Composer\Package\PackageInterface
pub trait PackageInterface {
    fn name(&self) -> &str;
    fn pretty_name(&self) -> &str;
    fn version(&self) -> &str;
    fn pretty_version(&self) -> &str;
    fn package_type(&self) -> &str;
    fn target_dir(&self) -> Option<&str>;
    fn source_type(&self) -> Option<&str>;
    fn source_url(&self) -> Option<&str>;
    fn source_reference(&self) -> Option<&str>;
    fn dist_type(&self) -> Option<&str>;
    fn dist_url(&self) -> Option<&str>;
    fn dist_reference(&self) -> Option<&str>;
    fn dist_sha1_checksum(&self) -> Option<&str>;
    fn release_date(&self) -> Option<&str>;
    fn extra(&self) -> &indexmap::IndexMap<String, serde_json::Value>;
    fn binaries(&self) -> &[String];
    fn is_dev(&self) -> bool;
    fn stability(&self) -> Stability;
    fn notification_url(&self) -> Option<&str>;
    fn requires(&self) -> &indexmap::IndexMap<String, Link>;
    fn conflicts(&self) -> &indexmap::IndexMap<String, Link>;
    fn provides(&self) -> &indexmap::IndexMap<String, Link>;
    fn replaces(&self) -> &indexmap::IndexMap<String, Link>;
    fn dev_requires(&self) -> &indexmap::IndexMap<String, Link>;
    fn suggests(&self) -> &indexmap::IndexMap<String, String>;
    fn autoload(&self) -> &AutoloadRules;
    fn dev_autoload(&self) -> &AutoloadRules;
    fn is_default_branch(&self) -> bool;
}
