use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Package stability level.
/// Higher value = less stable.
/// Corresponds to `Composer\Package\BasePackage::STABILITY_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum Stability {
    #[default]
    Stable = 0,
    RC = 5,
    Beta = 10,
    Alpha = 15,
    Dev = 20,
}

/// A versioned relationship between two packages.
/// Corresponds to `Composer\Package\Link`.
#[derive(Debug, Clone)]
pub struct Link {
    pub source: String,
    pub target: String,
    pub constraint: String,
    pub pretty_constraint: Option<String>,
    pub description: String,
}

/// Package author metadata.
#[derive(Debug, Clone)]
pub struct Author {
    pub name: Option<String>,
    pub email: Option<String>,
    pub homepage: Option<String>,
    pub role: Option<String>,
}

/// Autoload rule sets (PSR-4, PSR-0, classmap, files).
#[derive(Debug, Clone, Default)]
pub struct AutoloadRules {
    pub psr4: BTreeMap<String, Vec<String>>,
    pub psr0: BTreeMap<String, Vec<String>>,
    pub classmap: Vec<String>,
    pub files: Vec<String>,
}

/// Support channel information.
#[derive(Debug, Clone, Default)]
pub struct Support {
    pub email: Option<String>,
    pub issues: Option<String>,
    pub forum: Option<String>,
    pub wiki: Option<String>,
    pub source: Option<String>,
    pub docs: Option<String>,
    pub irc: Option<String>,
    pub chat: Option<String>,
    pub rss: Option<String>,
    pub security: Option<String>,
}

/// Funding link.
#[derive(Debug, Clone)]
pub struct Funding {
    pub url: Option<String>,
    pub funding_type: Option<String>,
}

/// Version alias entry for root packages.
#[derive(Debug, Clone)]
pub struct VersionAlias {
    pub package: String,
    pub version: String,
    pub alias: String,
    pub alias_normalized: String,
}

/// Core package data covering `BasePackage` + `Package` fields.
/// Corresponds to `Composer\Package\Package` (implements `PackageInterface`).
#[derive(Debug, Clone)]
pub struct PackageData {
    // BasePackage fields
    pub name: String,
    pub pretty_name: String,

    // Package fields
    pub version: String,
    pub pretty_version: String,
    pub package_type: String,
    pub target_dir: Option<String>,

    // source
    pub source_type: Option<String>,
    pub source_url: Option<String>,
    pub source_reference: Option<String>,

    // dist
    pub dist_type: Option<String>,
    pub dist_url: Option<String>,
    pub dist_reference: Option<String>,
    pub dist_sha1_checksum: Option<String>,

    pub release_date: Option<String>,
    pub extra: BTreeMap<String, serde_json::Value>,
    pub binaries: Vec<String>,
    pub dev: bool,
    pub stability: Stability,
    pub notification_url: Option<String>,

    // dependency links
    pub requires: BTreeMap<String, Link>,
    pub conflicts: BTreeMap<String, Link>,
    pub provides: BTreeMap<String, Link>,
    pub replaces: BTreeMap<String, Link>,
    pub dev_requires: BTreeMap<String, Link>,
    pub suggests: BTreeMap<String, String>,

    // autoload
    pub autoload: AutoloadRules,
    pub dev_autoload: AutoloadRules,

    pub is_default_branch: bool,
}

/// Package with full metadata (description, authors, license, etc.).
/// Corresponds to `Composer\Package\CompletePackage`.
#[derive(Debug, Clone)]
pub struct CompletePackageData {
    pub package: PackageData,

    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Vec<String>,
    pub keywords: Vec<String>,
    pub authors: Vec<Author>,
    pub scripts: BTreeMap<String, Vec<String>>,
    pub support: Support,
    pub funding: Vec<Funding>,
    pub repositories: Vec<serde_json::Value>,
    /// `None` = not abandoned, `Some("")` = abandoned, `Some(pkg)` = replaced by pkg.
    pub abandoned: Option<String>,
    pub archive_name: Option<String>,
    pub archive_excludes: Vec<String>,
}

