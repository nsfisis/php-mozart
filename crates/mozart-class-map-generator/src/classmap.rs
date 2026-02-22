use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Recursively collect PHP files from a directory, skipping excluded paths.
pub fn collect_php_files(
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
pub fn is_excluded(
    path: &Path,
    excluded: &[String],
    vendor_dir: &Path,
    project_dir: &Path,
) -> bool {
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
pub fn scan_classmap_dirs(
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
pub fn path_to_php_expr(file: &Path, vendor_dir: &Path, project_dir: &Path) -> String {
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
pub fn path_to_static_expr(file: &Path, vendor_dir: &Path, project_dir: &Path) -> String {
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
pub fn scan_psr_for_classmap(
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
