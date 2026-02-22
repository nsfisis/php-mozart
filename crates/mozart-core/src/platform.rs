// Shared platform detection module.
//
// Provides detection of the PHP environment (version, extensions, capabilities)
// and helpers for identifying platform package names (php, ext-*, lib-*, etc.).

// ─── Data structures ─────────────────────────────────────────────────────────

/// A detected platform package with its name and version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPackage {
    pub name: String,
    pub version: String,
}

// ─── Classification ──────────────────────────────────────────────────────────

/// Returns true if the package name is a Composer platform package.
///
/// Platform packages include: php, php-*, ext-*, lib-*, composer,
/// composer-plugin-api, composer-runtime-api.
pub fn is_platform_package(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "php"
        || lower.starts_with("php-")
        || lower.starts_with("ext-")
        || lower.starts_with("lib-")
        || lower == "composer"
        || lower == "composer-plugin-api"
        || lower == "composer-runtime-api"
}

// ─── Detection ───────────────────────────────────────────────────────────────

/// Composer runtime API version that Mozart emulates.
/// Corresponds to `Composer::RUNTIME_API_VERSION` in Composer.
pub const COMPOSER_RUNTIME_API_VERSION: &str = "2.2.2";

/// Composer plugin API version that Mozart emulates.
/// Corresponds to `PluginInterface::PLUGIN_API_VERSION` in Composer.
pub const COMPOSER_PLUGIN_API_VERSION: &str = "2.6.0";

/// Composer version that Mozart emulates.
pub const COMPOSER_VERSION: &str = "2.8.0";

