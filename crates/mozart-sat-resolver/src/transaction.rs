use crate::decisions::Decisions;
use crate::pool::{PackageId, Pool, literal_to_package_id};
use std::collections::{HashMap, HashSet};

/// An operation to perform on a package.
///
/// Port of Composer's SolverOperation hierarchy.
#[derive(Debug, Clone)]
pub enum Operation {
    /// Install a new package.
    Install { package_id: PackageId },
    /// Update a package from one version to another.
    Update {
        initial_id: PackageId,
        target_id: PackageId,
    },
    /// Remove a package.
    Uninstall { package_id: PackageId },
}

impl Operation {
    /// Get the operation type as a string.
    pub fn operation_type(&self) -> &'static str {
        match self {
            Operation::Install { .. } => "install",
            Operation::Update { .. } => "update",
            Operation::Uninstall { .. } => "uninstall",
        }
    }

    /// Format the operation as a human-readable string using pool data.
    pub fn pretty_string(&self, pool: &Pool) -> String {
        match self {
            Operation::Install { package_id } => {
                let pkg = pool.package_by_id(*package_id);
                format!("Installing {} ({})", pkg.name, pkg.pretty_version)
            }
            Operation::Update {
                initial_id,
                target_id,
            } => {
                let initial = pool.package_by_id(*initial_id);
                let target = pool.package_by_id(*target_id);
                format!(
                    "Updating {} ({} => {})",
                    target.name, initial.pretty_version, target.pretty_version
                )
            }
            Operation::Uninstall { package_id } => {
                let pkg = pool.package_by_id(*package_id);
                format!("Removing {} ({})", pkg.name, pkg.pretty_version)
            }
        }
    }
}

/// Computes install/update/remove operations from solver results.
///
/// Port of Composer's Transaction.php.
pub struct Transaction<'a> {
    pool: &'a Pool,
    /// Currently installed package IDs.
    present_ids: Vec<PackageId>,
    /// Result package IDs from the solver.
    result_ids: Vec<PackageId>,
    /// Computed operations.
    operations: Vec<Operation>,
}

impl<'a> Transaction<'a> {
    /// Create a new transaction from present and result package sets.
    pub fn new(pool: &'a Pool, present_ids: Vec<PackageId>, result_ids: Vec<PackageId>) -> Self {
        let mut tx = Transaction {
            pool,
            present_ids,
            result_ids,
            operations: Vec::new(),
        };
        tx.calculate_operations();
        tx
    }

    /// Create a transaction from solver decisions.
    pub fn from_decisions(
        pool: &'a Pool,
        present_ids: Vec<PackageId>,
        decisions: &Decisions,
    ) -> Self {
        let mut result_ids = Vec::new();
        for i in 0..decisions.len() {
            let decision = decisions.at_offset(i);
            if decision.literal > 0 {
                result_ids.push(literal_to_package_id(decision.literal));
            }
        }
        Self::new(pool, present_ids, result_ids)
    }

    /// Get the computed operations.
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Calculate the delta between present and result packages.
    fn calculate_operations(&mut self) {
        // Build maps: name -> package_id for present packages
        let mut present_by_name: HashMap<&str, PackageId> = HashMap::new();
        for &id in &self.present_ids {
            let pkg = self.pool.package_by_id(id);
            present_by_name.insert(&pkg.name, id);
        }

        // Track which present packages have been matched
        let mut matched_present: HashSet<PackageId> = HashSet::new();

        // Build topologically sorted result packages via DFS
        let sorted_results = self.topological_sort();

        // Process result packages in topological order
        for &result_id in &sorted_results {
            let result_pkg = self.pool.package_by_id(result_id);

            if let Some(&present_id) = present_by_name.get(result_pkg.name.as_str()) {
                matched_present.insert(present_id);
                let present_pkg = self.pool.package_by_id(present_id);

                // Check if update is needed (version changed)
                if present_pkg.version != result_pkg.version || present_id != result_id {
                    self.operations.push(Operation::Update {
                        initial_id: present_id,
                        target_id: result_id,
                    });
                }
                // Otherwise: no change needed, skip
            } else {
                // New package: install
                self.operations.push(Operation::Install {
                    package_id: result_id,
                });
            }
        }

        // Remove packages that are present but not in result
        let mut uninstalls = Vec::new();
        for &present_id in &self.present_ids {
            if !matched_present.contains(&present_id) {
                uninstalls.push(Operation::Uninstall {
                    package_id: present_id,
                });
            }
        }

        // Prepend uninstalls (remove before install/update)
        uninstalls.append(&mut self.operations);
        self.operations = uninstalls;
    }

