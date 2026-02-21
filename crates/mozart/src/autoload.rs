use crate::installed::InstalledPackages;
use crate::lockfile::LockedPackage;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

// Embed Composer PHP files from the submodule at compile time.
const CLASSLOADER_PHP: &str =
    include_str!("../../../composer/src/Composer/Autoload/ClassLoader.php");
const INSTALLED_VERSIONS_PHP: &str =
    include_str!("../../../composer/src/Composer/InstalledVersions.php");
const COMPOSER_LICENSE: &str = include_str!("../../../composer/LICENSE");

/// How platform requirements are checked during autoloader generation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PlatformCheckMode {
    /// Check all platform requirements (php, ext-*, lib-*).
    #[default]
    Full,
    /// Only check the PHP version requirement.
    PhpOnly,
    /// Disable platform requirement checks entirely.
    Disabled,
}

/// Configuration for autoload generation.
pub struct AutoloadConfig {
    /// Absolute path to the project root (where composer.json lives).
    pub project_dir: PathBuf,
    /// Absolute path to the vendor directory.
    pub vendor_dir: PathBuf,
    /// Whether dev-mode autoloading is active (include autoload-dev rules).
    pub dev_mode: bool,
    /// Unique suffix for the autoloader class names (typically the lock file content-hash).
    /// Used to generate `ComposerAutoloaderInit{suffix}` and `ComposerStaticInit{suffix}`.
    pub suffix: String,
    /// When true, emit `$loader->setClassMapAuthoritative(true)` in the generated autoloader.
    pub classmap_authoritative: bool,
    /// When true, scan PSR-4/PSR-0 directories and generate a full classmap (optimize mode).
    pub optimize: bool,
    /// When true, generate APCu-based class caching in the autoloader.
    pub apcu: bool,
    /// Optional prefix for APCu cache keys (implies `apcu`).
    pub apcu_prefix: Option<String>,
    /// When true, return an error on PSR mapping violations detected during classmap scan.
    pub strict_psr: bool,
    /// How to handle platform requirement checks.
    pub platform_check: PlatformCheckMode,
    /// When true, skip all platform requirement checks.
    pub ignore_platform_reqs: bool,
}

/// Collected autoload mappings from all packages.
pub struct AutoloadData {
    /// PSR-4: namespace prefix -> list of directory path expressions.
    /// Each path is a PHP expression string like `$vendorDir . '/psr/log/src'`.
    pub psr4: BTreeMap<String, Vec<String>>,
    /// PSR-0: namespace prefix -> list of directory path expressions.
    /// (Empty in Phase 2.2, populated in 5.6.)
    pub psr0: BTreeMap<String, Vec<String>>,
    /// Classmap entries: class name -> file path expression.
    /// (Empty in Phase 2.2, populated in 5.6.)
    pub classmap: BTreeMap<String, String>,
    /// Files to include on every request: file_identifier -> path expression.
    pub files: BTreeMap<String, String>,
}

/// Escape a string for use in a PHP single-quoted string literal.
pub fn php_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Compute the file identifier matching Composer's `getFileIdentifier()`.
/// This is the MD5 hex digest of `"package_name:path"`.
pub fn file_identifier(package_name: &str, path: &str) -> String {
    let input = format!("{package_name}:{path}");
    format!("{:x}", md5::compute(input.as_bytes()))
}

/// Extract a path or array of paths from a JSON value.
/// Handles both string and array-of-strings (Composer allows both).
fn json_to_paths(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    }
}

/// Strip trailing slash from a path component.
fn strip_trailing_slash(s: &str) -> &str {
    s.trim_end_matches('/')
}

/// Normalize a PSR-4 namespace: ensure it ends with `\`.
/// (The empty string "" is valid and is left as-is.)
fn normalize_namespace(ns: &str) -> String {
    if ns.is_empty() || ns.ends_with('\\') {
        ns.to_string()
    } else {
        format!("{ns}\\")
    }
}

/// Build a PHP path expression from a base expression and a relative path component.
///
/// For vendor packages: `base_expr` = `"$vendorDir"`, `pkg_path` = `"psr/log"`,
/// `sub_path` = `"src/"` → result: `"$vendorDir . '/psr/log/src'"`.
///
/// For root packages: `base_expr` = `"$baseDir"`, `pkg_path` = `""`,
/// `sub_path` = `"src/"` → result: `"$baseDir . '/src'"`.
fn build_path_expr(base_expr: &str, pkg_path: &str, sub_path: &str) -> String {
    let sub = strip_trailing_slash(sub_path);
    let combined = if pkg_path.is_empty() {
        sub.to_string()
    } else if sub.is_empty() {
        pkg_path.to_string()
    } else {
        format!("{pkg_path}/{sub}")
    };

    if combined.is_empty() {
        base_expr.to_string()
    } else {
        format!("{base_expr} . '/{combined}'")
    }
}

/// Process an autoload JSON value and merge its rules into `data`.
///
/// `pkg_path` is the package-relative path segment within vendor.
/// For vendor packages it is `"vendor/name"` (e.g. `"psr/log"`).
/// For the root package it is `""`.
///
/// `dyn_base` is the dynamic PHP variable: `"$vendorDir"` or `"$baseDir"`.
/// `static_base` is the static PHP expression: `"__DIR__ . '/..'"` or `"__DIR__ . '/../.'"`.
fn process_autoload_value(
    autoload_val: &serde_json::Value,
    package_name: &str,
    pkg_path: &str,
    dyn_base: &str,
    static_base: &str,
    data: &mut AutoloadData,
    static_data: &mut AutoloadData,
) {
    // PSR-4
    if let Some(psr4_obj) = autoload_val.get("psr-4").and_then(|v| v.as_object()) {
        for (ns_raw, paths_val) in psr4_obj {
            let ns = normalize_namespace(ns_raw);
            let paths = json_to_paths(paths_val);
            let entry = data.psr4.entry(ns.clone()).or_default();
            let static_entry = static_data.psr4.entry(ns).or_default();
            for path in paths {
                entry.push(build_path_expr(dyn_base, pkg_path, &path));
                static_entry.push(build_path_expr(static_base, pkg_path, &path));
            }
        }
    }

    // PSR-0
    if let Some(psr0_obj) = autoload_val.get("psr-0").and_then(|v| v.as_object()) {
        for (ns_raw, paths_val) in psr0_obj {
            let ns = ns_raw.clone();
            let paths = json_to_paths(paths_val);
            let entry = data.psr0.entry(ns.clone()).or_default();
            let static_entry = static_data.psr0.entry(ns).or_default();
            for path in paths {
                entry.push(build_path_expr(dyn_base, pkg_path, &path));
                static_entry.push(build_path_expr(static_base, pkg_path, &path));
            }
        }
    }

    // Files
    if let Some(files_arr) = autoload_val.get("files").and_then(|v| v.as_array()) {
        for file_val in files_arr {
            if let Some(file_path) = file_val.as_str() {
                let id = file_identifier(package_name, file_path);
                let expr = build_path_expr(dyn_base, pkg_path, file_path);
                let static_expr = build_path_expr(static_base, pkg_path, file_path);
                data.files.insert(id.clone(), expr);
                static_data.files.insert(id, static_expr);
            }
        }
    }
}

