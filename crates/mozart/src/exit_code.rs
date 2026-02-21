/// Exit code: success.
pub const OK: i32 = 0;

/// Exit code: general / unclassified error.
pub const GENERAL_ERROR: i32 = 1;

/// Exit code: dependency resolution failed.
pub const DEPENDENCY_RESOLUTION_FAILED: i32 = 2;

/// Exit code: partial update requested but no lock file exists.
pub const NO_LOCK_FILE_FOR_PARTIAL_UPDATE: i32 = 3;

/// Exit code: lock file is invalid or corrupt.
pub const LOCK_FILE_INVALID: i32 = 4;

/// Exit code: audit found a security advisory.
pub const AUDIT_FAILED: i32 = 5;

/// Exit code: HTTP / network transport error.
pub const TRANSPORT_ERROR: i32 = 100;

// ---------------------------------------------------------------------------
// MozartError — carries a specific exit code through anyhow's error chain
// ---------------------------------------------------------------------------

/// An error type that carries a specific exit code for Mozart to use on exit.
///
/// Use [`bail`] or [`bail_silent`] to construct one wrapped in `anyhow::Error`.
#[derive(Debug)]
pub struct MozartError {
    pub message: String,
    pub exit_code: i32,
}

impl std::fmt::Display for MozartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MozartError {}

/// Return an `anyhow::Error` that carries `exit_code` and prints `message`.
pub fn bail(exit_code: i32, message: impl Into<String>) -> anyhow::Error {
    MozartError {
        message: message.into(),
        exit_code,
    }
    .into()
}

/// Return an `anyhow::Error` that carries `exit_code` but suppresses the
/// message (caller has already printed it).
pub fn bail_silent(exit_code: i32) -> anyhow::Error {
    MozartError {
        message: String::new(),
        exit_code,
    }
    .into()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants_have_expected_values() {
        assert_eq!(OK, 0);
        assert_eq!(GENERAL_ERROR, 1);
        assert_eq!(DEPENDENCY_RESOLUTION_FAILED, 2);
        assert_eq!(NO_LOCK_FILE_FOR_PARTIAL_UPDATE, 3);
        assert_eq!(LOCK_FILE_INVALID, 4);
        assert_eq!(AUDIT_FAILED, 5);
        assert_eq!(TRANSPORT_ERROR, 100);
    }

    #[test]
    fn test_mozart_error_display() {
        let err = MozartError {
            message: "something went wrong".to_string(),
            exit_code: GENERAL_ERROR,
        };
        assert_eq!(format!("{err}"), "something went wrong");
    }

    #[test]
    fn test_bail_can_be_downcast() {
        let err = bail(DEPENDENCY_RESOLUTION_FAILED, "cannot resolve");
        let me = err.downcast_ref::<MozartError>().expect("should downcast");
        assert_eq!(me.exit_code, DEPENDENCY_RESOLUTION_FAILED);
        assert_eq!(me.message, "cannot resolve");
    }

    #[test]
    fn test_bail_silent_has_empty_message() {
        let err = bail_silent(GENERAL_ERROR);
        let me = err.downcast_ref::<MozartError>().expect("should downcast");
        assert_eq!(me.exit_code, GENERAL_ERROR);
        assert!(me.message.is_empty());
    }

    #[test]
    fn test_mozart_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(MozartError {
            message: "test".to_string(),
            exit_code: 1,
        });
        assert_eq!(err.to_string(), "test");
    }
}