    /// Topologically sort result packages by their dependency order.
    /// Uses DFS: dependencies are processed before dependents.
    fn topological_sort(&self) -> Vec<PackageId> {
        let result_set: HashSet<PackageId> = self.result_ids.iter().copied().collect();
        let result_by_name: HashMap<&str, Vec<PackageId>> = {
            let mut map: HashMap<&str, Vec<PackageId>> = HashMap::new();
            for &id in &self.result_ids {
                let pkg = self.pool.package_by_id(id);
                map.entry(&pkg.name).or_default().push(id);
            }
            map
        };

        let mut visited: HashSet<PackageId> = HashSet::new();
        let mut order: Vec<PackageId> = Vec::new();

        // Find root packages (not required by any other result package)
        let roots = self.get_root_packages(&result_set, &result_by_name);

        // DFS from roots
        let mut stack: Vec<(PackageId, bool)> = Vec::new();
        for &root_id in roots.iter().rev() {
            stack.push((root_id, false));
        }

        while let Some((pkg_id, processed)) = stack.pop() {
            if processed {
                if visited.insert(pkg_id) {
                    order.push(pkg_id);
                }
                continue;
            }

            if visited.contains(&pkg_id) {
                continue;
            }

            // Push self as "processed" marker
            stack.push((pkg_id, true));

            // Push dependencies
            let pkg = self.pool.package_by_id(pkg_id);
            for req in &pkg.requires {
                if let Some(provider_ids) = result_by_name.get(req.target.as_str()) {
                    for &dep_id in provider_ids {
                        if !visited.contains(&dep_id) {
                            stack.push((dep_id, false));
                        }
                    }
                }
            }
        }

        // Add any remaining unvisited packages
        for &id in &self.result_ids {
            if !visited.contains(&id) {
                order.push(id);
            }
        }

        order
    }

    /// Find root packages: result packages not required by any other result package.
    fn get_root_packages(
        &self,
        result_set: &HashSet<PackageId>,
        _result_by_name: &HashMap<&str, Vec<PackageId>>,
    ) -> Vec<PackageId> {
        // Collect all required package names
        let mut required_names: HashSet<&str> = HashSet::new();
        for &id in result_set {
            let pkg = self.pool.package_by_id(id);
            for req in &pkg.requires {
                required_names.insert(&req.target);
            }
        }

        // Root packages are those whose name is NOT in required_names
        let mut roots: Vec<PackageId> = Vec::new();
        for &id in &self.result_ids {
            let pkg = self.pool.package_by_id(id);
            if !required_names.contains(pkg.name.as_str()) {
                roots.push(id);
            }
        }

        // If no roots found (circular), use all
        if roots.is_empty() {
            return self.result_ids.clone();
        }

        roots
    }
}

/// Lock transaction: specialization for computing lock file operations.
///
/// Port of Composer's LockTransaction.php.
pub struct LockTransaction<'a> {
    /// The base transaction.
    transaction: Transaction<'a>,
    /// All result package IDs.
    all_result_ids: Vec<PackageId>,
    /// Non-dev result package IDs.
    non_dev_ids: Vec<PackageId>,
    /// Dev result package IDs.
    dev_ids: Vec<PackageId>,
}

