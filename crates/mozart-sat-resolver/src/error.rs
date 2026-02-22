use std::fmt;

/// A bug in the solver itself (should never happen in normal operation).
/// Equivalent to Composer's SolverBugException.
#[derive(Debug, Clone)]
pub struct SolverBugError {
    pub message: String,
}

impl fmt::Display for SolverBugError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Solver bug: {}", self.message)
    }
}

impl std::error::Error for SolverBugError {}

/// Errors produced by the SAT solver.
#[derive(Debug)]
pub enum SolverError {
    /// Internal solver bug (should never happen).
    Bug(SolverBugError),
    /// The dependency set is unsolvable. Contains problem descriptions.
    Unsolvable(Vec<String>),
}

impl fmt::Display for SolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolverError::Bug(e) => write!(f, "{e}"),
            SolverError::Unsolvable(problems) => {
                for (i, problem) in problems.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "  Problem {}: {problem}", i + 1)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SolverError {}

impl From<SolverBugError> for SolverError {
    fn from(e: SolverBugError) -> Self {
        SolverError::Bug(e)
    }
}
