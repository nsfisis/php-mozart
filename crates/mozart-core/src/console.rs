use colored::{ColoredString, Colorize};
use dialoguer::{Confirm, Input};
use std::io::IsTerminal;

// ---------------------------------------------------------------------------
// Tag-style color helpers (module-level free functions, unchanged API)
// ---------------------------------------------------------------------------

/// `<info>` — green foreground
pub fn info(message: &str) -> ColoredString {
    message.green()
}

/// `<comment>` — yellow foreground
pub fn comment(message: &str) -> ColoredString {
    message.yellow()
}

/// `<error>` — white on red
pub fn error(message: &str) -> ColoredString {
    message.white().on_red()
}

/// `<question>` — black on cyan
pub fn question(message: &str) -> ColoredString {
    message.black().on_cyan()
}

/// `<highlight>` — red foreground (Composer extension)
pub fn highlight(message: &str) -> ColoredString {
    message.red()
}

/// `<warning>` — black on yellow (Composer extension)
pub fn warning(message: &str) -> ColoredString {
    message.black().on_yellow()
}

// ---------------------------------------------------------------------------
// Verbosity
// ---------------------------------------------------------------------------

/// Output verbosity level, ordered from least to most verbose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// `-q` / `--quiet`: suppress all non-error output.
    Quiet,
    /// Default: normal informational messages.
    Normal,
    /// `-v`: additional detail (URLs, cache hits, skips).
    Verbose,
    /// `-vv`: HTTP details, file operations, resolver iterations.
    VeryVerbose,
    /// `-vvv`: full debug output (headers, raw payloads, timing).
    Debug,
}

impl Verbosity {
    /// Construct a `Verbosity` from CLI flag counts.
    ///
    /// - `quiet == true` → `Quiet` (takes priority over `-v` flags)
    /// - `verbose_count == 0` → `Normal`
    /// - `verbose_count == 1` → `Verbose`
    /// - `verbose_count == 2` → `VeryVerbose`
    /// - `verbose_count >= 3` → `Debug`
    pub fn from_flags(verbose_count: u8, quiet: bool) -> Self {
        if quiet {
            return Verbosity::Quiet;
        }
        match verbose_count {
            0 => Verbosity::Normal,
            1 => Verbosity::Verbose,
            2 => Verbosity::VeryVerbose,
            _ => Verbosity::Debug,
        }
    }
}

// ---------------------------------------------------------------------------
// Console
// ---------------------------------------------------------------------------

/// Central IO hub for Mozart commands.
///
/// Constructed once in `commands::execute()` and passed as `&Console` to every
/// command and library function that needs to produce output.
pub struct Console {
    /// Whether the user can answer interactive prompts.
    pub interactive: bool,
    /// Current verbosity level.
    pub verbosity: Verbosity,
    /// Whether ANSI color codes should be emitted.
    pub decorated: bool,
}

impl Console {
    /// Build a `Console` from primitive arguments.
    ///
    /// This is the primary constructor. Pass the relevant CLI flag values:
    /// - `verbose`: the `-v` flag count (0, 1, 2, 3+)
    /// - `quiet`: whether `--quiet` was passed
    /// - `ansi`: whether `--ansi` was passed
    /// - `no_ansi`: whether `--no-ansi` was passed
    /// - `no_interaction`: whether `--no-interaction` / `-n` was passed
    pub fn new(verbose: u8, quiet: bool, ansi: bool, no_ansi: bool, no_interaction: bool) -> Self {
        let verbosity = Verbosity::from_flags(verbose, quiet);
        let decorated = Self::resolve_decorated(ansi, no_ansi);
        colored::control::set_override(decorated);
        Self {
            interactive: !no_interaction,
            verbosity,
            decorated,
        }
    }

    /// Determine whether ANSI color output should be enabled.
    ///
    /// - `no_ansi == true` → always disable
    /// - `ansi == true` → always enable
    /// - Otherwise → auto-detect: enabled only when stderr is a TTY
    pub fn resolve_decorated(ansi: bool, no_ansi: bool) -> bool {
        if no_ansi {
            return false;
        }
        if ansi {
            return true;
        }
        std::io::stderr().is_terminal()
    }

    // -----------------------------------------------------------------------
    // Output methods
    // -----------------------------------------------------------------------

    /// Write `msg` to stderr if `self.verbosity >= required`.
    pub fn write(&self, msg: &str, required: Verbosity) {
        if self.verbosity >= required {
            eprintln!("{msg}");
        }
    }

    /// Write `msg` to stdout if `self.verbosity >= required`.
    pub fn write_stdout(&self, msg: &str, required: Verbosity) {
        if self.verbosity >= required {
            println!("{msg}");
        }
    }

    /// Write an error to stderr. Always shown, even in quiet mode.
    pub fn write_error(&self, msg: &str) {
        eprintln!("{}", error(msg));
    }

    // Convenience verbosity-level shortcuts:

    /// Normal-level message (suppressed by `--quiet`).
    pub fn info(&self, msg: &str) {
        self.write(msg, Verbosity::Normal);
    }

    /// Verbose-level message (shown with `-v` or higher).
    pub fn verbose(&self, msg: &str) {
        self.write(msg, Verbosity::Verbose);
    }