impl<'a> LockTransaction<'a> {
    /// Create a lock transaction from solver decisions.
    pub fn new(
        pool: &'a Pool,
        present_ids: Vec<PackageId>,
        unlockable_ids: HashSet<PackageId>,
        decisions: &Decisions,
    ) -> Self {
        // Extract result packages from decisions
        let mut all_result_ids = Vec::new();
        let mut non_dev_ids = Vec::new();
        for i in 0..decisions.len() {
            let decision = decisions.at_offset(i);
            if decision.literal > 0 {
                let pkg_id = literal_to_package_id(decision.literal);
                all_result_ids.push(pkg_id);
                if !unlockable_ids.contains(&pkg_id) {
                    non_dev_ids.push(pkg_id);
                }
            }
        }

        let transaction = Transaction::new(pool, present_ids, all_result_ids.clone());

        LockTransaction {
            transaction,
            all_result_ids,
            non_dev_ids,
            dev_ids: Vec::new(),
        }
    }

    /// Set the non-dev packages from an extraction-only solve result.
    /// `extraction_ids` are the package IDs that were resolved without dev deps.
    pub fn set_non_dev_packages(&mut self, extraction_ids: &[PackageId]) {
        let extraction_names: HashSet<String> = extraction_ids
            .iter()
            .map(|&id| self.transaction.pool.package_by_id(id).name.clone())
            .collect();

        self.non_dev_ids.clear();
        self.dev_ids.clear();

        for &id in &self.all_result_ids {
            let pkg = self.transaction.pool.package_by_id(id);
            if extraction_names.contains(&pkg.name) {
                self.non_dev_ids.push(id);
            } else {
                self.dev_ids.push(id);
            }
        }
    }

    /// Get the computed operations.
    pub fn operations(&self) -> &[Operation] {
        self.transaction.operations()
    }

    /// Get all result package IDs.
    pub fn all_result_ids(&self) -> &[PackageId] {
        &self.all_result_ids
    }

    /// Get non-dev result package IDs.
    pub fn non_dev_ids(&self) -> &[PackageId] {
        &self.non_dev_ids
    }

    /// Get dev result package IDs.
    pub fn dev_ids(&self) -> &[PackageId] {
        &self.dev_ids
    }

