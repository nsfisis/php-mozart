use crate::composer::Composer;
use clap::Args;
use mozart_core::console::IoInterface;
use mozart_core::script_events;
use mozart_core::{console_writeln, console_writeln_error};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Args)]
pub struct RunScriptArgs {
    /// Script name to run
    pub script: Option<String>,

    /// Arguments to pass to the script
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,

    /// Sets script timeout in seconds, or 0 for never.
    #[arg(long)]
    pub timeout: Option<String>,

    /// Sets the dev mode
    #[arg(long)]
    pub dev: bool,

    /// Disables the dev mode
    #[arg(long)]
    pub no_dev: bool,

    /// List the available scripts
    #[arg(short, long)]
    pub list: bool,
}

pub async fn execute(
    args: &RunScriptArgs,
    cli: &super::Cli,
    io: std::sync::Arc<std::sync::Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    let working_dir = cli.working_dir()?;

    if args.list {
        Composer::require(io.clone(), &working_dir)?;
        let (scripts, descriptions) = load_scripts(&working_dir)?;
        return list_scripts(&scripts, &descriptions, io.clone());
    }

    let script = match &args.script {
        Some(name) => name.clone(),
        None => anyhow::bail!("Missing required argument \"script\""),
    };

    if !script_events::USER_RUNNABLE.contains(&script.as_str())
        && script_events::ALL.contains(&script.as_str())
    {
        anyhow::bail!("Script \"{}\" cannot be run with this command", script);
    }

    let composer = Composer::require(io.clone(), &working_dir)?;
    let dev_mode = args.dev || !args.no_dev;

    let (scripts, _descriptions) = load_scripts(&working_dir)?;
    if !scripts.contains_key(&script) {
        anyhow::bail!("Script \"{}\" is not defined in this package", script);
    }

    let timeout = match &args.timeout {
        Some(s) => {
            if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
                anyhow::bail!(
                    "Timeout value must be numeric and positive if defined, or 0 for forever"
                );
            }
            let secs: u64 = s.parse()?;
            if secs == 0 {
                None
            } else {
                Some(Duration::from_secs(secs))
            }
        }
        None => {
            let t = composer.config().process_timeout;
            if t != 0 {
                Some(Duration::from_secs(t))
            } else {
                None
            }
        }
    };

    // SAFETY: single-threaded at this point; no concurrent env access
    unsafe {
        std::env::set_var("COMPOSER_DEV_MODE", if dev_mode { "1" } else { "0" });
    }

    let bin_dir = resolve_bin_dir(&working_dir, &composer);

    let mut event_stack: Vec<String> = Vec::new();
    let exit_code = run_script(
        &script,
        &args.args,
        &scripts,
        &working_dir,
        &bin_dir,
        timeout,
        dev_mode,
        &mut event_stack,
        cli.verbose,
        io,
    )?;

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code));
    }

    Ok(())
}

#[allow(clippy::type_complexity)]
fn load_scripts(
    working_dir: &Path,
) -> anyhow::Result<(BTreeMap<String, Vec<String>>, BTreeMap<String, String>)> {
    let composer_json_path = working_dir.join("composer.json");
    if !composer_json_path.exists() {
        return Ok((BTreeMap::new(), BTreeMap::new()));
    }

    let content = std::fs::read_to_string(&composer_json_path)?;
    let parsed: serde_json::Value = serde_json::from_str(&content)?;

    let mut scripts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(scripts_obj) = parsed.get("scripts").and_then(|v| v.as_object()) {
        for (name, value) in scripts_obj {
            let listeners = match value {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect(),
                _ => vec![],
            };
            scripts.insert(name.clone(), listeners);
        }
    }

    let mut descriptions: BTreeMap<String, String> = BTreeMap::new();
    if let Some(desc_obj) = parsed
        .get("scripts-descriptions")
        .and_then(|v| v.as_object())
    {
        for (name, value) in desc_obj {
            if let Some(desc) = value.as_str() {
                descriptions.insert(name.clone(), desc.to_string());
            }
        }
    }

    Ok((scripts, descriptions))
}

