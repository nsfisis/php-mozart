use crate::composer::{InstallationSource, LocalPackage};

/// Mirrors `Composer\Package\Dumper\ArrayDumper`. Serialises a `LocalPackage`
/// into the JSON shape that `VersionGuesser::guess_version` expects.
#[derive(Default)]
pub struct ArrayDumper;

impl ArrayDumper {
    pub fn new() -> Self {
        Self
    }

    pub fn dump(&self, package: &LocalPackage) -> serde_json::Value {
        build_package_config(package)
    }
}

/// Serialises a `LocalPackage` to the JSON shape consumed by
/// `VersionGuesser::guess_version`. Mirrors `ArrayDumper::dump($package)` —
/// we include all fields that `VersionGuesser` inspects.
fn build_package_config(package: &LocalPackage) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), package.pretty_name().into());
    obj.insert("version".into(), package.pretty_version().into());
    if let Some(t) = package.package_type() {
        obj.insert("type".into(), t.into());
    }
    obj.insert("extra".into(), package.extra().clone());
    if let Some(src) = package.source() {
        let mut s = serde_json::Map::new();
        s.insert("type".into(), src.kind.clone().into());
        s.insert("url".into(), src.url.clone().into());
        if let Some(r) = &src.reference {
            s.insert("reference".into(), r.clone().into());
        }
        obj.insert("source".into(), serde_json::Value::Object(s));
    }
    if let Some(dist) = package.dist() {
        let mut d = serde_json::Map::new();
        d.insert("type".into(), dist.kind.clone().into());
        d.insert("url".into(), dist.url.clone().into());
        if let Some(r) = &dist.reference {
            d.insert("reference".into(), r.clone().into());
        }
        obj.insert("dist".into(), serde_json::Value::Object(d));
    }
    if let Some(is) = package.installation_source() {
        let s = match is {
            InstallationSource::Source => "source",
            InstallationSource::Dist => "dist",
        };
        obj.insert("installation-source".into(), s.into());
    }
    serde_json::Value::Object(obj)
}
