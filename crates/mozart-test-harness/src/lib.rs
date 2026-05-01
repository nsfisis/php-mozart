//! Harness for Composer's `.test` integration fixture format.
//!
//! See `composer/tests/Composer/Test/Fixtures/installer/SAMPLE` and
//! `composer/tests/Composer/Test/InstallerTest.php` for the reference
//! implementation. This crate provides the parser and a binary-invoking
//! runner; actual `.test` fixtures and tests live elsewhere.

mod parser;
mod runner;

pub use parser::{ParsedTest, parse_test_file, parse_test_str};
pub use runner::{RunResult, run_test};