/// The root project package with project-level configuration.
/// Corresponds to `Composer\Package\RootPackage`.
#[derive(Debug, Clone)]
pub struct RootPackageData {
    pub complete: CompletePackageData,

    pub minimum_stability: Stability,
    pub prefer_stable: bool,
    pub stability_flags: BTreeMap<String, Stability>,
    pub config: BTreeMap<String, serde_json::Value>,
    pub references: BTreeMap<String, String>,
    pub aliases: Vec<VersionAlias>,
}

/// Accessor for `PackageData` fields.
/// Corresponds to `Composer\Package\PackageInterface`.
pub trait Package {
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
    fn extra(&self) -> &BTreeMap<String, serde_json::Value>;
    fn binaries(&self) -> &[String];
    fn is_dev(&self) -> bool;
    fn stability(&self) -> Stability;
    fn notification_url(&self) -> Option<&str>;
    fn requires(&self) -> &BTreeMap<String, Link>;
    fn conflicts(&self) -> &BTreeMap<String, Link>;
    fn provides(&self) -> &BTreeMap<String, Link>;
    fn replaces(&self) -> &BTreeMap<String, Link>;
    fn dev_requires(&self) -> &BTreeMap<String, Link>;
    fn suggests(&self) -> &BTreeMap<String, String>;
    fn autoload(&self) -> &AutoloadRules;
    fn dev_autoload(&self) -> &AutoloadRules;
    fn is_default_branch(&self) -> bool;
}

/// Accessor for `CompletePackageData` fields.
/// Corresponds to `Composer\Package\CompletePackageInterface`.
pub trait CompletePackage: Package {
    fn description(&self) -> Option<&str>;
    fn homepage(&self) -> Option<&str>;
    fn license(&self) -> &[String];
    fn keywords(&self) -> &[String];
    fn authors(&self) -> &[Author];
    fn scripts(&self) -> &BTreeMap<String, Vec<String>>;
    fn support(&self) -> &Support;
    fn funding(&self) -> &[Funding];
    fn repositories(&self) -> &[serde_json::Value];
    fn abandoned(&self) -> Option<&str>;
    fn archive_name(&self) -> Option<&str>;
    fn archive_excludes(&self) -> &[String];
}

/// Accessor for `RootPackageData` fields.
/// Corresponds to `Composer\Package\RootPackageInterface`.
pub trait RootPackage: CompletePackage {
    fn minimum_stability(&self) -> Stability;
    fn prefer_stable(&self) -> bool;
    fn stability_flags(&self) -> &BTreeMap<String, Stability>;
    fn config(&self) -> &BTreeMap<String, serde_json::Value>;
    fn references(&self) -> &BTreeMap<String, String>;
    fn aliases(&self) -> &[VersionAlias];
}

// ──────────────────────────────────────────────
// Delegation macros
// ──────────────────────────────────────────────