    /// Get new lock packages for writing to the lock file.
    /// If `dev_mode` is true, returns dev packages; otherwise non-dev.
    pub fn new_lock_package_ids(&self, dev_mode: bool) -> &[PackageId] {
        if dev_mode {
            &self.dev_ids
        } else {
            &self.non_dev_ids
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{PoolLink, PoolPackageInput};

    fn make_input(name: &str, version: &str, pretty: &str) -> PoolPackageInput {
        PoolPackageInput {
            name: name.to_string(),
            version: version.to_string(),
            pretty_version: pretty.to_string(),
            requires: vec![],
            replaces: vec![],
            provides: vec![],
            conflicts: vec![],
            is_fixed: false,
        }
    }

    fn make_input_with_deps(
        name: &str,
        version: &str,
        pretty: &str,
        deps: Vec<(&str, &str)>,
    ) -> PoolPackageInput {
        let requires = deps
            .into_iter()
            .map(|(target, constraint)| PoolLink {
                target: target.to_string(),
                constraint: constraint.to_string(),
                source: name.to_string(),
            })
            .collect();

        PoolPackageInput {
            name: name.to_string(),
            version: version.to_string(),
            pretty_version: pretty.to_string(),
            requires,
            replaces: vec![],
            provides: vec![],
            conflicts: vec![],
            is_fixed: false,
        }
    }

    #[test]
    fn test_fresh_install() {
        let pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0", "1.0.0"),
                make_input("b/b", "2.0.0.0", "2.0.0"),
            ],
            vec![],
        );

        let tx = Transaction::new(&pool, vec![], vec![1, 2]);
        let ops = tx.operations();

        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], Operation::Install { package_id: _ }));
        assert!(matches!(ops[1], Operation::Install { package_id: _ }));
    }

    #[test]
    fn test_update_package() {
        let pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0", "1.0.0"),
                make_input("a/a", "2.0.0.0", "2.0.0"),
            ],
            vec![],
        );

        // Present: a/a 1.0.0 (id=1), Result: a/a 2.0.0 (id=2)
        let tx = Transaction::new(&pool, vec![1], vec![2]);
        let ops = tx.operations();

        assert_eq!(ops.len(), 1);
        match &ops[0] {
            Operation::Update {
                initial_id,
                target_id,
            } => {
                assert_eq!(*initial_id, 1);
                assert_eq!(*target_id, 2);
            }
            _ => panic!("Expected update operation"),
        }
    }

    #[test]
    fn test_uninstall_package() {
        let pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0", "1.0.0"),
                make_input("b/b", "1.0.0.0", "1.0.0"),
            ],
            vec![],
        );

        // Present: a/a and b/b, Result: only a/a
        let tx = Transaction::new(&pool, vec![1, 2], vec![1]);
        let ops = tx.operations();

        assert_eq!(ops.len(), 1);
        match &ops[0] {
            Operation::Uninstall { package_id } => {
                assert_eq!(*package_id, 2);
            }
            _ => panic!("Expected uninstall operation"),
        }
    }

    #[test]
    fn test_uninstalls_before_installs() {
        let pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0", "1.0.0"),
                make_input("b/b", "1.0.0.0", "1.0.0"),
            ],
            vec![],
        );

        // Present: a/a, Result: b/b (uninstall a, install b)
        let tx = Transaction::new(&pool, vec![1], vec![2]);
        let ops = tx.operations();

        assert_eq!(ops.len(), 2);
        assert!(
            matches!(ops[0], Operation::Uninstall { .. }),
            "Uninstalls should come first"
        );
        assert!(
            matches!(ops[1], Operation::Install { .. }),
            "Installs should come after"
        );
    }

    #[test]
    fn test_dependency_ordering() {
        // a/a requires b/b — b/b should be installed before a/a
        let pool = Pool::new(
            vec![
                make_input_with_deps("a/a", "1.0.0.0", "1.0.0", vec![("b/b", "^1.0")]),
                make_input("b/b", "1.0.0.0", "1.0.0"),
            ],
            vec![],
        );

        let tx = Transaction::new(&pool, vec![], vec![1, 2]);
        let ops = tx.operations();

        assert_eq!(ops.len(), 2);
        // b/b (dependency) should be installed before a/a
        match (&ops[0], &ops[1]) {
            (
                Operation::Install { package_id: first },
                Operation::Install { package_id: second },
            ) => {
                assert_eq!(*first, 2, "b/b should be installed first");
                assert_eq!(*second, 1, "a/a should be installed second");
            }
            _ => panic!("Expected two install operations"),
        }
    }

    #[test]
    fn test_no_change() {
        let pool = Pool::new(vec![make_input("a/a", "1.0.0.0", "1.0.0")], vec![]);

        // Same package present and in result
        let tx = Transaction::new(&pool, vec![1], vec![1]);
        let ops = tx.operations();

        assert!(ops.is_empty(), "No operations when nothing changed");
    }

    #[test]
    fn test_operation_pretty_string() {
        let pool = Pool::new(
            vec![
                make_input("a/a", "1.0.0.0", "1.0.0"),
                make_input("a/a", "2.0.0.0", "2.0.0"),
            ],
            vec![],
        );

        let install = Operation::Install { package_id: 1 };
        assert_eq!(install.pretty_string(&pool), "Installing a/a (1.0.0)");

        let update = Operation::Update {
            initial_id: 1,
            target_id: 2,
        };
        assert_eq!(update.pretty_string(&pool), "Updating a/a (1.0.0 => 2.0.0)");

        let uninstall = Operation::Uninstall { package_id: 1 };
        assert_eq!(uninstall.pretty_string(&pool), "Removing a/a (1.0.0)");
    }
}
