use colored::{ColoredString, Colorize};
use dialoguer::{Confirm, Input};

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

pub struct Console {
    pub interactive: bool,
    pub quiet: bool,
}

impl Console {
    pub fn new(no_interaction: bool, quiet: bool) -> Self {
        Self {
            interactive: !no_interaction,
            quiet,
        }
    }

    pub fn info(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }

    pub fn error(&self, msg: &str) {
        eprintln!("{}", console::error(msg));
    }

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
                    self.error(&e);
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