    /// Very-verbose-level message (shown with `-vv` or higher).
    pub fn very_verbose(&self, msg: &str) {
        self.write(msg, Verbosity::VeryVerbose);
    }

    /// Debug-level message (shown with `-vvv`).
    pub fn debug(&self, msg: &str) {
        self.write(msg, Verbosity::Debug);
    }

    /// Error message — always shown.
    pub fn error(&self, msg: &str) {
        self.write_error(msg);
    }

    // -----------------------------------------------------------------------
    // Query methods
    // -----------------------------------------------------------------------

    pub fn is_verbose(&self) -> bool {
        self.verbosity >= Verbosity::Verbose
    }

    pub fn is_very_verbose(&self) -> bool {
        self.verbosity >= Verbosity::VeryVerbose
    }

    pub fn is_debug(&self) -> bool {
        self.verbosity >= Verbosity::Debug
    }

    pub fn is_quiet(&self) -> bool {
        self.verbosity == Verbosity::Quiet
    }

    // -----------------------------------------------------------------------
    // Interactive prompt methods (unchanged from prior implementation)
    // -----------------------------------------------------------------------

    pub fn ask(&self, prompt: &str, default: &str) -> String {
        if !self.interactive {
            return default.to_string();
        }

        Input::new()
            .with_prompt(prompt)
            .default(default.to_string())
            .allow_empty(true)
            .interact_text()
            .unwrap_or_else(|_| default.to_string())
    }

    pub fn ask_validated<F>(
        &self,
        prompt: &str,
        default: &str,
        validator: F,
    ) -> Result<String, String>
    where
        F: Fn(&str) -> Result<(), String>,
    {
        if !self.interactive {
            validator(default)?;
            return Ok(default.to_string());
        }

        loop {
            let input: String = Input::new()
                .with_prompt(prompt)
                .default(default.to_string())
                .allow_empty(true)
                .interact_text()
                .unwrap_or_else(|_| default.to_string());

            match validator(&input) {
                Ok(()) => return Ok(input),
                Err(e) => {
                    self.write_error(&e);
                }
            }
        }
    }

    pub fn confirm(&self, prompt: &str) -> bool {
        if !self.interactive {
            return true;
        }

        Confirm::new()
            .with_prompt(prompt)
            .default(true)
            .interact()
            .unwrap_or(true)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Verbosity::from_flags ───────────────────────────────────────────────

    #[test]
    fn test_verbosity_quiet_takes_priority() {
        assert_eq!(Verbosity::from_flags(3, true), Verbosity::Quiet);
        assert_eq!(Verbosity::from_flags(0, true), Verbosity::Quiet);
    }

    #[test]
    fn test_verbosity_normal() {
        assert_eq!(Verbosity::from_flags(0, false), Verbosity::Normal);
    }

    #[test]
    fn test_verbosity_verbose() {
        assert_eq!(Verbosity::from_flags(1, false), Verbosity::Verbose);
    }

    #[test]
    fn test_verbosity_very_verbose() {
        assert_eq!(Verbosity::from_flags(2, false), Verbosity::VeryVerbose);
    }

    #[test]
    fn test_verbosity_debug() {
        assert_eq!(Verbosity::from_flags(3, false), Verbosity::Debug);
        assert_eq!(Verbosity::from_flags(10, false), Verbosity::Debug);
    }

    // ── Verbosity ordering ──────────────────────────────────────────────────

    #[test]
    fn test_verbosity_ordering() {
        assert!(Verbosity::Quiet < Verbosity::Normal);
        assert!(Verbosity::Normal < Verbosity::Verbose);
        assert!(Verbosity::Verbose < Verbosity::VeryVerbose);
        assert!(Verbosity::VeryVerbose < Verbosity::Debug);
    }

    // ── Console::resolve_decorated ──────────────────────────────────────────

    #[test]
    fn test_resolve_decorated_no_ansi_wins() {
        assert!(!Console::resolve_decorated(true, true));
        assert!(!Console::resolve_decorated(false, true));
    }

    #[test]
    fn test_resolve_decorated_ansi_forces_on() {
        assert!(Console::resolve_decorated(true, false));
    }

    // ── Console query methods ───────────────────────────────────────────────

    fn make_console(verbosity: Verbosity) -> Console {
        Console {
            interactive: false,
            verbosity,
            decorated: false,
        }
    }

    #[test]
    fn test_is_quiet() {
        assert!(make_console(Verbosity::Quiet).is_quiet());
        assert!(!make_console(Verbosity::Normal).is_quiet());
    }

    #[test]
    fn test_is_verbose() {
        assert!(!make_console(Verbosity::Normal).is_verbose());
        assert!(make_console(Verbosity::Verbose).is_verbose());
        assert!(make_console(Verbosity::VeryVerbose).is_verbose());
        assert!(make_console(Verbosity::Debug).is_verbose());
    }

    #[test]
    fn test_is_very_verbose() {
        assert!(!make_console(Verbosity::Verbose).is_very_verbose());
        assert!(make_console(Verbosity::VeryVerbose).is_very_verbose());
        assert!(make_console(Verbosity::Debug).is_very_verbose());
    }

    #[test]
    fn test_is_debug() {
        assert!(!make_console(Verbosity::VeryVerbose).is_debug());
        assert!(make_console(Verbosity::Debug).is_debug());
    }
}