/// Implements `Package` trait by delegating to an inner `PackageData` field.
macro_rules! delegate_package {
    ($type:ty => $($path:ident).+) => {
        impl Package for $type {
            fn name(&self)               -> &str                                 { &self.$($path).+.name                          }
            fn pretty_name(&self)        -> &str                                 { &self.$($path).+.pretty_name                   }
            fn version(&self)            -> &str                                 { &self.$($path).+.version                       }
            fn pretty_version(&self)     -> &str                                 { &self.$($path).+.pretty_version                }
            fn package_type(&self)       -> &str                                 { &self.$($path).+.package_type                  }
            fn target_dir(&self)         -> Option<&str>                         {  self.$($path).+.target_dir.as_deref()         }
            fn source_type(&self)        -> Option<&str>                         {  self.$($path).+.source_type.as_deref()        }
            fn source_url(&self)         -> Option<&str>                         {  self.$($path).+.source_url.as_deref()         }
            fn source_reference(&self)   -> Option<&str>                         {  self.$($path).+.source_reference.as_deref()   }
            fn dist_type(&self)          -> Option<&str>                         {  self.$($path).+.dist_type.as_deref()          }
            fn dist_url(&self)           -> Option<&str>                         {  self.$($path).+.dist_url.as_deref()           }
            fn dist_reference(&self)     -> Option<&str>                         {  self.$($path).+.dist_reference.as_deref()     }
            fn dist_sha1_checksum(&self) -> Option<&str>                         {  self.$($path).+.dist_sha1_checksum.as_deref() }
            fn release_date(&self)       -> Option<&str>                         {  self.$($path).+.release_date.as_deref()       }
            fn extra(&self)              -> &BTreeMap<String, serde_json::Value> { &self.$($path).+.extra                         }
            fn binaries(&self)           -> &[String]                            { &self.$($path).+.binaries                      }
            fn is_dev(&self)             -> bool                                 {  self.$($path).+.dev                           }
            fn stability(&self)          -> Stability                            {  self.$($path).+.stability                     }
            fn notification_url(&self)   -> Option<&str>                         {  self.$($path).+.notification_url.as_deref()   }
            fn requires(&self)           -> &BTreeMap<String, Link>              { &self.$($path).+.requires                      }
            fn conflicts(&self)          -> &BTreeMap<String, Link>              { &self.$($path).+.conflicts                     }
            fn provides(&self)           -> &BTreeMap<String, Link>              { &self.$($path).+.provides                      }
            fn replaces(&self)           -> &BTreeMap<String, Link>              { &self.$($path).+.replaces                      }
            fn dev_requires(&self)       -> &BTreeMap<String, Link>              { &self.$($path).+.dev_requires                  }
            fn suggests(&self)           -> &BTreeMap<String, String>            { &self.$($path).+.suggests                      }
            fn autoload(&self)           -> &AutoloadRules                       { &self.$($path).+.autoload                      }
            fn dev_autoload(&self)       -> &AutoloadRules                       { &self.$($path).+.dev_autoload                  }
            fn is_default_branch(&self)  -> bool                                 {  self.$($path).+.is_default_branch             }
        }
    };
}

/// Implements `CompletePackage` trait by delegating to an inner `CompletePackageData` field.
macro_rules! delegate_complete_package {
    ($type:ty => $($path:ident).+) => {
        impl CompletePackage for $type {
            fn description(&self)      -> Option<&str>                   {  self.$($path).+.description.as_deref()  }
            fn homepage(&self)         -> Option<&str>                   {  self.$($path).+.homepage.as_deref()     }
            fn license(&self)          -> &[String]                      { &self.$($path).+.license                 }
            fn keywords(&self)         -> &[String]                      { &self.$($path).+.keywords                }
            fn authors(&self)          -> &[Author]                      { &self.$($path).+.authors                 }
            fn scripts(&self)          -> &BTreeMap<String, Vec<String>> { &self.$($path).+.scripts                 }
            fn support(&self)          -> &Support                       { &self.$($path).+.support                 }
            fn funding(&self)          -> &[Funding]                     { &self.$($path).+.funding                 }
            fn repositories(&self)     -> &[serde_json::Value]           { &self.$($path).+.repositories            }
            fn abandoned(&self)        -> Option<&str>                   {  self.$($path).+.abandoned.as_deref()    }
            fn archive_name(&self)     -> Option<&str>                   {  self.$($path).+.archive_name.as_deref() }
            fn archive_excludes(&self) -> &[String]                      { &self.$($path).+.archive_excludes        }
        }
    };
}

