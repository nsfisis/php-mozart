use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Args)]
pub struct RunScriptArgs {
    /// Script name to run
    pub script: Option<String>,

    /// Arguments to pass to the script
    #[arg(trailing_var_arg = true)]
    pub args: Vec<String>,

    /// Set the script timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,

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

// ─── Constants ────────────────────────────────────────────────────────────────

const ALLOWED_SCRIPT_EVENTS: &[&str] = &[
    "pre-install-cmd",
    "post-install-cmd",
    "pre-update-cmd",
    "post-update-cmd",
    "pre-status-cmd",
    "post-status-cmd",
    "post-root-package-install",
    "post-create-project-cmd",
    "pre-archive-cmd",
    "post-archive-cmd",
    "pre-autoload-dump",
    "post-autoload-dump",
];

const ALL_SCRIPT_EVENTS: &[&str] = &[
    "pre-install-cmd",
    "post-install-cmd",
    "pre-update-cmd",
    "post-update-cmd",
    "pre-status-cmd",
    "post-status-cmd",
    "post-root-package-install",
    "post-create-project-cmd",
    "pre-archive-cmd",
    "post-archive-cmd",
    "pre-autoload-dump",
    "post-autoload-dump",
    "pre-dependencies-solving",
    "post-dependencies-solving",
    "pre-package-install",
    "post-package-install",
    "pre-package-update",
    "post-package-update",
    "pre-package-uninstall",
    "post-package-uninstall",
    "pre-operations-exec",
    "pre-pool-create",
    "pre-file-download",
];

// ─── Main entry point ────────────────────────────────────────────────────────

pub async fn execute(
    args: &RunScriptArgs,
    cli: &super::Cli,
    _console: &mozart_core::console::Console,
) -> anyhow::Result<()> {
    let working_dir = match &cli.working_dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };

    let (scripts, descriptions) = load_scripts(&working_dir)?;

    if args.list {
        return list_scripts(&scripts, &descriptions);
    }

    if cli.no_scripts {
        return Ok(());
    }

    let script_name = match &args.script {
        Some(name) => name.clone(),
        None => {
            anyhow::bail!("Missing required argument: script");
        }
    };

    if !ALLOWED_SCRIPT_EVENTS.contains(&script_name.as_str())
        && ALL_SCRIPT_EVENTS.contains(&script_name.as_str())
    {
        anyhow::bail!("Script \"{}\" cannot be run with this command", script_name);
    }

    if !scripts.contains_key(&script_name) {
        anyhow::bail!("Script \"{}\" is not defined in this package", script_name);
    }

    let timeout = match args.timeout {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
        None => {
            let composer_json_path = working_dir.join("composer.json");
            if let Ok(content) = std::fs::read_to_string(&composer_json_path)
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(secs) = parsed["config"]["process-timeout"].as_u64()
            {
                if secs == 0 {
                    None
                } else {
                    Some(Duration::from_secs(secs))
                }
            } else {
                Some(Duration::from_secs(300))
            }
        }
    };

    let dev_mode = !args.no_dev;

    // SAFETY: single-threaded at this point; no concurrent env access
    unsafe {
        std::env::set_var("COMPOSER_DEV_MODE", if dev_mode { "1" } else { "0" });
    }

    let bin_dir = resolve_bin_dir(&working_dir);

    let mut event_stack: Vec<String> = Vec::new();
    let exit_code = run_script(
        &script_name,
        &args.args,
        &scripts,
        &working_dir,
        &bin_dir,
        timeout,
        dev_mode,
        &mut event_stack,
        cli.verbose,
    )?;

    if exit_code != 0 {
        return Err(mozart_core::exit_code::bail_silent(exit_code));
    }

    Ok(())
}

// ─── Script loading ───────────────────────────────────────────────────────────

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
            let entries = match value {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect(),
                _ => vec![],
            };
            scripts.insert(name.clone(), entries);
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

// ─── List scripts ─────────────────────────────────────────────────────────────

fn list_scripts(
    scripts: &BTreeMap<String, Vec<String>>,
    descriptions: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    println!("scripts:");
    for name in scripts.keys() {
        let desc = descriptions.get(name).map(|s| s.as_str()).unwrap_or("");
        println!("  {}  {}", name, desc);
    }
    Ok(())
}