/// Collect autoload rules from all installed packages and the root package.
///
/// Returns a tuple of `(dynamic_data, static_data)` where:
/// - `dynamic_data` uses `$vendorDir` / `$baseDir` path expressions (for autoload_psr4.php, etc.)
/// - `static_data` uses `__DIR__ . '/..'` path expressions (for autoload_static.php)
fn collect_autoloads(
    installed: &InstalledPackages,
    root_autoload: Option<&serde_json::Value>,
    root_autoload_dev: Option<&serde_json::Value>,
    root_package_name: &str,
    dev_mode: bool,
) -> (AutoloadData, AutoloadData) {
    let mut data = AutoloadData {
        psr4: BTreeMap::new(),
        psr0: BTreeMap::new(),
        classmap: BTreeMap::new(),
        files: BTreeMap::new(),
    };
    let mut static_data = AutoloadData {
        psr4: BTreeMap::new(),
        psr0: BTreeMap::new(),
        classmap: BTreeMap::new(),
        files: BTreeMap::new(),
    };

    // Process each installed package
    for pkg in &installed.packages {
        if let Some(autoload_val) = &pkg.autoload {
            process_autoload_value(
                autoload_val,
                &pkg.name,
                &pkg.name, // pkg_path within vendor
                "$vendorDir",
                "__DIR__ . '/..'",
                &mut data,
                &mut static_data,
            );
        }
    }

    // Process root package autoload
    if let Some(autoload_val) = root_autoload {
        process_autoload_value(
            autoload_val,
            root_package_name,
            "", // no pkg_path for root
            "$baseDir",
            "__DIR__ . '/../..'",
            &mut data,
            &mut static_data,
        );
    }

    // Process root package autoload-dev (only in dev mode)
    if dev_mode && let Some(autoload_dev_val) = root_autoload_dev {
        process_autoload_value(
            autoload_dev_val,
            root_package_name,
            "",
            "$baseDir",
            "__DIR__ . '/../..'",
            &mut data,
            &mut static_data,
        );
    }

    (data, static_data)
}

/// Generate `vendor/composer/autoload_psr4.php`.
fn generate_autoload_psr4(data: &AutoloadData) -> String {
    let mut out = String::new();
    out.push_str("<?php\n\n// autoload_psr4.php @generated by Composer\n\n");
    out.push_str("$vendorDir = dirname(__DIR__);\n");
    out.push_str("$baseDir = dirname($vendorDir);\n\n");
    out.push_str("return array(\n");

    // krsort: reverse alphabetical (longer/more specific namespaces first)
    let mut sorted: Vec<(&String, &Vec<String>)> = data.psr4.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| b.cmp(a));

    for (ns, paths) in &sorted {
        let escaped_ns = php_escape(ns);
        if paths.len() == 1 {
            out.push_str(&format!("    '{}' => array({}),\n", escaped_ns, paths[0]));
        } else {
            out.push_str(&format!("    '{}' => array(\n", escaped_ns));
            for path in paths.iter() {
                out.push_str(&format!("        {},\n", path));
            }
            out.push_str("    ),\n");
        }
    }

    out.push_str(");\n");
    out
}

/// Generate `vendor/composer/autoload_namespaces.php` (PSR-0, empty for Phase 2.2).
fn generate_autoload_namespaces(data: &AutoloadData) -> String {
    let mut out = String::new();
    out.push_str("<?php\n\n// autoload_namespaces.php @generated by Composer\n\n");
    out.push_str("$vendorDir = dirname(__DIR__);\n");
    out.push_str("$baseDir = dirname($vendorDir);\n\n");
    out.push_str("return array(\n");

    let mut sorted: Vec<(&String, &Vec<String>)> = data.psr0.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| b.cmp(a));

    for (ns, paths) in &sorted {
        let escaped_ns = php_escape(ns);
        if paths.len() == 1 {
            out.push_str(&format!("    '{}' => array({}),\n", escaped_ns, paths[0]));
        } else {
            out.push_str(&format!("    '{}' => array(\n", escaped_ns));
            for path in paths.iter() {
                out.push_str(&format!("        {},\n", path));
            }
            out.push_str("    ),\n");
        }
    }

    out.push_str(");\n");
    out
}

/// Generate `vendor/composer/autoload_classmap.php`.
/// Always contains `Composer\InstalledVersions`; classmap scanning deferred to Phase 5.6.
fn generate_autoload_classmap(data: &AutoloadData) -> String {
    let mut out = String::new();
    out.push_str("<?php\n\n// autoload_classmap.php @generated by Composer\n\n");
    out.push_str("$vendorDir = dirname(__DIR__);\n");
    out.push_str("$baseDir = dirname($vendorDir);\n\n");
    out.push_str("return array(\n");
    out.push_str(
        "    'Composer\\\\InstalledVersions' => $vendorDir . '/composer/InstalledVersions.php',\n",
    );

    // Include any additional classmap entries from data
    for (class, path) in &data.classmap {
        let escaped_class = php_escape(class);
        out.push_str(&format!("    '{}' => {},\n", escaped_class, path));
    }

    out.push_str(");\n");
    out
}

/// Generate `vendor/composer/autoload_files.php`.
/// Returns `None` if there are no files to autoload.
fn generate_autoload_files(data: &AutoloadData) -> Option<String> {
    if data.files.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("<?php\n\n// autoload_files.php @generated by Composer\n\n");
    out.push_str("$vendorDir = dirname(__DIR__);\n");
    out.push_str("$baseDir = dirname($vendorDir);\n\n");
    out.push_str("return array(\n");

    for (id, path) in &data.files {
        out.push_str(&format!("    '{}' => {},\n", id, path));
    }

    out.push_str(");\n");
    Some(out)
}

/// Generate `vendor/composer/autoload_static.php`.
///
/// `static_data` must have been collected with `__DIR__ . '/..'` path prefixes.
fn generate_autoload_static(static_data: &AutoloadData, suffix: &str) -> String {
    let mut out = String::new();
    out.push_str("<?php\n\n// autoload_static.php @generated by Composer\n\n");
    out.push_str("namespace Composer\\Autoload;\n\n");
    out.push_str(&format!("class ComposerStaticInit{suffix}\n{{\n"));

    // $files
    if !static_data.files.is_empty() {
        out.push_str("    public static $files = array (\n");
        for (id, path) in &static_data.files {
            out.push_str(&format!("        '{id}' => {path},\n"));
        }
        out.push_str("    );\n\n");
    }

    // $prefixLengthsPsr4 — group by first character of namespace
    if !static_data.psr4.is_empty() {
        // Group namespaces by first character, sorted reverse
        let mut by_char: BTreeMap<char, Vec<(&String, usize)>> = BTreeMap::new();

        let mut sorted_ns: Vec<&String> = static_data.psr4.keys().collect();
        sorted_ns.sort_by(|a, b| b.cmp(a));

        for ns in sorted_ns {
            if let Some(first_char) = ns.chars().next() {
                // The byte length in PHP (single-quoted string with single backslashes)
                // ns in our data uses single backslash (stored as-is from JSON).
                let byte_len = ns.len();
                by_char.entry(first_char).or_default().push((ns, byte_len));
            }
        }

        out.push_str("    public static $prefixLengthsPsr4 = array (\n");
        // Sort characters in reverse order too
        let mut chars: Vec<char> = by_char.keys().copied().collect();
        chars.sort_by(|a, b| b.cmp(a));
        for ch in &chars {
            out.push_str(&format!("        '{ch}' =>\n        array (\n"));
            if let Some(entries) = by_char.get(ch) {
                for (ns, len) in entries {
                    let escaped_ns = php_escape(ns);
                    out.push_str(&format!("            '{escaped_ns}' => {len},\n"));
                }
            }
            out.push_str("        ),\n");
        }
        out.push_str("    );\n\n");

        // $prefixDirsPsr4
        out.push_str("    public static $prefixDirsPsr4 = array (\n");
        let mut sorted_ns2: Vec<(&String, &Vec<String>)> = static_data.psr4.iter().collect();
        sorted_ns2.sort_by(|(a, _), (b, _)| b.cmp(a));
        for (ns, paths) in sorted_ns2 {
            let escaped_ns = php_escape(ns);
            out.push_str(&format!("        '{escaped_ns}' =>\n        array (\n"));
            for (i, path) in paths.iter().enumerate() {
                out.push_str(&format!("            {i} => {path},\n"));
            }
            out.push_str("        ),\n");
        }
        out.push_str("    );\n\n");
    }

    // $classMap — always contains Composer\InstalledVersions
    out.push_str("    public static $classMap = array (\n");
    out.push_str(
        "        'Composer\\\\InstalledVersions' => __DIR__ . '/..' . '/composer/InstalledVersions.php',\n",
    );
    for (class, path) in &static_data.classmap {
        let escaped_class = php_escape(class);
        out.push_str(&format!("        '{}' => {},\n", escaped_class, path));
    }
    out.push_str("    );\n\n");

    // getInitializer
    out.push_str("    public static function getInitializer(ClassLoader $loader)\n    {\n");
    out.push_str("        return \\Closure::bind(function () use ($loader) {\n");

    if !static_data.psr4.is_empty() {
        out.push_str(&format!(
            "            $loader->prefixLengthsPsr4 = ComposerStaticInit{suffix}::$prefixLengthsPsr4;\n"
        ));
        out.push_str(&format!(
            "            $loader->prefixDirsPsr4 = ComposerStaticInit{suffix}::$prefixDirsPsr4;\n"
        ));
    }
    out.push_str(&format!(
        "            $loader->classMap = ComposerStaticInit{suffix}::$classMap;\n"
    ));
    out.push_str("\n        }, null, ClassLoader::class);\n    }\n}\n");

    out
}