impl Package for PackageData {
    fn name(&self) -> &str {
        &self.name
    }
    fn pretty_name(&self) -> &str {
        &self.pretty_name
    }
    fn version(&self) -> &str {
        &self.version
    }
    fn pretty_version(&self) -> &str {
        &self.pretty_version
    }
    fn package_type(&self) -> &str {
        &self.package_type
    }
    fn target_dir(&self) -> Option<&str> {
        self.target_dir.as_deref()
    }
    fn source_type(&self) -> Option<&str> {
        self.source_type.as_deref()
    }
    fn source_url(&self) -> Option<&str> {
        self.source_url.as_deref()
    }
    fn source_reference(&self) -> Option<&str> {
        self.source_reference.as_deref()
    }
    fn dist_type(&self) -> Option<&str> {
        self.dist_type.as_deref()
    }
    fn dist_url(&self) -> Option<&str> {
        self.dist_url.as_deref()
    }
    fn dist_reference(&self) -> Option<&str> {
        self.dist_reference.as_deref()
    }
    fn dist_sha1_checksum(&self) -> Option<&str> {
        self.dist_sha1_checksum.as_deref()
    }
    fn release_date(&self) -> Option<&str> {
        self.release_date.as_deref()
    }
    fn extra(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.extra
    }
    fn binaries(&self) -> &[String] {
        &self.binaries
    }
    fn is_dev(&self) -> bool {
        self.dev
    }
    fn stability(&self) -> Stability {
        self.stability
    }
    fn notification_url(&self) -> Option<&str> {
        self.notification_url.as_deref()
    }
    fn requires(&self) -> &BTreeMap<String, Link> {
        &self.requires
    }
    fn conflicts(&self) -> &BTreeMap<String, Link> {
        &self.conflicts
    }
    fn provides(&self) -> &BTreeMap<String, Link> {
        &self.provides
    }
    fn replaces(&self) -> &BTreeMap<String, Link> {
        &self.replaces
    }
    fn dev_requires(&self) -> &BTreeMap<String, Link> {
        &self.dev_requires
    }
    fn suggests(&self) -> &BTreeMap<String, String> {
        &self.suggests
    }
    fn autoload(&self) -> &AutoloadRules {
        &self.autoload
    }
    fn dev_autoload(&self) -> &AutoloadRules {
        &self.dev_autoload
    }
    fn is_default_branch(&self) -> bool {
        self.is_default_branch
    }
}

impl CompletePackage for CompletePackageData {
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    fn homepage(&self) -> Option<&str> {
        self.homepage.as_deref()
    }
    fn license(&self) -> &[String] {
        &self.license
    }
    fn keywords(&self) -> &[String] {
        &self.keywords
    }
    fn authors(&self) -> &[Author] {
        &self.authors
    }
    fn scripts(&self) -> &BTreeMap<String, Vec<String>> {
        &self.scripts
    }
    fn support(&self) -> &Support {
        &self.support
    }
    fn funding(&self) -> &[Funding] {
        &self.funding
    }
    fn repositories(&self) -> &[serde_json::Value] {
        &self.repositories
    }
    fn abandoned(&self) -> Option<&str> {
        self.abandoned.as_deref()
    }
    fn archive_name(&self) -> Option<&str> {
        self.archive_name.as_deref()
    }
    fn archive_excludes(&self) -> &[String] {
        &self.archive_excludes
    }
}

impl RootPackage for RootPackageData {
    fn minimum_stability(&self) -> Stability {
        self.minimum_stability
    }
    fn prefer_stable(&self) -> bool {
        self.prefer_stable
    }
    fn stability_flags(&self) -> &BTreeMap<String, Stability> {
        &self.stability_flags
    }
    fn config(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.config
    }
    fn references(&self) -> &BTreeMap<String, String> {
        &self.references
    }
    fn aliases(&self) -> &[VersionAlias] {
        &self.aliases
    }
}

// CompletePackageData delegates Package → inner PackageData
delegate_package!(CompletePackageData => package);

// RootPackageData delegates Package → inner CompletePackageData → PackageData
delegate_package!(RootPackageData => complete.package);

// RootPackageData delegates CompletePackage → inner CompletePackageData
delegate_complete_package!(RootPackageData => complete);

/// Unstructured representation of a composer.json file.
/// Used by `init` and `create-project` to write a new composer.json.
/// Unlike the typed hierarchy above, all fields live at a single level
/// and map directly to the JSON keys via serde.
#[derive(Debug, Clone, Serialize)]
pub struct RawPackageData {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub package_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<RawAuthor>,

    #[serde(rename = "minimum-stability", skip_serializing_if = "Option::is_none")]
    pub minimum_stability: Option<String>,

    pub require: BTreeMap<String, String>,

