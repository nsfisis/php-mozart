pub mod decisions;
pub mod error;
pub mod policy;
pub mod pool;
pub mod pool_builder;
pub mod problem;
pub mod request;
pub mod rule;
pub mod rule_set;
pub mod rule_set_generator;
pub mod rule_watch_graph;
pub mod solver;
pub mod transaction;

// Re-export key types for public API
pub use error::SolverError;
pub use policy::DefaultPolicy;
pub use pool::{Literal, PackageId, Pool, PoolLink, PoolPackage, PoolPackageInput};
pub use pool_builder::{PoolBuilder, make_pool_links};
pub use request::Request;
pub use rule::{ReasonData, Rule, RuleReason};
pub use rule_set::RuleSet;
pub use rule_set_generator::RuleSetGenerator;
pub use solver::{Solver, SolverResult};
pub use transaction::{LockTransaction, Operation, Transaction};
