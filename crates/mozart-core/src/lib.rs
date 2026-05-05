pub mod composer;
pub mod console;
pub mod exit_code;
pub mod http;
pub mod package;
pub mod platform;
pub mod suggest;
pub mod validation;
pub mod version_bumper;
pub mod wildcard;

pub use mozart_console_macros::console_format;
pub use wildcard::matches_wildcard;

pub const MOZART_VERSION: &str = env!("CARGO_PKG_VERSION");
