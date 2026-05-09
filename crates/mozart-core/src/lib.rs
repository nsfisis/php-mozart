extern crate self as mozart_core;

pub mod advisory;
pub mod autoload;
pub mod composer;
pub mod config;
pub mod config_source;
pub mod config_validator;
pub mod console;
pub mod dependency_resolver;
pub mod exit_code;
pub mod factory;
pub mod http;
pub mod installer;
pub mod package;
pub mod package_info;
pub mod package_sorter;
pub mod platform;
pub mod repository;
pub mod repository_utils;
pub mod script_events;
pub mod suggest;
pub mod validation;
pub mod vcs;
pub mod version_bumper;
pub mod wildcard;

pub use mozart_console_macros::console_format;
pub use wildcard::matches_wildcard;

pub const MOZART_VERSION: &str = env!("CARGO_PKG_VERSION");