/// Recursively collect PHP files from a directory, skipping excluded paths.
fn collect_php_files(
    dir: &Path,
    excluded: &[String],
    vendor_dir: &Path,
    project_dir: &Path,
) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if !dir.is_dir() {
        return result;
    }
    collect_php_files_inner(dir, excluded, vendor_dir, project_dir, &mut result);
    result
}

fn collect_php_files_inner(
    dir: &Path,
    excluded: &[String],
    vendor_dir: &Path,
    project_dir: &Path,
    result: &mut Vec<PathBuf>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();

        // Check if path matches any excluded pattern
        if is_excluded(&path, excluded, vendor_dir, project_dir) {
            continue;
        }

        if path.is_dir() {
            collect_php_files_inner(&path, excluded, vendor_dir, project_dir, result);
        } else if crate::php_scanner::is_php_ext(&path) {
            result.push(path);
        }
    }
}

/// Check whether a path matches any of the excluded patterns.
fn is_excluded(path: &Path, excluded: &[String], vendor_dir: &Path, project_dir: &Path) -> bool {
    for exc in excluded {
        // Excluded patterns can be relative to project_dir or absolute
        let exc_path = if Path::new(exc).is_absolute() {
            PathBuf::from(exc)
        } else {
            project_dir.join(exc)
        };
        if path.starts_with(&exc_path) || path == exc_path {
            return true;
        }
        // Also check relative to vendor_dir
        let exc_vendor = vendor_dir.join(exc);
        if path.starts_with(&exc_vendor) || path == exc_vendor {
            return true;
        }
    }
    false
}

/// Scan directories for PHP class declarations and return a classmap.
///
/// `dirs` is a list of absolute directory paths to scan.
/// Returns a `BTreeMap<class_name, file_path_expression>` where the path expression
/// uses `$vendorDir` or `$baseDir` as appropriate.
fn scan_classmap_dirs(
    dirs: &[PathBuf],
    vendor_dir: &Path,
    project_dir: &Path,
    excluded: &[String],
) -> BTreeMap<String, String> {
    let mut classmap = BTreeMap::new();

    for dir in dirs {
        let files = collect_php_files(dir, excluded, vendor_dir, project_dir);
        for file in files {
            match crate::php_scanner::find_classes(&file) {
                Ok(classes) => {
                    for class in classes {
                        let path_expr = path_to_php_expr(&file, vendor_dir, project_dir);
                        classmap.entry(class).or_insert(path_expr);
                    }
                }
                Err(_) => continue,
            }
        }
    }

    classmap
}

/// Convert an absolute file path to a PHP path expression using `$vendorDir` or `$baseDir`.
fn path_to_php_expr(file: &Path, vendor_dir: &Path, project_dir: &Path) -> String {
    if let Ok(rel) = file.strip_prefix(vendor_dir) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        format!("$vendorDir . '/{rel_str}'")
    } else if let Ok(rel) = file.strip_prefix(project_dir) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        format!("$baseDir . '/{rel_str}'")
    } else {
        // Fall back to absolute path
        let abs = file.to_string_lossy().replace('\\', "/");
        format!("'{abs}'")
    }
}

/// Convert an absolute file path to a static PHP path expression using `__DIR__ . '/..` form.
fn path_to_static_expr(file: &Path, vendor_dir: &Path, project_dir: &Path) -> String {
    if let Ok(rel) = file.strip_prefix(vendor_dir) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        format!("__DIR__ . '/..' . '/{rel_str}'")
    } else if let Ok(rel) = file.strip_prefix(project_dir) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        format!("__DIR__ . '/../..' . '/{rel_str}'")
    } else {
        let abs = file.to_string_lossy().replace('\\', "/");
        format!("'{abs}'")
    }
}

/// Scan PSR-4 and PSR-0 directories for class declarations (used in optimize mode).
///
/// Returns `(dynamic_classmap, static_classmap, psr_violations)`.
fn scan_psr_for_classmap(
    psr4: &BTreeMap<String, Vec<String>>,
    psr0: &BTreeMap<String, Vec<String>>,
    vendor_dir: &Path,
    project_dir: &Path,
    excluded: &[String],
) -> (
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    Vec<String>,
) {
    let mut dyn_map: BTreeMap<String, String> = BTreeMap::new();
    let mut static_map: BTreeMap<String, String> = BTreeMap::new();
    let mut violations: Vec<String> = Vec::new();

    // Helper: resolve a PHP path expression to an absolute path.
    let resolve = |expr: &str| -> Option<PathBuf> {
        // Expressions look like:
        //   $vendorDir . '/psr/log/src'
        //   $baseDir . '/src'
        //   __DIR__ . '/..' . '/psr/log/src'
        //   __DIR__ . '/../..' . '/src'
        if let Some(rest) = expr.strip_prefix("$vendorDir . '") {
            let rel = rest.trim_end_matches('\'');
            Some(vendor_dir.join(rel.trim_start_matches('/')))
        } else if let Some(rest) = expr.strip_prefix("$baseDir . '") {
            let rel = rest.trim_end_matches('\'');
            Some(project_dir.join(rel.trim_start_matches('/')))
        } else if expr == "$vendorDir" {
            Some(vendor_dir.to_path_buf())
        } else if expr == "$baseDir" {
            Some(project_dir.to_path_buf())
        } else {
            None
        }
    };

    // Scan PSR-4 dirs
    for (ns, paths) in psr4 {
        for path_expr in paths {
            if let Some(abs_dir) = resolve(path_expr) {
                let files = collect_php_files(&abs_dir, excluded, vendor_dir, project_dir);
                for file in files {
                    match crate::php_scanner::find_classes(&file) {
                        Ok(classes) => {
                            for class in classes {
                                // PSR-4 validation
                                let file_str = file.to_string_lossy();
                                let dir_str = abs_dir.to_string_lossy();
                                let base_ns = ns.as_str();
                                if !crate::php_scanner::validate_psr4_class(
                                    &class, base_ns, &file_str, &dir_str,
                                ) {
                                    violations.push(format!(
                                        "Class {class} in {file_str} does not comply with PSR-4 (namespace prefix: {ns})"
                                    ));
                                }
                                let dyn_expr = path_to_php_expr(&file, vendor_dir, project_dir);
                                let static_expr =
                                    path_to_static_expr(&file, vendor_dir, project_dir);
                                dyn_map.entry(class.clone()).or_insert(dyn_expr);
                                static_map.entry(class).or_insert(static_expr);
                            }
                        }
                        Err(_) => continue,
                    }
                }
            }
        }
    }

    // Scan PSR-0 dirs
    for (ns, paths) in psr0 {
        for path_expr in paths {
            if let Some(abs_dir) = resolve(path_expr) {
                let files = collect_php_files(&abs_dir, excluded, vendor_dir, project_dir);
                for file in files {
                    match crate::php_scanner::find_classes(&file) {
                        Ok(classes) => {
                            for class in classes {
                                let file_str = file.to_string_lossy();
                                let dir_str = abs_dir.to_string_lossy();
                                if !crate::php_scanner::validate_psr0_class(
                                    &class, &file_str, &dir_str,
                                ) {
                                    violations.push(format!(
                                        "Class {class} in {file_str} does not comply with PSR-0 (namespace prefix: {ns})"
                                    ));
                                }
                                let dyn_expr = path_to_php_expr(&file, vendor_dir, project_dir);
                                let static_expr =
                                    path_to_static_expr(&file, vendor_dir, project_dir);
                                dyn_map.entry(class.clone()).or_insert(dyn_expr);
                                static_map.entry(class).or_insert(static_expr);
                            }
                        }
                        Err(_) => continue,
                    }
                }
            }
        }
    }

    (dyn_map, static_map, violations)
}