// ─── Script execution ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn run_script(
    script_name: &str,
    additional_args: &[String],
    scripts: &BTreeMap<String, Vec<String>>,
    working_dir: &Path,
    bin_dir: &Path,
    timeout: Option<Duration>,
    dev_mode: bool,
    event_stack: &mut Vec<String>,
    verbose: u8,
) -> anyhow::Result<i32> {
    if event_stack.contains(&script_name.to_string()) {
        anyhow::bail!(
            "Circular script reference detected: {} -> {}",
            event_stack.join(" -> "),
            script_name
        );
    }

    event_stack.push(script_name.to_string());

    let entries = scripts.get(script_name).cloned().unwrap_or_default();

    let mut max_exit_code = 0;

    for entry in &entries {
        let code = run_script_entry(
            entry,
            additional_args,
            scripts,
            working_dir,
            bin_dir,
            timeout,
            dev_mode,
            event_stack,
            verbose,
        )?;
        if code > max_exit_code {
            max_exit_code = code;
        }
        if code != 0 {
            event_stack.pop();
            anyhow::bail!(
                "Script \"{}\" returned a non-zero exit code: {}",
                script_name,
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
    additional_args: &[String],
    scripts: &BTreeMap<String, Vec<String>>,
    working_dir: &Path,
    bin_dir: &Path,
    timeout: Option<Duration>,
    dev_mode: bool,
    event_stack: &mut Vec<String>,
    verbose: u8,
) -> anyhow::Result<i32> {
    let suppress_additional_args = entry.contains("@no_additional_args");
    let effective_args: &[String] = if suppress_additional_args {
        &[]
    } else {
        additional_args
    };

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
        eprintln!(
            "Skipping PHP callback '{}' -- Mozart cannot execute PHP class methods.",
            entry
        );
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

// ─── Bin dir resolution ───────────────────────────────────────────────────────

fn resolve_bin_dir(working_dir: &Path) -> PathBuf {
    let composer_json_path = working_dir.join("composer.json");
    if let Ok(content) = std::fs::read_to_string(&composer_json_path)
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content)
    {
        let vendor_dir = parsed["config"]["vendor-dir"].as_str().unwrap_or("vendor");
        let bin_dir = parsed["config"]["bin-dir"].as_str().unwrap_or_default();
        if !bin_dir.is_empty() {
            let resolved = bin_dir.replace("{$vendor-dir}", vendor_dir);
            return working_dir.join(resolved);
        }
        return working_dir.join(vendor_dir).join("bin");
    }
    working_dir.join("vendor/bin")
}

// ─── Classifier functions ─────────────────────────────────────────────────────

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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── Classifier tests ──────────────────────────────────────────────────────

    #[test]
    fn test_is_php_callback_static_method() {
        assert!(is_php_callback("MyClass::myMethod"));
    }

    #[test]
    fn test_is_php_callback_fqn_command() {
        assert!(is_php_callback("Vendor\\MyCommand"));
    }

    #[test]
    fn test_is_php_callback_namespaced_listener() {
        assert!(is_php_callback("App\\Listeners\\PostInstall"));
    }

    #[test]
    fn test_is_php_callback_shell_command() {
        assert!(!is_php_callback("echo hello"));
    }

    #[test]
    fn test_is_php_callback_at_php() {
        assert!(!is_php_callback("@php script.php"));
    }

    #[test]
    fn test_is_script_reference() {
        assert!(is_script_reference("@test"));
        assert!(!is_script_reference("@php foo"));
        assert!(!is_script_reference("@putenv X=1"));
        assert!(!is_script_reference("@composer install"));
    }

    #[test]
    fn test_is_putenv() {
        assert!(is_putenv("@putenv FOO=bar"));
        assert!(is_putenv("@putenv FOO"));
    }

    #[test]
    fn test_is_php_prefix() {
        assert!(is_php_prefix("@php artisan migrate"));
    }

    #[test]
    fn test_is_composer_prefix() {
        assert!(is_composer_prefix("@composer install"));
    }

    // ── load_scripts tests ────────────────────────────────────────────────────

    #[test]
    fn test_load_scripts_array_form() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "scripts": {"test": ["echo a", "echo b"]}}"#,
        )
        .unwrap();

        let (scripts, _) = load_scripts(dir.path()).unwrap();
        let entries = scripts.get("test").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "echo a");
        assert_eq!(entries[1], "echo b");
    }

    #[test]
    fn test_load_scripts_string_form() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "scripts": {"test": "echo a"}}"#,
        )
        .unwrap();

        let (scripts, _) = load_scripts(dir.path()).unwrap();
        let entries = scripts.get("test").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "echo a");
    }

    #[test]
    fn test_load_scripts_with_descriptions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "scripts": {"test": "phpunit"}, "scripts-descriptions": {"test": "Run tests"}}"#,
        )
        .unwrap();

        let (_, descriptions) = load_scripts(dir.path()).unwrap();
        assert_eq!(
            descriptions.get("test").map(|s| s.as_str()),
            Some("Run tests")
        );
    }

    #[test]
    fn test_load_scripts_empty() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("composer.json"), r#"{"name": "test/pkg"}"#).unwrap();

        let (scripts, descriptions) = load_scripts(dir.path()).unwrap();
        assert!(scripts.is_empty());
        assert!(descriptions.is_empty());
    }

    #[test]
    fn test_load_scripts_mixed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "scripts": {"test": "phpunit", "post-install-cmd": ["echo installed", "echo done"]}}"#,
        )
        .unwrap();

        let (scripts, _) = load_scripts(dir.path()).unwrap();
        let test_entries = scripts.get("test").unwrap();
        assert_eq!(test_entries.len(), 1);
        let post_entries = scripts.get("post-install-cmd").unwrap();
        assert_eq!(post_entries.len(), 2);
    }

    // ── list_scripts tests ────────────────────────────────────────────────────

    #[test]
    fn test_list_scripts_output() {
        let mut scripts = BTreeMap::new();
        scripts.insert("test".to_string(), vec!["phpunit".to_string()]);
        scripts.insert("lint".to_string(), vec!["phpcs".to_string()]);

        let mut descriptions = BTreeMap::new();
        descriptions.insert("test".to_string(), "Run tests".to_string());

        // Just verify the function doesn't error
        let result = list_scripts(&scripts, &descriptions);
        assert!(result.is_ok());
    }

    // ── run_shell_command tests ───────────────────────────────────────────────

    #[test]
    fn test_run_shell_command_success() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");
        let code = run_shell_command("echo hello", dir.path(), &bin_dir, None, &[]).unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_run_shell_command_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");
        let code = run_shell_command("exit 1", dir.path(), &bin_dir, None, &[]).unwrap();
        assert_eq!(code, 1);
    }

    #[test]
    fn test_run_shell_command_with_args() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let mut scripts = BTreeMap::new();
        scripts.insert("greet".to_string(), vec!["echo".to_string()]);

        let mut stack = vec![];
        let code = run_script(
            "greet",
            &["world".to_string()],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        )
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_run_putenv_set() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let mut scripts = BTreeMap::new();
        scripts.insert(
            "setup".to_string(),
            vec!["@putenv MOZART_TEST_VAR=hello_world".to_string()],
        );

        let mut stack = vec![];
        run_script(
            "setup",
            &[],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        )
        .unwrap();

        assert_eq!(std::env::var("MOZART_TEST_VAR").unwrap(), "hello_world");
    }

    #[test]
    fn test_run_putenv_unset() {
        // SAFETY: test-only; no concurrent env access in this test
        unsafe { std::env::set_var("MOZART_UNSET_VAR", "some_value") };

        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let mut scripts = BTreeMap::new();
        scripts.insert(
            "cleanup".to_string(),
            vec!["@putenv MOZART_UNSET_VAR".to_string()],
        );

        let mut stack = vec![];
        run_script(
            "cleanup",
            &[],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        )
        .unwrap();

        assert!(std::env::var("MOZART_UNSET_VAR").is_err());
    }

    #[test]
    fn test_run_script_reference() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let mut scripts = BTreeMap::new();
        scripts.insert("a".to_string(), vec!["@b".to_string()]);
        scripts.insert("b".to_string(), vec!["echo from-b".to_string()]);

        let mut stack = vec![];
        let code = run_script(
            "a",
            &[],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        )
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_circular_reference_detected() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let mut scripts = BTreeMap::new();
        scripts.insert("a".to_string(), vec!["@b".to_string()]);
        scripts.insert("b".to_string(), vec!["@a".to_string()]);

        let mut stack = vec![];
        let result = run_script(
            "a",
            &[],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Circular script reference"));
    }

    #[test]
    fn test_php_callback_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let mut scripts = BTreeMap::new();
        scripts.insert(
            "callback".to_string(),
            vec!["MyClass::myMethod".to_string()],
        );

        let mut stack = vec![];
        let code = run_script(
            "callback",
            &[],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        )
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_no_additional_args_respected() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        // Use @no_additional_args -- extra args should not be forwarded
        let mut scripts = BTreeMap::new();
        scripts.insert(
            "test".to_string(),
            vec!["echo base @no_additional_args".to_string()],
        );

        let mut stack = vec![];
        let code = run_script(
            "test",
            &["extra-arg".to_string()],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        )
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_additional_args_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let mut scripts = BTreeMap::new();
        scripts.insert(
            "test".to_string(),
            vec!["echo before @additional_args after".to_string()],
        );

        let mut stack = vec![];
        let code = run_script(
            "test",
            &["injected".to_string()],
            &scripts,
            dir.path(),
            &bin_dir,
            None,
            true,
            &mut stack,
            0,
        )
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_script_not_defined_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("composer.json"), r#"{"name": "test/pkg"}"#).unwrap();

        let (scripts, _) = load_scripts(dir.path()).unwrap();
        assert!(!scripts.contains_key("nonexistent"));
    }

    #[test]
    fn test_bin_dir_in_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let fake_bin = bin_dir.join("my-fake-tool");
        fs::write(&fake_bin, "#!/bin/sh\necho ran-fake-tool\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake_bin, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let code = run_shell_command("my-fake-tool", dir.path(), &bin_dir, None, &[]).unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_timeout_kills_long_running() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        let result = run_shell_command(
            "sleep 10",
            dir.path(),
            &bin_dir,
            Some(Duration::from_secs(1)),
            &[],
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("timed out"));
    }

    #[test]
    fn test_composer_dev_mode_env() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("vendor/bin");

        // Run with dev mode true
        let code = run_shell_command(
            "test \"$COMPOSER_DEV_MODE\" = \"1\"",
            dir.path(),
            &bin_dir,
            None,
            &[("COMPOSER_DEV_MODE".to_string(), "1".to_string())],
        )
        .unwrap();
        assert_eq!(code, 0);

        // Run with dev mode false
        let code = run_shell_command(
            "test \"$COMPOSER_DEV_MODE\" = \"0\"",
            dir.path(),
            &bin_dir,
            None,
            &[("COMPOSER_DEV_MODE".to_string(), "0".to_string())],
        )
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn test_list_flag() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name": "test/pkg", "scripts": {"test": "phpunit", "lint": "phpcs"}}"#,
        )
        .unwrap();

        let (scripts, descriptions) = load_scripts(dir.path()).unwrap();
        assert!(scripts.contains_key("test"));
        assert!(scripts.contains_key("lint"));

        let result = list_scripts(&scripts, &descriptions);
        assert!(result.is_ok());
    }

    #[test]
    fn test_internal_event_rejected() {
        // Internal events are in ALL_SCRIPT_EVENTS but not in ALLOWED_SCRIPT_EVENTS
        assert!(ALL_SCRIPT_EVENTS.contains(&"pre-package-install"));
        assert!(ALL_SCRIPT_EVENTS.contains(&"post-package-install"));
        assert!(ALL_SCRIPT_EVENTS.contains(&"pre-dependencies-solving"));
        assert!(!ALLOWED_SCRIPT_EVENTS.contains(&"pre-package-install"));
        assert!(!ALLOWED_SCRIPT_EVENTS.contains(&"post-package-install"));
        assert!(!ALLOWED_SCRIPT_EVENTS.contains(&"pre-dependencies-solving"));
        // User-runnable events are in both
        assert!(ALLOWED_SCRIPT_EVENTS.contains(&"pre-install-cmd"));
        assert!(ALL_SCRIPT_EVENTS.contains(&"pre-install-cmd"));
    }
}