    #[serde(rename = "require-dev", skip_serializing_if = "BTreeMap::is_empty")]
    pub require_dev: BTreeMap<String, String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<RawRepository>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoload: Option<RawAutoload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RawAuthor {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RawAutoload {
    #[serde(rename = "psr-4")]
    pub psr4: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RawRepository {
    #[serde(rename = "type")]
    pub repo_type: String,
    pub url: String,
}

impl RawPackageData {
    pub fn new(name: String) -> Self {
        Self {
            name,
            description: None,
            package_type: None,
            homepage: None,
            license: None,
            authors: Vec::new(),
            minimum_stability: None,
            require: BTreeMap::new(),
            require_dev: BTreeMap::new(),
            repositories: Vec::new(),
            autoload: None,
        }
    }
}

pub fn to_json_pretty(value: &impl Serialize) -> serde_json::Result<String> {
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser)?;
    let mut json = String::from_utf8(buf).expect("serde_json produces valid UTF-8");
    json.push('\n');
    Ok(json)
}

pub fn write_to_file(value: &impl Serialize, path: &Path) -> anyhow::Result<()> {
    let json = to_json_pretty(value)?;
    fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_minimal_json() {
        let raw = RawPackageData::new("test/pkg".to_string());
        let json = to_json_pretty(&raw).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["name"], "test/pkg");
        assert!(parsed["require"].is_object());
        assert!(parsed.get("description").is_none());
        assert!(parsed.get("type").is_none());
        assert!(parsed.get("authors").is_none());
        assert!(parsed.get("require-dev").is_none());
        assert!(parsed.get("autoload").is_none());
    }

    #[test]
    fn raw_full_json() {
        let mut raw = RawPackageData::new("acme/full".to_string());
        raw.description = Some("A full package".to_string());
        raw.package_type = Some("library".to_string());
        raw.homepage = Some("https://example.com".to_string());
        raw.license = Some("MIT".to_string());
        raw.authors = vec![RawAuthor {
            name: "Jane Doe".to_string(),
            email: Some("jane@example.com".to_string()),
        }];
        raw.minimum_stability = Some("dev".to_string());
        raw.require.insert("php".to_string(), ">=8.1".to_string());
        raw.require_dev
            .insert("phpunit/phpunit".to_string(), "^10.0".to_string());
        raw.repositories = vec![RawRepository {
            repo_type: "vcs".to_string(),
            url: "https://github.com/acme/repo".to_string(),
        }];

        let mut psr4 = BTreeMap::new();
        psr4.insert("Acme\\Full\\".to_string(), "src/".to_string());
        raw.autoload = Some(RawAutoload { psr4 });

        let json = to_json_pretty(&raw).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["name"], "acme/full");
        assert_eq!(parsed["description"], "A full package");
        assert_eq!(parsed["type"], "library");
        assert_eq!(parsed["homepage"], "https://example.com");
        assert_eq!(parsed["license"], "MIT");
        assert_eq!(parsed["minimum-stability"], "dev");
        assert_eq!(parsed["authors"][0]["name"], "Jane Doe");
        assert_eq!(parsed["authors"][0]["email"], "jane@example.com");
        assert_eq!(parsed["require"]["php"], ">=8.1");
        assert_eq!(parsed["require-dev"]["phpunit/phpunit"], "^10.0");
        assert_eq!(parsed["repositories"][0]["type"], "vcs");
        assert_eq!(parsed["autoload"]["psr-4"]["Acme\\Full\\"], "src/");
    }

    #[test]
    fn raw_none_fields_omitted() {
        let raw = RawPackageData::new("test/empty".to_string());
        let json = to_json_pretty(&raw).unwrap();

        assert!(!json.contains("\"description\""));
        assert!(!json.contains("\"type\""));
        assert!(!json.contains("\"homepage\""));
        assert!(!json.contains("\"license\""));
        assert!(!json.contains("\"authors\""));
        assert!(!json.contains("\"minimum-stability\""));
        assert!(!json.contains("\"require-dev\""));
        assert!(!json.contains("\"repositories\""));
        assert!(!json.contains("\"autoload\""));
    }
}