/// Generate `vendor/composer/platform_check.php`.
///
/// Returns `None` if mode is `Disabled` or there are no relevant requirements.
fn generate_platform_check(
    packages: &[LockedPackage],
    root_require: Option<&serde_json::Value>,
    mode: &PlatformCheckMode,
    dev_package_names: &HashSet<String>,
) -> Option<String> {
    if matches!(mode, PlatformCheckMode::Disabled) {
        return None;
    }

    // Collect PHP version constraint from root require
    let mut php_constraint: Option<String> = None;
    if let Some(req_obj) = root_require.and_then(|v| v.as_object())
        && let Some(v) = req_obj.get("php").and_then(|v| v.as_str())
    {
        php_constraint = Some(v.to_string());
    }

    // Collect extension requirements from packages (prod only)
    let mut ext_reqs: Vec<(String, String)> = Vec::new();
    if matches!(mode, PlatformCheckMode::Full) {
        for pkg in packages {
            let is_dev = dev_package_names.contains(&pkg.name.to_lowercase());
            if is_dev {
                continue;
            }
            for (req_name, req_constraint) in &pkg.require {
                let lower = req_name.to_lowercase();
                if lower.starts_with("ext-") {
                    ext_reqs.push((req_name.clone(), req_constraint.clone()));
                }
            }
        }
        ext_reqs.sort();
        ext_reqs.dedup();
    }

    if php_constraint.is_none() && ext_reqs.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("<?php\n\n");
    out.push_str("// platform_check.php @generated by Composer\n\n");
    out.push_str("$issues = array();\n\n");

    if let Some(ref constraint) = php_constraint {
        // Emit a simple PHP version check
        let escaped = php_escape(constraint);
        out.push_str(&format!("// PHP version check: {constraint}\n"));
        out.push_str("if (!(PHP_VERSION_ID >= 50600)) {\n");
        out.push_str(&format!(
            "    $issues[] = 'Your Composer dependencies require a PHP version \"{escaped}\". You are running ' . PHP_VERSION . '.';\n"
        ));
        out.push_str("}\n\n");
    }

    for (ext_name, _constraint) in &ext_reqs {
        let ext_short = ext_name.trim_start_matches("ext-");
        let escaped_ext = php_escape(ext_short);
        out.push_str(&format!("if (!extension_loaded('{escaped_ext}')) {{\n"));
        out.push_str(&format!(
            "    $issues[] = 'Your Composer dependencies require the \"{escaped_ext}\" PHP extension to be installed.';\n"
        ));
        out.push_str("}\n\n");
    }

    out.push_str("if ($issues) {\n");
    out.push_str("    if (!headers_sent()) {\n");
    out.push_str("        header('HTTP/1.1 500 Internal Server Error');\n");
    out.push_str("    }\n");
    out.push_str("    if (!ini_get('display_errors')) {\n");
    out.push_str("        if (PHP_SAPI === 'cli' || PHP_SAPI === 'phpdbg') {\n");
    out.push_str("            fwrite(STDERR, 'Composer detected issues in your platform:' . PHP_EOL.PHP_EOL . implode(PHP_EOL, $issues) . PHP_EOL);\n");
    out.push_str("        } elseif (!headers_sent()) {\n");
    out.push_str("            echo 'Composer detected issues in your platform:' . PHP_EOL.PHP_EOL . implode(PHP_EOL, $issues) . PHP_EOL;\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    trigger_error(\n");
    out.push_str(
        "        'Composer detected issues in your platform: ' . implode(' ', $issues),\n",
    );
    out.push_str("        E_USER_ERROR\n");
    out.push_str("    );\n");
    out.push_str("}\n");

    Some(out)
}