/// Detect all platform packages by running a single PHP invocation.
///
/// Returns an empty vec if PHP is not found or not executable.
pub fn detect_platform() -> Vec<PlatformPackage> {
    let php_script = concat!(
        "echo 'PHP_VERSION:' . PHP_VERSION . PHP_EOL;",
        "echo 'PHP_INT_SIZE:' . PHP_INT_SIZE . PHP_EOL;",
        "echo 'PHP_DEBUG:' . (PHP_DEBUG ? '1' : '0') . PHP_EOL;",
        "echo 'PHP_ZTS:' . (defined('PHP_ZTS') && PHP_ZTS ? '1' : '0') . PHP_EOL;",
        "echo 'IPV6:' . ((defined('AF_INET6') || @inet_pton('::') !== false) ? '1' : '0') . PHP_EOL;",
        "echo 'EXTENSIONS:' . PHP_EOL;",
        "foreach(get_loaded_extensions() as $e) { echo $e . ':' . (phpversion($e) ?: '0') . PHP_EOL; }",
        // lib-* detection
        "echo 'LIBS:' . PHP_EOL;",
        // lib-pcre: strip trailing text (e.g. "10.42 2023-01-15" → "10.42")
        "if (defined('PCRE_VERSION')) { $v = preg_replace('/^(\\S+).*/', '$1', PCRE_VERSION); echo 'LIB:pcre:' . $v . PHP_EOL; }",
        // lib-pcre-unicode: same version if unicode classes are supported
        "if (defined('PCRE_VERSION') && @preg_match('/\\pL/u', 'a') === 1) { $v = preg_replace('/^(\\S+).*/', '$1', PCRE_VERSION); echo 'LIB:pcre-unicode:' . $v . PHP_EOL; }",
        // lib-openssl: parse version from OPENSSL_VERSION_TEXT (e.g. "OpenSSL 3.0.2 15 Mar 2022")
        "if (defined('OPENSSL_VERSION_TEXT') && preg_match('/(\\d+\\.\\d+\\.\\d+[a-z]?)/', OPENSSL_VERSION_TEXT, $m)) { echo 'LIB:openssl:' . $m[1] . PHP_EOL; }",
        // lib-curl, lib-curl-openssl, lib-curl-zlib
        "if (function_exists('curl_version')) { $c = curl_version();",
        "  echo 'LIB:curl:' . $c['version'] . PHP_EOL;",
        "  if (!empty($c['ssl_version']) && preg_match('/(\\d+\\.\\d+\\.\\d+[a-z]?)/', $c['ssl_version'], $m)) { echo 'LIB:curl-openssl:' . $m[1] . PHP_EOL; }",
        "  if (!empty($c['libz_version'])) { echo 'LIB:curl-zlib:' . $c['libz_version'] . PHP_EOL; }",
        "}",
        // lib-libxml
        "if (defined('LIBXML_DOTTED_VERSION')) { echo 'LIB:libxml:' . LIBXML_DOTTED_VERSION . PHP_EOL; }",
        // lib-icu
        "if (defined('INTL_ICU_VERSION')) { echo 'LIB:icu:' . INTL_ICU_VERSION . PHP_EOL; }",
        // lib-zlib
        "if (defined('ZLIB_VERSION')) { echo 'LIB:zlib:' . ZLIB_VERSION . PHP_EOL; }",
        // lib-iconv
        "if (defined('ICONV_VERSION')) { echo 'LIB:iconv:' . ICONV_VERSION . PHP_EOL; }",
        // lib-gd
        "if (defined('GD_VERSION')) { echo 'LIB:gd:' . GD_VERSION . PHP_EOL; }",
        // lib-gmp
        "if (defined('GMP_VERSION')) { echo 'LIB:gmp:' . GMP_VERSION . PHP_EOL; }",
        // lib-libsodium
        "if (defined('SODIUM_LIBRARY_VERSION')) { echo 'LIB:libsodium:' . SODIUM_LIBRARY_VERSION . PHP_EOL; }",
        // lib-sqlite3-sqlite
        "if (class_exists('SQLite3')) { $sv = SQLite3::version(); echo 'LIB:sqlite3-sqlite:' . $sv['versionString'] . PHP_EOL; }",
    );

    let output = match std::process::Command::new("php")
        .arg("-r")
        .arg(php_script)
        .output()
    {
        Ok(o) => o,
        Err(_) => return vec![],
    };

    if !output.status.success() {
        return vec![];
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_platform_info(&stdout)
}

/// Parse the output of the PHP platform detection script.
///
/// Exposed for testing purposes.
pub fn parse_platform_info(output: &str) -> Vec<PlatformPackage> {
    let mut packages: Vec<PlatformPackage> = Vec::new();

    let mut php_version = String::new();
    let mut int_size: u8 = 0;
    let mut php_debug = false;
    let mut php_zts = false;
    let mut php_ipv6 = false;
    let mut in_extensions = false;
    let mut lib_packages: Vec<PlatformPackage> = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(v) = line.strip_prefix("PHP_VERSION:") {
            php_version = v.to_string();
            continue;
        }
        if let Some(v) = line.strip_prefix("PHP_INT_SIZE:") {
            int_size = v.parse().unwrap_or(0);
            continue;
        }
        if let Some(v) = line.strip_prefix("PHP_DEBUG:") {
            php_debug = v == "1";
            continue;
        }
        if let Some(v) = line.strip_prefix("PHP_ZTS:") {
            php_zts = v == "1";
            continue;
        }
        if let Some(v) = line.strip_prefix("IPV6:") {
            php_ipv6 = v == "1";
            continue;
        }
        if line == "EXTENSIONS:" {
            in_extensions = true;
            continue;
        }
        if line == "LIBS:" {
            in_extensions = false;
            continue;
        }

        // Format: LIB:name:version
        if let Some(rest) = line.strip_prefix("LIB:") {
            if let Some(colon_pos) = rest.find(':') {
                let lib_name = rest[..colon_pos].trim();
                let lib_version = rest[colon_pos + 1..].trim();
                if !lib_name.is_empty() && !lib_version.is_empty() {
                    lib_packages.push(PlatformPackage {
                        name: format!("lib-{lib_name}"),
                        version: lib_version.to_string(),
                    });
                }
            }
            continue;
        }

        if in_extensions {
            // Format: ExtensionName:version
            if let Some(colon_pos) = line.find(':') {
                let ext_name = line[..colon_pos].trim().to_lowercase();
                let ext_version = line[colon_pos + 1..].trim();
                // Normalize: if version is "0", "false", or empty, use the PHP version
                let version =
                    if ext_version.is_empty() || ext_version == "0" || ext_version == "false" {
                        if php_version.is_empty() {
                            "0.0.0".to_string()
                        } else {
                            php_version.clone()
                        }
                    } else {
                        ext_version.to_string()
                    };
                packages.push(PlatformPackage {
                    name: format!("ext-{ext_name}"),
                    version,
                });
            }
        }
    }

    // Build the base php entry first (so it's easy to find)
    if !php_version.is_empty() {
        let mut result: Vec<PlatformPackage> = Vec::new();

        result.push(PlatformPackage {
            name: "php".to_string(),
            version: php_version.clone(),
        });

        if int_size == 8 {
            result.push(PlatformPackage {
                name: "php-64bit".to_string(),
                version: php_version.clone(),
            });
        }

        if php_debug {
            result.push(PlatformPackage {
                name: "php-debug".to_string(),
                version: php_version.clone(),
            });
        }

        if php_zts {
            result.push(PlatformPackage {
                name: "php-zts".to_string(),
                version: php_version.clone(),
            });
        }

        if php_ipv6 {
            result.push(PlatformPackage {
                name: "php-ipv6".to_string(),
                version: php_version.clone(),
            });
        }

        result.extend(packages);
        result.extend(lib_packages);

        // Add Composer pseudo packages
        result.push(PlatformPackage {
            name: "composer".to_string(),
            version: COMPOSER_VERSION.to_string(),
        });
        result.push(PlatformPackage {
            name: "composer-plugin-api".to_string(),
            version: COMPOSER_PLUGIN_API_VERSION.to_string(),
        });
        result.push(PlatformPackage {
            name: "composer-runtime-api".to_string(),
            version: COMPOSER_RUNTIME_API_VERSION.to_string(),
        });

        result
    } else {
        // Even without PHP, provide lib and Composer pseudo packages
        packages.extend(lib_packages);
        packages.push(PlatformPackage {
            name: "composer".to_string(),
            version: COMPOSER_VERSION.to_string(),
        });
        packages.push(PlatformPackage {
            name: "composer-plugin-api".to_string(),
            version: COMPOSER_PLUGIN_API_VERSION.to_string(),
        });
        packages.push(PlatformPackage {
            name: "composer-runtime-api".to_string(),
            version: COMPOSER_RUNTIME_API_VERSION.to_string(),
        });
        packages
    }
}

