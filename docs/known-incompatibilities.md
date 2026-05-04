# Known Incompatibilities

NOTE: This is not an exhaustive list. Mozart is in early development and there are still a number of significant incompatibilities with Composer that are not documented here yet.


## TLS / CA certificate discovery

Composer relies on the [`composer/ca-bundle`](https://github.com/composer/ca-bundle) package to locate a usable CA bundle for HTTPS verification. It probes a number of well-known paths (`/etc/ssl/certs/...`, Homebrew, Cygwin, etc.), inspects PHP ini settings (`openssl.cafile`, `openssl.capath`), and ships its own `cacert.pem` as a fallback when nothing else is found.

Mozart performs HTTPS through the OS-native TLS stack (OpenSSL on Linux, Secure Transport on macOS, SChannel on Windows), which already knows where the system trust store lives. As a result Mozart does not ship a bundled `cacert.pem` and does not implement `composer/ca-bundle`'s manual probing.

In typical environments this does not matter; both Composer and Mozart trust the same system CAs. Some edge cases where the trusted certificate set may differ:

- No system trust store: Composer still works because it falls back to its bundled `cacert.pem`; Mozart does not.
- PHP-only ini overrides: `openssl.cafile` / `openssl.capath` configured via `php.ini` affect Composer but have no effect on Mozart.

If you rely on a private CA, set `config.cafile` and `config.capath` in `composer.json` (or the global `$COMPOSER_HOME/config.json`). It works in Mozart too.