/// Generate `vendor/composer/autoload_real.php`.
fn generate_autoload_real(
    suffix: &str,
    has_files: bool,
    classmap_authoritative: bool,
    apcu: bool,
    apcu_prefix: Option<&str>,
    has_platform_check: bool,
) -> String {
    let mut out = String::new();
    out.push_str("<?php\n\n");
    out.push_str("// autoload_real.php @generated by Composer\n\n");
    out.push_str(&format!("class ComposerAutoloaderInit{suffix}\n"));
    out.push_str("{\n");
    out.push_str("    private static $loader;\n\n");
    out.push_str("    public static function loadClassLoader($class)\n");
    out.push_str("    {\n");
    out.push_str("        if ('Composer\\Autoload\\ClassLoader' === $class) {\n");
    out.push_str("            require __DIR__ . '/ClassLoader.php';\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");
    out.push_str("    /**\n");
    out.push_str("     * @return \\Composer\\Autoload\\ClassLoader\n");
    out.push_str("     */\n");
    out.push_str("    public static function getLoader()\n");
    out.push_str("    {\n");
    out.push_str("        if (null !== self::$loader) {\n");
    out.push_str("            return self::$loader;\n");
    out.push_str("        }\n\n");
    out.push_str(&format!(
        "        spl_autoload_register(array('ComposerAutoloaderInit{suffix}', 'loadClassLoader'), true, true);\n"
    ));
    out.push_str(
        "        self::$loader = $loader = new \\Composer\\Autoload\\ClassLoader(\\dirname(__DIR__));\n",
    );
    out.push_str(&format!(
        "        spl_autoload_unregister(array('ComposerAutoloaderInit{suffix}', 'loadClassLoader'));\n\n"
    ));
    if has_platform_check {
        out.push_str("        require __DIR__ . '/platform_check.php';\n");
    }
    out.push_str("        require __DIR__ . '/autoload_static.php';\n");
    out.push_str(&format!(
        "        call_user_func(\\Composer\\Autoload\\ComposerStaticInit{suffix}::getInitializer($loader));\n\n"
    ));
    out.push_str("        $loader->register(true);\n");

    if classmap_authoritative {
        out.push_str("        $loader->setClassMapAuthoritative(true);\n");
    }

    if apcu {
        let prefix = apcu_prefix.unwrap_or(suffix);
        let escaped = php_escape(prefix);
        out.push_str(&format!("        $loader->setApcuPrefix('{escaped}');\n"));
    }

    if has_files {
        out.push('\n');
        out.push_str(&format!(
            "        $filesToLoad = \\Composer\\Autoload\\ComposerStaticInit{suffix}::$files;\n"
        ));
        out.push_str(
            "        $requireFile = \\Closure::bind(static function ($fileIdentifier, $file) {\n",
        );
        out.push_str(
            "            if (empty($GLOBALS['__composer_autoload_files'][$fileIdentifier])) {\n",
        );
        out.push_str(
            "                $GLOBALS['__composer_autoload_files'][$fileIdentifier] = true;\n",
        );
        out.push('\n');
        out.push_str("                require $file;\n");
        out.push_str("            }\n");
        out.push_str("        }, null, null);\n");
        out.push_str("        foreach ($filesToLoad as $fileIdentifier => $file) {\n");
        out.push_str("            $requireFile($fileIdentifier, $file);\n");
        out.push_str("        }\n");
    }

    out.push('\n');
    out.push_str("        return $loader;\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

/// Generate `vendor/autoload.php` (the entry point).
fn generate_autoload_php(suffix: &str) -> String {
    let mut out = String::new();
    out.push_str("<?php\n\n");
    out.push_str("// autoload.php @generated by Composer\n\n");
    out.push_str("if (PHP_VERSION_ID < 50600) {\n");
    out.push_str("    if (!headers_sent()) {\n");
    out.push_str("        header('HTTP/1.1 500 Internal Server Error');\n");
    out.push_str("    }\n");
    out.push_str("    $err = 'Composer 2.3.0 dropped support for autoloading on PHP <5.6 and you are running '.PHP_VERSION.', please upgrade PHP or use Composer 2.2 LTS via \"composer self-update --2.2\". Aborting.'.PHP_EOL;\n");
    out.push_str("    if (!ini_get('display_errors')) {\n");
    out.push_str("        if (PHP_SAPI === 'cli' || PHP_SAPI === 'phpdbg') {\n");
    out.push_str("            fwrite(STDERR, $err);\n");
    out.push_str("        } elseif (!headers_sent()) {\n");
    out.push_str("            echo $err;\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    throw new RuntimeException($err);\n");
    out.push_str("}\n\n");
    out.push_str("require_once __DIR__ . '/composer/autoload_real.php';\n\n");
    out.push_str(&format!(
        "return ComposerAutoloaderInit{suffix}::getLoader();\n"
    ));
    out
}

/// Generate `vendor/composer/installed.php`.
fn generate_installed_php(
    root_name: &str,
    root_type: &str,
    installed: &InstalledPackages,
    dev_mode: bool,
) -> String {
    let dev_str = if dev_mode { "true" } else { "false" };

    let mut out = String::new();
    out.push_str("<?php return array(\n");
    out.push_str("    'root' => array(\n");
    out.push_str(&format!("        'name' => '{}',\n", php_escape(root_name)));
    out.push_str("        'pretty_version' => 'dev-main',\n");
    out.push_str("        'version' => 'dev-main',\n");
    out.push_str("        'reference' => null,\n");
    out.push_str(&format!("        'type' => '{}',\n", php_escape(root_type)));
    out.push_str("        'install_path' => __DIR__ . '/../../',\n");
    out.push_str("        'aliases' => array(),\n");
    out.push_str(&format!("        'dev' => {dev_str},\n"));
    out.push_str("    ),\n");
    out.push_str("    'versions' => array(\n");

    for pkg in &installed.packages {
        let version = &pkg.version;
        let version_normalized = pkg.version_normalized.as_deref().unwrap_or(version);
        let pkg_type = pkg.package_type.as_deref().unwrap_or("library");
        let is_dev = installed
            .dev_package_names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&pkg.name));
        let is_dev_str = if is_dev { "true" } else { "false" };

        out.push_str(&format!("        '{}' => array(\n", php_escape(&pkg.name)));
        out.push_str(&format!(
            "            'pretty_version' => '{}',\n",
            php_escape(version)
        ));
        out.push_str(&format!(
            "            'version' => '{}',\n",
            php_escape(version_normalized)
        ));
        out.push_str("            'reference' => null,\n");
        out.push_str(&format!(
            "            'type' => '{}',\n",
            php_escape(pkg_type)
        ));
        // Install path relative to vendor/composer/installed.php: __DIR__ . '/./' . relative_name
        // The install_path stored is like '../psr/log', relative to vendor/composer/
        // So from vendor/composer/, the package is at __DIR__ . '/../psr/log/'
        out.push_str(&format!(
            "            'install_path' => __DIR__ . '/../{}/',\n",
            pkg.name
        ));
        out.push_str("            'aliases' => array(),\n");
        out.push_str(&format!("            'dev_requirement' => {is_dev_str},\n"));
        out.push_str("        ),\n");
    }

    out.push_str("    ),\n");
    out.push_str(");\n");
    out
}

/// Determine the autoloader suffix.
///
/// Priority:
/// 1. Existing `vendor/autoload.php` suffix (carry over to avoid breaking existing references).
/// 2. Lock file `content-hash` (if locked).
/// 3. Fall back to a timestamp-based hex string.
pub fn determine_suffix(working_dir: &Path, vendor_dir: &Path) -> anyhow::Result<String> {
    // Try existing autoload.php
    let autoload_path = vendor_dir.join("autoload.php");
    if autoload_path.exists() {
        let content = std::fs::read_to_string(&autoload_path)?;
        if let Some(start) = content.find("ComposerAutoloaderInit") {
            let rest = &content[start + "ComposerAutoloaderInit".len()..];
            if let Some(end) = rest.find("::") {
                let suffix = &rest[..end];
                if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Ok(suffix.to_string());
                }
            }
        }
    }

    // Try composer.lock content-hash
    let lock_path = working_dir.join("composer.lock");
    if lock_path.exists() {
        let lock = crate::lockfile::LockFile::read_from_file(&lock_path)?;
        return Ok(lock.content_hash);
    }

    // Fall back to MD5 of current timestamp
    let ts = format!("{:?}", std::time::SystemTime::now());
    Ok(format!("{:x}", md5::compute(ts.as_bytes())))
}