/// Try to detect the installed PHP version by running `php --version`.
pub fn detect_php_version() -> Option<String> {
    let output = std::process::Command::new("php")
        .arg("--version")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse "PHP 8.2.1 (cli) ..." → "8.2.1"
    let first_line = stdout.lines().next()?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() >= 2 && parts[0] == "PHP" {
        Some(parts[1].to_string())
    } else {
        None
    }
}

/// Try to detect PHP extensions by running `php -m`.
pub fn detect_php_extensions() -> Vec<String> {
    let output = match std::process::Command::new("php").arg("-m").output() {
        Ok(o) => o,
        Err(_) => return vec![],
    };

    if !output.status.success() {
        return vec![];
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|line| {
            let l = line.trim();
            !l.is_empty()
                && !l.starts_with('[')
                && l.chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        })
        .map(|l| l.trim().to_lowercase())
        .collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_platform_package_php() {
        assert!(is_platform_package("php"));
        assert!(is_platform_package("PHP"));
    }

    #[test]
    fn test_is_platform_package_php_variants() {
        assert!(is_platform_package("php-64bit"));
        assert!(is_platform_package("php-debug"));
        assert!(is_platform_package("php-zts"));
        assert!(is_platform_package("php-ipv6"));
    }

    #[test]
    fn test_is_platform_package_ext() {
        assert!(is_platform_package("ext-json"));
        assert!(is_platform_package("ext-mbstring"));
        assert!(is_platform_package("ext-ctype"));
    }

    #[test]
    fn test_is_platform_package_lib() {
        assert!(is_platform_package("lib-pcre"));
        assert!(is_platform_package("lib-curl"));
    }

    #[test]
    fn test_is_platform_package_composer() {
        assert!(is_platform_package("composer"));
        assert!(is_platform_package("composer-plugin-api"));
        assert!(is_platform_package("composer-runtime-api"));
    }

    #[test]
    fn test_is_platform_package_not_platform() {
        assert!(!is_platform_package("monolog/monolog"));
        assert!(!is_platform_package("psr/log"));
        assert!(!is_platform_package("symfony/console"));
        assert!(!is_platform_package("vendor/package"));
    }

    #[test]
    fn test_parse_platform_info_basic() {
        let output = "PHP_VERSION:8.2.1\nPHP_INT_SIZE:8\nPHP_DEBUG:0\nPHP_ZTS:0\nIPV6:1\nEXTENSIONS:\njson:8.2.1\nctype:8.2.1\n";
        let packages = parse_platform_info(output);

        let php = packages.iter().find(|p| p.name == "php");
        assert!(php.is_some());
        assert_eq!(php.unwrap().version, "8.2.1");

        let php64 = packages.iter().find(|p| p.name == "php-64bit");
        assert!(php64.is_some(), "PHP_INT_SIZE=8 should produce php-64bit");

        let ipv6 = packages.iter().find(|p| p.name == "php-ipv6");
        assert!(ipv6.is_some());

        let ext_json = packages.iter().find(|p| p.name == "ext-json");
        assert!(ext_json.is_some());
        assert_eq!(ext_json.unwrap().version, "8.2.1");

        let ext_ctype = packages.iter().find(|p| p.name == "ext-ctype");
        assert!(ext_ctype.is_some());

        // Composer pseudo packages should always be present
        assert!(packages.iter().any(|p| p.name == "composer"));
        assert!(packages.iter().any(|p| p.name == "composer-plugin-api"));
        assert!(packages.iter().any(|p| p.name == "composer-runtime-api"));
    }

    #[test]
    fn test_parse_platform_info_no_debug_no_zts() {
        let output =
            "PHP_VERSION:8.1.0\nPHP_INT_SIZE:4\nPHP_DEBUG:0\nPHP_ZTS:0\nIPV6:0\nEXTENSIONS:\n";
        let packages = parse_platform_info(output);

        assert!(packages.iter().any(|p| p.name == "php"));
        assert!(!packages.iter().any(|p| p.name == "php-64bit"));
        assert!(!packages.iter().any(|p| p.name == "php-debug"));
        assert!(!packages.iter().any(|p| p.name == "php-zts"));
        assert!(!packages.iter().any(|p| p.name == "php-ipv6"));
    }

    #[test]
    fn test_parse_platform_info_debug_and_zts() {
        let output =
            "PHP_VERSION:8.3.0\nPHP_INT_SIZE:8\nPHP_DEBUG:1\nPHP_ZTS:1\nIPV6:0\nEXTENSIONS:\n";
        let packages = parse_platform_info(output);

        assert!(packages.iter().any(|p| p.name == "php-debug"));
        assert!(packages.iter().any(|p| p.name == "php-zts"));
    }

    #[test]
    fn test_parse_platform_info_extension_version_zero() {
        // Extensions returning version "0" should fall back to PHP version
        let output = "PHP_VERSION:8.2.5\nPHP_INT_SIZE:8\nPHP_DEBUG:0\nPHP_ZTS:0\nIPV6:0\nEXTENSIONS:\nCore:0\n";
        let packages = parse_platform_info(output);

        let ext_core = packages.iter().find(|p| p.name == "ext-core");
        assert!(ext_core.is_some());
        assert_eq!(
            ext_core.unwrap().version,
            "8.2.5",
            "version '0' should fall back to PHP version"
        );
    }

    #[test]
    fn test_parse_platform_info_no_php() {
        // If PHP_VERSION is missing, only extensions are returned
        let output = "EXTENSIONS:\njson:1.7\n";
        let packages = parse_platform_info(output);

        assert!(!packages.iter().any(|p| p.name == "php"));
        assert!(packages.iter().any(|p| p.name == "ext-json"));
    }

    #[test]
    fn test_parse_platform_info_lib_packages() {
        let output = "\
PHP_VERSION:8.2.1
PHP_INT_SIZE:8
PHP_DEBUG:0
PHP_ZTS:0
IPV6:1
EXTENSIONS:
json:8.2.1
LIBS:
LIB:pcre:10.42
LIB:pcre-unicode:10.42
LIB:openssl:3.0.2
LIB:curl:7.81.0
LIB:curl-openssl:3.0.2
LIB:curl-zlib:1.2.11
LIB:libxml:2.9.14
LIB:icu:70.1
LIB:zlib:1.2.11
LIB:iconv:2.35
LIB:gd:2.3.3
LIB:gmp:6.2.1
LIB:libsodium:1.0.18
LIB:sqlite3-sqlite:3.37.2
";
        let packages = parse_platform_info(output);

        let lib_pcre = packages.iter().find(|p| p.name == "lib-pcre");
        assert!(lib_pcre.is_some(), "lib-pcre should be detected");
        assert_eq!(lib_pcre.unwrap().version, "10.42");

        let lib_openssl = packages.iter().find(|p| p.name == "lib-openssl");
        assert!(lib_openssl.is_some(), "lib-openssl should be detected");
        assert_eq!(lib_openssl.unwrap().version, "3.0.2");

        let lib_curl = packages.iter().find(|p| p.name == "lib-curl");
        assert!(lib_curl.is_some(), "lib-curl should be detected");
        assert_eq!(lib_curl.unwrap().version, "7.81.0");

        let lib_libxml = packages.iter().find(|p| p.name == "lib-libxml");
        assert!(lib_libxml.is_some());
        assert_eq!(lib_libxml.unwrap().version, "2.9.14");

        let lib_icu = packages.iter().find(|p| p.name == "lib-icu");
        assert!(lib_icu.is_some());
        assert_eq!(lib_icu.unwrap().version, "70.1");

        let lib_pcre_unicode = packages.iter().find(|p| p.name == "lib-pcre-unicode");
        assert!(lib_pcre_unicode.is_some());

        let lib_curl_openssl = packages.iter().find(|p| p.name == "lib-curl-openssl");
        assert!(lib_curl_openssl.is_some());
        assert_eq!(lib_curl_openssl.unwrap().version, "3.0.2");

        let lib_sqlite = packages.iter().find(|p| p.name == "lib-sqlite3-sqlite");
        assert!(lib_sqlite.is_some());
        assert_eq!(lib_sqlite.unwrap().version, "3.37.2");
    }

    #[test]
    fn test_parse_platform_info_lib_packages_no_php() {
        // lib-* packages should still be detected even without PHP_VERSION
        let output = "EXTENSIONS:\nLIBS:\nLIB:pcre:10.40\nLIB:libxml:2.9.12\n";
        let packages = parse_platform_info(output);

        assert!(!packages.iter().any(|p| p.name == "php"));
        assert!(packages.iter().any(|p| p.name == "lib-pcre"));
        assert!(packages.iter().any(|p| p.name == "lib-libxml"));
    }

    #[test]
    fn test_parse_platform_info_lib_empty_version_skipped() {
        let output = "PHP_VERSION:8.2.0\nPHP_INT_SIZE:8\nPHP_DEBUG:0\nPHP_ZTS:0\nIPV6:0\nEXTENSIONS:\nLIBS:\nLIB:pcre:\nLIB::1.0\n";
        let packages = parse_platform_info(output);

        // Empty version or empty name lines should be skipped
        assert!(!packages.iter().any(|p| p.name == "lib-pcre"));
        assert!(!packages.iter().any(|p| p.name == "lib-"));
    }

    #[test]
    fn test_parse_platform_info_extension_names_lowercased() {
        let output = "PHP_VERSION:8.0.0\nPHP_INT_SIZE:8\nPHP_DEBUG:0\nPHP_ZTS:0\nIPV6:0\nEXTENSIONS:\nJSON:8.0.0\nMbstring:8.0.0\n";
        let packages = parse_platform_info(output);

        assert!(packages.iter().any(|p| p.name == "ext-json"));
        assert!(packages.iter().any(|p| p.name == "ext-mbstring"));
    }
}
