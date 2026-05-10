use crate::package::{Author, Funding, PackageInterface, Support};

/// ref: \Composer\Package\CompletePackageInterface
pub trait CompletePackageInterface: PackageInterface {
    fn description(&self) -> Option<&str>;
    fn homepage(&self) -> Option<&str>;
    fn license(&self) -> &[String];
    fn keywords(&self) -> &[String];
    fn authors(&self) -> &[Author];
    fn scripts(&self) -> &indexmap::IndexMap<String, Vec<String>>;
    fn support(&self) -> &Support;
    fn funding(&self) -> &[Funding];
    fn repositories(&self) -> &[serde_json::Value];
    fn abandoned(&self) -> Option<&str>;
    fn archive_name(&self) -> Option<&str>;
    fn archive_excludes(&self) -> &[String];
}