/// Generate all autoloader files for the given project.
///
/// This is the main entry point called by `install` and `dump-autoload`.
pub fn generate(config: &AutoloadConfig) -> anyhow::Result<()> {
    // 1. Read installed.json
    let installed = InstalledPackages::read(&config.vendor_dir)?;

    // 2. Read root package autoload from composer.json
    let composer_json_path = config.project_dir.join("composer.json");
    let (root_autoload, root_autoload_dev, root_name, root_type) = if composer_json_path.exists() {
        let content = std::fs::read_to_string(&composer_json_path)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        (
            value.get("autoload").cloned(),
            value.get("autoload-dev").cloned(),
            value
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("__root__")
                .to_string(),
            value
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("project")
                .to_string(),
        )
    } else {
        (None, None, "__root__".to_string(), "project".to_string())
    };

    // 3. Collect autoload data
    let (mut data, mut static_data) = collect_autoloads(
        &installed,
        root_autoload.as_ref(),
        root_autoload_dev.as_ref(),
        &root_name,
        config.dev_mode,
    );

    // 3a. Read classmap dirs declared in composer.json
    let excluded: Vec<String> = root_autoload
        .as_ref()
        .and_then(|v| v.get("exclude-from-classmap"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Scan explicit classmap dirs from all packages
    let mut classmap_dirs: Vec<PathBuf> = Vec::new();

    // Collect classmap dirs from installed packages
    for pkg in &installed.packages {
        if let Some(autoload_val) = &pkg.autoload
            && let Some(cm_arr) = autoload_val.get("classmap").and_then(|v| v.as_array())
        {
            for cm_val in cm_arr {
                if let Some(cm_path) = cm_val.as_str() {
                    let abs = config.vendor_dir.join(&pkg.name).join(cm_path);
                    classmap_dirs.push(abs);
                }
            }
        }
    }

    // Collect classmap dirs from root autoload
    if let Some(autoload_val) = root_autoload.as_ref()
        && let Some(cm_arr) = autoload_val.get("classmap").and_then(|v| v.as_array())
    {
        for cm_val in cm_arr {
            if let Some(cm_path) = cm_val.as_str() {
                let abs = config.project_dir.join(cm_path);
                classmap_dirs.push(abs);
            }
        }
    }

    // Scan classmap dirs
    if !classmap_dirs.is_empty() {
        let scanned = scan_classmap_dirs(
            &classmap_dirs,
            &config.vendor_dir,
            &config.project_dir,
            &excluded,
        );
        for (class, path_expr) in scanned {
            // Also generate the static expression
            // We store the dynamic expression in data.classmap; static_data.classmap
            // will be populated similarly. For now we insert into both.
            data.classmap.entry(class.clone()).or_insert(path_expr);
            // Generate corresponding static expr by replacing dynamic prefixes
            // (static_data classmap is populated in the static pass below)
        }
    }

    // 3b. Optimize mode: scan PSR-4/PSR-0 dirs for classmap
    let do_optimize = config.optimize || config.classmap_authoritative;
    let mut psr_violations: Vec<String> = Vec::new();

    if do_optimize {
        let (opt_dyn, opt_static, violations) = scan_psr_for_classmap(
            &data.psr4,
            &data.psr0,
            &config.vendor_dir,
            &config.project_dir,
            &excluded,
        );
        psr_violations = violations;
        for (class, path_expr) in opt_dyn {
            data.classmap.entry(class).or_insert(path_expr);
        }
        for (class, path_expr) in opt_static {
            static_data.classmap.entry(class).or_insert(path_expr);
        }
    }

    // 3c. Handle strict-psr violations
    if config.strict_psr && !psr_violations.is_empty() {
        for violation in &psr_violations {
            eprintln!("PSR violation: {violation}");
        }
        return Err(anyhow::anyhow!(
            "PSR mapping violations detected (--strict-psr). Run without --strict-psr to ignore."
        ));
    }

    // 4. Generate and write files
    let composer_dir = config.vendor_dir.join("composer");
    std::fs::create_dir_all(&composer_dir)?;

    std::fs::write(
        composer_dir.join("autoload_psr4.php"),
        generate_autoload_psr4(&data),
    )?;
    std::fs::write(
        composer_dir.join("autoload_namespaces.php"),
        generate_autoload_namespaces(&data),
    )?;
    std::fs::write(
        composer_dir.join("autoload_classmap.php"),
        generate_autoload_classmap(&data),
    )?;

    if let Some(files_content) = generate_autoload_files(&data) {
        std::fs::write(composer_dir.join("autoload_files.php"), files_content)?;
    } else {
        // Remove stale file if it exists
        let files_path = composer_dir.join("autoload_files.php");
        if files_path.exists() {
            std::fs::remove_file(files_path)?;
        }
    }

    // 4a. Generate platform_check.php if needed
    let dev_package_names_set: HashSet<String> = installed
        .dev_package_names
        .iter()
        .map(|n| n.to_lowercase())
        .collect();

    // Re-read composer.json for root require (not from autoload, but from root "require" key)
    let root_require_val: Option<serde_json::Value> = if composer_json_path.exists() {
        let content = std::fs::read_to_string(&composer_json_path)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        value.get("require").cloned()
    } else {
        None
    };

    let all_locked: Vec<LockedPackage> = {
        // Collect locked packages from installed for platform check
        // (installed.packages are LockedPackage-compatible via InstalledPackageEntry)
        // We'll build minimal LockedPackage-like data from installed entries
        installed
            .packages
            .iter()
            .map(|p| crate::lockfile::LockedPackage {
                name: p.name.clone(),
                version: p.version.clone(),
                version_normalized: p.version_normalized.clone(),
                source: None,
                dist: None,
                require: std::collections::BTreeMap::new(),
                require_dev: std::collections::BTreeMap::new(),
                conflict: std::collections::BTreeMap::new(),
                suggest: None,
                package_type: p.package_type.clone(),
                autoload: p.autoload.clone(),
                autoload_dev: None,
                license: None,
                description: None,
                homepage: None,
                keywords: None,
                authors: None,
                support: None,
                funding: None,
                time: None,
                extra_fields: std::collections::BTreeMap::new(),
            })
            .collect()
    };

    let effective_mode = if config.ignore_platform_reqs {
        PlatformCheckMode::Disabled
    } else {
        config.platform_check.clone()
    };

    let platform_check_content = generate_platform_check(
        &all_locked,
        root_require_val.as_ref(),
        &effective_mode,
        &dev_package_names_set,
    );
    let has_platform_check = platform_check_content.is_some();

    if let Some(content) = platform_check_content {
        std::fs::write(composer_dir.join("platform_check.php"), content)?;
    } else {
        let pc_path = composer_dir.join("platform_check.php");
        if pc_path.exists() {
            std::fs::remove_file(pc_path)?;
        }
    }

    let has_files = !data.files.is_empty();
    let use_apcu = config.apcu || config.apcu_prefix.is_some();
    std::fs::write(
        composer_dir.join("autoload_static.php"),
        generate_autoload_static(&static_data, &config.suffix),
    )?;
    std::fs::write(
        composer_dir.join("autoload_real.php"),
        generate_autoload_real(
            &config.suffix,
            has_files,
            config.classmap_authoritative,
            use_apcu,
            config.apcu_prefix.as_deref(),
            has_platform_check,
        ),
    )?;
    std::fs::write(
        config.vendor_dir.join("autoload.php"),
        generate_autoload_php(&config.suffix),
    )?;

    // 5. Copy ClassLoader.php, InstalledVersions.php, LICENSE
    std::fs::write(composer_dir.join("ClassLoader.php"), CLASSLOADER_PHP)?;
    std::fs::write(
        composer_dir.join("InstalledVersions.php"),
        INSTALLED_VERSIONS_PHP,
    )?;
    std::fs::write(composer_dir.join("LICENSE"), COMPOSER_LICENSE)?;

    // 6. Generate installed.php
    std::fs::write(
        composer_dir.join("installed.php"),
        generate_installed_php(&root_name, &root_type, &installed, config.dev_mode),
    )?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installed::{InstalledPackageEntry, InstalledPackages};
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn make_installed_pkg(name: &str, version: &str) -> InstalledPackageEntry {
        InstalledPackageEntry {
            name: name.to_string(),
            version: version.to_string(),
            version_normalized: None,
            source: None,
            dist: None,
            package_type: Some("library".to_string()),
            install_path: Some(format!("../{name}")),
            autoload: None,
            aliases: vec![],
            extra_fields: BTreeMap::new(),
        }
    }

    fn make_installed_pkg_with_autoload(
        name: &str,
        version: &str,
        autoload: serde_json::Value,
    ) -> InstalledPackageEntry {
        let mut entry = make_installed_pkg(name, version);
        entry.autoload = Some(autoload);
        entry
    }

    // -------------------------------------------------------------------------
    // Helper function tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_php_escape_backslash() {
        assert_eq!(php_escape("Psr\\Log\\"), "Psr\\\\Log\\\\");
    }

    #[test]
    fn test_php_escape_quote() {
        assert_eq!(php_escape("don't"), "don\\'t");
    }

    #[test]
    fn test_php_escape_mixed() {
        assert_eq!(php_escape("A\\B'C"), "A\\\\B\\'C");
    }

    #[test]
    fn test_file_identifier_known_vector() {
        // Known test vector from Composer docs:
        // md5("symfony/polyfill-php80:bootstrap.php") = "a4a119a56e50fbb293281d9a48007e0e"
        let id = file_identifier("symfony/polyfill-php80", "bootstrap.php");
        assert_eq!(id, "a4a119a56e50fbb293281d9a48007e0e");
    }

    #[test]
    fn test_file_identifier_format() {
        let id = file_identifier("psr/log", "src/functions.php");
        // Should be 32 hex chars (MD5)
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_json_to_paths_string() {
        let v = serde_json::json!("src/");
        assert_eq!(json_to_paths(&v), vec!["src/"]);
    }

    #[test]
    fn test_json_to_paths_array() {
        let v = serde_json::json!(["src/", "lib/"]);
        assert_eq!(json_to_paths(&v), vec!["src/", "lib/"]);
    }

    #[test]
    fn test_json_to_paths_invalid() {
        let v = serde_json::json!(42);
        assert!(json_to_paths(&v).is_empty());
    }

    // -------------------------------------------------------------------------
    // collect_autoloads tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_collect_autoloads_psr4_basic() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "psr/log",
            "3.0.2",
            serde_json::json!({"psr-4": {"Psr\\Log\\": "src/"}}),
        ));

        let (data, _static_data) = collect_autoloads(&installed, None, None, "__root__", false);

        assert!(data.psr4.contains_key("Psr\\Log\\"));
        let paths = &data.psr4["Psr\\Log\\"];
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], "$vendorDir . '/psr/log/src'");
    }

    #[test]
    fn test_collect_autoloads_psr4_multiple_dirs() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "monolog/monolog",
            "3.8.0",
            serde_json::json!({"psr-4": {"Monolog\\": ["src/Monolog", "lib/"]}}),
        ));

        let (data, _static_data) = collect_autoloads(&installed, None, None, "__root__", false);

        let paths = &data.psr4["Monolog\\"];
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "$vendorDir . '/monolog/monolog/src/Monolog'");
        assert_eq!(paths[1], "$vendorDir . '/monolog/monolog/lib'");
    }

    #[test]
    fn test_collect_autoloads_files() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "symfony/polyfill-php80",
            "1.32.0",
            serde_json::json!({"files": ["bootstrap.php"]}),
        ));

        let (data, _static_data) = collect_autoloads(&installed, None, None, "__root__", false);

        // The identifier should match Composer's MD5 computation
        let expected_id = "a4a119a56e50fbb293281d9a48007e0e";
        assert!(data.files.contains_key(expected_id));
        assert_eq!(
            data.files[expected_id],
            "$vendorDir . '/symfony/polyfill-php80/bootstrap.php'"
        );
    }

    #[test]
    fn test_collect_autoloads_root_package() {
        let installed = InstalledPackages::new();
        let root_autoload = serde_json::json!({"psr-4": {"App\\": "src/"}});

        let (data, _static_data) = collect_autoloads(
            &installed,
            Some(&root_autoload),
            None,
            "myproject/app",
            false,
        );

        assert!(data.psr4.contains_key("App\\"));
        let paths = &data.psr4["App\\"];
        assert_eq!(paths[0], "$baseDir . '/src'");
    }

    #[test]
    fn test_collect_autoloads_root_autoload_dev_included_when_dev() {
        let installed = InstalledPackages::new();
        let root_autoload_dev = serde_json::json!({"psr-4": {"Tests\\": "tests/"}});

        let (data, _) = collect_autoloads(
            &installed,
            None,
            Some(&root_autoload_dev),
            "myproject/app",
            true, // dev_mode = true
        );

        assert!(data.psr4.contains_key("Tests\\"));
    }

    #[test]
    fn test_collect_autoloads_root_autoload_dev_excluded_when_no_dev() {
        let installed = InstalledPackages::new();
        let root_autoload_dev = serde_json::json!({"psr-4": {"Tests\\": "tests/"}});

        let (data, _) = collect_autoloads(
            &installed,
            None,
            Some(&root_autoload_dev),
            "myproject/app",
            false, // dev_mode = false
        );

        assert!(!data.psr4.contains_key("Tests\\"));
    }

    // -------------------------------------------------------------------------
    // generate_autoload_psr4 tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_generate_autoload_psr4_output() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "psr/log",
            "3.0.2",
            serde_json::json!({"psr-4": {"Psr\\Log\\": "src/"}}),
        ));

        let (data, _) = collect_autoloads(&installed, None, None, "__root__", false);
        let output = generate_autoload_psr4(&data);

        assert!(output.contains("<?php"));
        assert!(output.contains("autoload_psr4.php @generated by Composer"));
        assert!(output.contains("$vendorDir = dirname(__DIR__);"));
        assert!(output.contains("$baseDir = dirname($vendorDir);"));
        assert!(output.contains("'Psr\\\\Log\\\\'"));
        assert!(output.contains("$vendorDir . '/psr/log/src'"));
        assert!(output.starts_with("<?php\n"));
    }

    #[test]
    fn test_generate_autoload_psr4_empty() {
        let data = AutoloadData {
            psr4: BTreeMap::new(),
            psr0: BTreeMap::new(),
            classmap: BTreeMap::new(),
            files: BTreeMap::new(),
        };
        let output = generate_autoload_psr4(&data);
        assert!(output.contains("return array(\n);"));
    }

    #[test]
    fn test_generate_autoload_psr4_sorted_reverse() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "aaa/pkg",
            "1.0.0",
            serde_json::json!({"psr-4": {"Aaa\\": "src/"}}),
        ));
        installed.upsert(make_installed_pkg_with_autoload(
            "zzz/pkg",
            "1.0.0",
            serde_json::json!({"psr-4": {"Zzz\\": "src/"}}),
        ));

        let (data, _) = collect_autoloads(&installed, None, None, "__root__", false);
        let output = generate_autoload_psr4(&data);

        // Zzz should appear before Aaa (reverse sort)
        let zzz_pos = output.find("Zzz").unwrap();
        let aaa_pos = output.find("Aaa").unwrap();
        assert!(
            zzz_pos < aaa_pos,
            "Zzz should appear before Aaa (reverse sort)"
        );
    }

    // -------------------------------------------------------------------------
    // generate_autoload_static tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_generate_autoload_static_output() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "psr/log",
            "3.0.2",
            serde_json::json!({"psr-4": {"Psr\\Log\\": "src/"}}),
        ));

        let (_, static_data) = collect_autoloads(&installed, None, None, "__root__", false);
        let output = generate_autoload_static(&static_data, "abc123");

        assert!(output.contains("class ComposerStaticInitabc123"));
        assert!(output.contains("$prefixLengthsPsr4"));
        assert!(output.contains("$prefixDirsPsr4"));
        assert!(output.contains("$classMap"));
        assert!(output.contains("Composer\\\\InstalledVersions"));
        assert!(output.contains("getInitializer"));
        assert!(output.contains("__DIR__ . '/..' . '/psr/log/src'"));
    }

    #[test]
    fn test_generate_autoload_static_prefix_lengths() {
        let mut installed = InstalledPackages::new();
        // "Psr\Log\" = 8 bytes (with single backslashes)
        installed.upsert(make_installed_pkg_with_autoload(
            "psr/log",
            "3.0.2",
            serde_json::json!({"psr-4": {"Psr\\Log\\": "src/"}}),
        ));

        let (_, static_data) = collect_autoloads(&installed, None, None, "__root__", false);
        let output = generate_autoload_static(&static_data, "test");

        // The namespace "Psr\Log\" is 8 bytes
        assert!(output.contains("'Psr\\\\Log\\\\' => 8"));
    }

    // -------------------------------------------------------------------------
    // generate_autoload_real tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_generate_autoload_real_with_files() {
        let output = generate_autoload_real("abc123", true, false, false, None, false);
        assert!(output.contains("class ComposerAutoloaderInitabc123"));
        assert!(output.contains("ComposerStaticInitabc123::$files"));
        assert!(output.contains("$requireFile"));
        assert!(output.contains("__composer_autoload_files"));
    }

    #[test]
    fn test_generate_autoload_real_without_files() {
        let output = generate_autoload_real("abc123", false, false, false, None, false);
        assert!(output.contains("class ComposerAutoloaderInitabc123"));
        assert!(!output.contains("$filesToLoad"));
        assert!(!output.contains("__composer_autoload_files"));
    }

    #[test]
    fn test_generate_autoload_real_apcu() {
        let output = generate_autoload_real("abc123", false, false, true, None, false);
        assert!(output.contains("setApcuPrefix('abc123')"));
    }

    #[test]
    fn test_generate_autoload_real_apcu_custom_prefix() {
        let output = generate_autoload_real("abc123", false, false, true, Some("myprefix"), false);
        assert!(output.contains("setApcuPrefix('myprefix')"));
    }

    #[test]
    fn test_generate_autoload_real_platform_check() {
        let output = generate_autoload_real("abc123", false, false, false, None, true);
        assert!(output.contains("require __DIR__ . '/platform_check.php'"));
    }

    #[test]
    fn test_generate_autoload_real_no_platform_check() {
        let output = generate_autoload_real("abc123", false, false, false, None, false);
        assert!(!output.contains("platform_check.php"));
    }

    // -------------------------------------------------------------------------
    // generate_installed_php tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_generate_installed_php() {
        let mut installed = InstalledPackages::new();
        let mut pkg = make_installed_pkg("psr/log", "3.0.2");
        pkg.version_normalized = Some("3.0.2.0".to_string());
        installed.upsert(pkg);

        let output = generate_installed_php("myproject/app", "project", &installed, true);

        assert!(output.contains("'name' => 'myproject/app'"));
        assert!(output.contains("'type' => 'project'"));
        assert!(output.contains("'dev' => true"));
        assert!(output.contains("'psr/log'"));
        assert!(output.contains("'pretty_version' => '3.0.2'"));
        assert!(output.contains("'version' => '3.0.2.0'"));
        assert!(output.contains("__DIR__ . '/../psr/log/'"));
        assert!(output.contains("'dev_requirement' => false"));
    }

    #[test]
    fn test_generate_installed_php_dev_package() {
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg("phpunit/phpunit", "11.0.0"));
        installed
            .dev_package_names
            .push("phpunit/phpunit".to_string());

        let output = generate_installed_php("test/project", "project", &installed, true);

        assert!(output.contains("'dev_requirement' => true"));
    }

    // -------------------------------------------------------------------------
    // generate() integration test
    // -------------------------------------------------------------------------

    #[test]
    fn test_generate_full_roundtrip() {
        let dir = tempdir().unwrap();
        let project_dir = dir.path().to_path_buf();
        let vendor_dir = project_dir.join("vendor");

        // Write a minimal composer.json
        std::fs::write(
            project_dir.join("composer.json"),
            r#"{"name": "test/project", "type": "project", "autoload": {"psr-4": {"App\\": "src/"}}}"#,
        )
        .unwrap();

        // Write a minimal installed.json
        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "psr/log",
            "3.0.2",
            serde_json::json!({"psr-4": {"Psr\\Log\\": "src/"}}),
        ));
        installed.write(&vendor_dir).unwrap();

        let config = AutoloadConfig {
            project_dir: project_dir.clone(),
            vendor_dir: vendor_dir.clone(),
            dev_mode: false,
            suffix: "abc123def456".to_string(),
            classmap_authoritative: false,
            optimize: false,
            apcu: false,
            apcu_prefix: None,
            strict_psr: false,
            platform_check: PlatformCheckMode::Disabled,
            ignore_platform_reqs: false,
        };

        generate(&config).unwrap();

        // Verify all expected files exist
        assert!(
            vendor_dir.join("autoload.php").exists(),
            "autoload.php should exist"
        );
        assert!(
            vendor_dir.join("composer/autoload_psr4.php").exists(),
            "autoload_psr4.php should exist"
        );
        assert!(
            vendor_dir.join("composer/autoload_namespaces.php").exists(),
            "autoload_namespaces.php should exist"
        );
        assert!(
            vendor_dir.join("composer/autoload_classmap.php").exists(),
            "autoload_classmap.php should exist"
        );
        assert!(
            vendor_dir.join("composer/autoload_static.php").exists(),
            "autoload_static.php should exist"
        );
        assert!(
            vendor_dir.join("composer/autoload_real.php").exists(),
            "autoload_real.php should exist"
        );
        assert!(
            vendor_dir.join("composer/ClassLoader.php").exists(),
            "ClassLoader.php should exist"
        );
        assert!(
            vendor_dir.join("composer/InstalledVersions.php").exists(),
            "InstalledVersions.php should exist"
        );
        assert!(
            vendor_dir.join("composer/installed.php").exists(),
            "installed.php should exist"
        );
        assert!(
            vendor_dir.join("composer/LICENSE").exists(),
            "LICENSE should exist"
        );
        // autoload_files.php should NOT exist (no files autoloading)
        assert!(
            !vendor_dir.join("composer/autoload_files.php").exists(),
            "autoload_files.php should not exist when no files"
        );

        // Check autoload.php content
        let autoload_php = std::fs::read_to_string(vendor_dir.join("autoload.php")).unwrap();
        assert!(autoload_php.contains("ComposerAutoloaderInitabc123def456"));

        // Check autoload_psr4.php
        let psr4_php =
            std::fs::read_to_string(vendor_dir.join("composer/autoload_psr4.php")).unwrap();
        assert!(psr4_php.contains("Psr\\\\Log\\\\"));
        assert!(psr4_php.contains("App\\\\"));
        assert!(psr4_php.contains("$vendorDir . '/psr/log/src'"));
        assert!(psr4_php.contains("$baseDir . '/src'"));

        // Check installed.php
        let installed_php =
            std::fs::read_to_string(vendor_dir.join("composer/installed.php")).unwrap();
        assert!(installed_php.contains("'name' => 'test/project'"));
        assert!(installed_php.contains("'psr/log'"));
    }

    #[test]
    fn test_generate_with_files_autoload() {
        let dir = tempdir().unwrap();
        let project_dir = dir.path().to_path_buf();
        let vendor_dir = project_dir.join("vendor");

        std::fs::write(
            project_dir.join("composer.json"),
            r#"{"name": "test/project", "type": "project"}"#,
        )
        .unwrap();

        let mut installed = InstalledPackages::new();
        installed.upsert(make_installed_pkg_with_autoload(
            "symfony/polyfill-php80",
            "1.32.0",
            serde_json::json!({"files": ["bootstrap.php"]}),
        ));
        installed.write(&vendor_dir).unwrap();

        let config = AutoloadConfig {
            project_dir: project_dir.clone(),
            vendor_dir: vendor_dir.clone(),
            dev_mode: false,
            suffix: "test".to_string(),
            classmap_authoritative: false,
            optimize: false,
            apcu: false,
            apcu_prefix: None,
            strict_psr: false,
            platform_check: PlatformCheckMode::Disabled,
            ignore_platform_reqs: false,
        };

        generate(&config).unwrap();

        // autoload_files.php SHOULD exist
        assert!(
            vendor_dir.join("composer/autoload_files.php").exists(),
            "autoload_files.php should exist when files are present"
        );

        let files_php =
            std::fs::read_to_string(vendor_dir.join("composer/autoload_files.php")).unwrap();
        assert!(files_php.contains("a4a119a56e50fbb293281d9a48007e0e"));
        assert!(files_php.contains("$vendorDir . '/symfony/polyfill-php80/bootstrap.php'"));

        // autoload_real.php should contain the files loading block
        let real_php =
            std::fs::read_to_string(vendor_dir.join("composer/autoload_real.php")).unwrap();
        assert!(real_php.contains("$filesToLoad"));
    }
}