fn list_scripts(
    scripts: &BTreeMap<String, Vec<String>>,
    descriptions: &BTreeMap<String, String>,
    io: Arc<Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<()> {
    if scripts.is_empty() {
        return Ok(());
    }

    console_writeln_error!(io, "<info>scripts:</info>");

    let name_width = scripts.keys().map(|n| n.len() + 2).max().unwrap_or(0);
    for name in scripts.keys() {
        let desc = descriptions.get(name).map(|s| s.as_str()).unwrap_or("");
        let padded = format!("  {:<w$}", name, w = name_width - 2);
        console_writeln!(io, "{}  {}", padded, desc);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_script(
    script: &str,
    args: &[String],
    scripts: &BTreeMap<String, Vec<String>>,
    working_dir: &Path,
    bin_dir: &Path,
    timeout: Option<Duration>,
    dev_mode: bool,
    event_stack: &mut Vec<String>,
    verbose: u8,
    io: Arc<Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<i32> {
    if event_stack.contains(&script.to_string()) {
        anyhow::bail!(
            "Circular script reference detected: {} -> {}",
            event_stack.join(" -> "),
            script
        );
    }

    event_stack.push(script.to_string());

    let listeners = scripts.get(script).cloned().unwrap_or_default();

    let mut max_exit_code = 0;

    for listener in &listeners {
        let code = run_script_entry(
            listener,
            args,
            scripts,
            working_dir,
            bin_dir,
            timeout,
            dev_mode,
            event_stack,
            verbose,
            io.clone(),
        )?;
        if code > max_exit_code {
            max_exit_code = code;
        }
        if code != 0 {
            event_stack.pop();
            anyhow::bail!(
                "Script \"{}\" returned a non-zero exit code: {}",
                script,
                code
            );
        }
    }

    event_stack.pop();
    Ok(max_exit_code)
}

#[allow(clippy::too_many_arguments)]
fn run_script_entry(
    entry: &str,
    args: &[String],
    scripts: &BTreeMap<String, Vec<String>>,
    working_dir: &Path,
    bin_dir: &Path,
    timeout: Option<Duration>,
    dev_mode: bool,
    event_stack: &mut Vec<String>,
    verbose: u8,
    io: Arc<Mutex<Box<dyn IoInterface>>>,
) -> anyhow::Result<i32> {
    let suppress_additional_args = entry.contains("@no_additional_args");
    let effective_args: &[String] = if suppress_additional_args { &[] } else { args };

    let entry = entry.replace("@no_additional_args", "").trim().to_string();

    let had_additional_args_placeholder = entry.contains("@additional_args");
    let entry = if had_additional_args_placeholder {
        let joined = effective_args.join(" ");
        entry.replace("@additional_args", &joined)
    } else {
        entry
    };

    let effective_args: &[String] = if had_additional_args_placeholder {
        &[]
    } else {
        effective_args
    };

    if is_php_callback(&entry) {
        io.lock().unwrap().info(&format!(
            "Skipping PHP callback '{}' -- Mozart cannot execute PHP class methods.",
            entry
        ));
        return Ok(0);
    }

    if is_putenv(&entry) {
        let spec = entry.trim_start_matches("@putenv ").trim();
        // SAFETY: single-threaded script execution context; no concurrent env access
        unsafe {
            if let Some((var, val)) = spec.split_once('=') {
                std::env::set_var(var, val);
            } else {
                std::env::remove_var(spec);
            }
        }
        return Ok(0);
    }

    if is_script_reference(&entry) {
        let referenced = entry.trim_start_matches('@');
        return run_script(
            referenced,
            effective_args,
            scripts,
            working_dir,
            bin_dir,
            timeout,
            dev_mode,
            event_stack,
            verbose,
            io,
        );
    }

    if is_composer_prefix(&entry) {
        let sub_args = entry.trim_start_matches("@composer ").trim();
        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("mozart"));
        let full_cmd = format!(
            "{} {}",
            current_exe.display(),
            if effective_args.is_empty() {
                sub_args.to_string()
            } else {
                format!("{} {}", sub_args, effective_args.join(" "))
            }
        );
        return run_shell_command(&full_cmd, working_dir, bin_dir, timeout, &[]);
    }

    if is_php_prefix(&entry) {
        let php_args = entry.trim_start_matches("@php ").trim();
        let php_binary = std::env::var("PHP_BINARY").unwrap_or_else(|_| "php".to_string());
        let full_cmd = if effective_args.is_empty() {
            format!("{} {}", php_binary, php_args)
        } else {
            format!("{} {} {}", php_binary, php_args, effective_args.join(" "))
        };
        return run_shell_command(&full_cmd, working_dir, bin_dir, timeout, &[]);
    }

    let full_cmd = if effective_args.is_empty() {
        entry.clone()
    } else {
        format!("{} {}", entry, effective_args.join(" "))
    };

    run_shell_command(&full_cmd, working_dir, bin_dir, timeout, &[])
}

fn run_shell_command(
    cmd: &str,
    working_dir: &Path,
    bin_dir: &Path,
    timeout: Option<Duration>,
    env_overrides: &[(String, String)],
) -> anyhow::Result<i32> {
    let path_env = {
        let current_path = std::env::var_os("PATH").unwrap_or_default();
        let mut parts: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
        parts.extend(std::env::split_paths(&current_path));
        std::env::join_paths(parts)?
    };

    #[cfg(unix)]
    let mut child = std::process::Command::new("sh");
    #[cfg(unix)]
    let child = child.args(["-c", cmd]);

    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd");
    #[cfg(windows)]
    let child = child.args(["/C", cmd]);

    let child = child.env("PATH", path_env).current_dir(working_dir);

    for (key, val) in env_overrides {
        child.env(key, val);
    }

    let mut child = child.spawn()?;

    let exit_code = if let Some(dur) = timeout {
        let result = wait_with_timeout(&mut child, dur);
        match result {
            Ok(Some(code)) => code,
            Ok(None) => {
                let _ = child.kill();
                anyhow::bail!("Script timed out after {} second(s)", dur.as_secs());
            }
            Err(e) => {
                let _ = child.kill();
                return Err(e);
            }
        }
    } else {
        child.wait()?.code().unwrap_or(1)
    };

    Ok(exit_code)
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> anyhow::Result<Option<i32>> {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(50);

    loop {
        match child.try_wait()? {
            Some(status) => return Ok(Some(status.code().unwrap_or(1))),
            None => {
                if start.elapsed() >= timeout {
                    return Ok(None);
                }
                std::thread::sleep(poll_interval);
            }
        }
    }
}

fn resolve_bin_dir(working_dir: &Path, composer: &Composer) -> PathBuf {
    // bin-dir's `{$vendor-dir}` placeholder is already resolved by Composer::load.
    working_dir.join(&composer.config().bin_dir)
}

fn is_php_callback(entry: &str) -> bool {
    let trimmed = entry.trim();
    if trimmed.contains(' ') {
        return false;
    }
    if trimmed.contains("::") {
        return true;
    }
    if trimmed.contains('\\') {
        return true;
    }
    false
}

fn is_script_reference(entry: &str) -> bool {
    entry.starts_with('@')
        && !entry.starts_with("@php ")
        && !entry.starts_with("@putenv ")
        && !entry.starts_with("@composer ")
}

fn is_php_prefix(entry: &str) -> bool {
    entry.starts_with("@php ")
}

fn is_composer_prefix(entry: &str) -> bool {
    entry.starts_with("@composer ")
}

fn is_putenv(entry: &str) -> bool {
    entry.starts_with("@putenv ")
}
