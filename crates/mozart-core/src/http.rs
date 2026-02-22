/// Returns the common User-Agent string for all HTTP requests.
///
/// Format: `Mozart/<version> (<os>; <arch>)`
pub fn user_agent() -> String {
    format!(
        "Mozart/{} ({}; {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}
