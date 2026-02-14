use colored::{ColoredString, Colorize};

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
