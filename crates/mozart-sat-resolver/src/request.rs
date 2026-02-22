use crate::pool::PackageId;
use std::collections::HashMap;

/// A requirement: package name + version constraint string.
#[derive(Debug, Clone)]
pub struct Require {
    pub package_name: String,
    pub constraint: Option<String>,
}

/// A request for the solver: what to install/fix/lock.
///
/// Port of Composer's Request.php.
#[derive(Debug, Clone)]
pub struct Request {
    /// Root requirements: package name → constraint string.
    pub requires: HashMap<String, Option<String>>,
    /// Fixed packages (must be installed, cannot be modified).
    pub fixed_packages: Vec<PackageId>,
    /// Locked packages (installed but can be removed if nothing requires them).
    pub locked_packages: Vec<PackageId>,
}

impl Request {
    pub fn new() -> Self {
        Request {
            requires: HashMap::new(),
            fixed_packages: Vec::new(),
            locked_packages: Vec::new(),
        }
    }

    /// Add a root requirement.
    pub fn require_name(&mut self, package_name: &str, constraint: Option<&str>) {
        self.requires.insert(
            package_name.to_lowercase(),
            constraint.map(|s| s.to_string()),
        );
    }

    /// Mark a package as fixed (must remain installed).
    pub fn fix_package(&mut self, package_id: PackageId) {
        if !self.fixed_packages.contains(&package_id) {
            self.fixed_packages.push(package_id);
        }
    }

    /// Mark a package as locked.
    pub fn lock_package(&mut self, package_id: PackageId) {
        if !self.locked_packages.contains(&package_id) {
            self.locked_packages.push(package_id);
        }
    }

    /// Check if a package is fixed.
    pub fn is_fixed(&self, package_id: PackageId) -> bool {
        self.fixed_packages.contains(&package_id)
    }
}

impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}
