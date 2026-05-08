use crate::MOZART_VERSION;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};

/// Returns the common User-Agent string for all HTTP requests.
///
/// Format: `Mozart/<version> (<os>; <arch>)`
pub fn user_agent() -> String {
    format!(
        "Mozart/{} ({}; {})",
        MOZART_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// TLS verification options, mirroring Composer's `config.cafile` and
/// `config.capath`.
#[derive(Debug, Default, Clone)]
pub struct TlsOptions {
    pub cafile: Option<PathBuf>,
    pub capath: Option<PathBuf>,
}

/// Pre-parsed root certificates, loaded once from `cafile`/`capath` and shared
/// across every reqwest client built via [`client_builder`].
static EXTRA_ROOT_CERTS: OnceLock<Vec<reqwest::Certificate>> = OnceLock::new();

/// Initialize the process-wide TLS options.
///
/// Reads `cafile` and `capath` (if set), parses every certificate up-front,
/// and stores the parsed [`reqwest::Certificate`] list in a global so that
/// subsequent [`client_builder`] calls are infallible.
///
/// May be called at most once; subsequent calls are silently ignored. This
/// matches the lifetime of the binary's HTTP configuration: load on startup,
/// reuse for the rest of the process.
pub fn init_tls_options(opts: &TlsOptions) -> Result<()> {
    if EXTRA_ROOT_CERTS.get().is_some() {
        return Ok(());
    }
    let mut certs = Vec::new();
    if let Some(ref cafile) = opts.cafile {
        certs.extend(load_cafile(cafile)?);
    }
    if let Some(ref capath) = opts.capath {
        certs.extend(load_capath(capath)?);
    }
    let _ = EXTRA_ROOT_CERTS.set(certs);
    Ok(())
}

fn load_cafile(path: &Path) -> Result<Vec<reqwest::Certificate>> {
    let pem = std::fs::read(path).with_context(|| {
        format!(
            "The configured cafile {} could not be read.",
            path.display()
        )
    })?;
    let certs = reqwest::Certificate::from_pem_bundle(&pem)
        .with_context(|| format!("The configured cafile {} was not valid.", path.display()))?;
    if certs.is_empty() {
        bail!(
            "The configured cafile {} did not contain any certificates.",
            path.display()
        );
    }
    Ok(certs)
}

fn load_capath(path: &Path) -> Result<Vec<reqwest::Certificate>> {
    let metadata = std::fs::metadata(path).with_context(|| {
        format!(
            "The configured capath {} could not be accessed.",
            path.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(anyhow!(
            "The configured capath {} is not a directory.",
            path.display()
        ));
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(path).with_context(|| {
        format!(
            "The configured capath {} could not be read.",
            path.display()
        )
    })?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("Failed to enumerate capath {}", path.display()))?;
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        let Ok(pem) = std::fs::read(&entry_path) else {
            continue;
        };
        match reqwest::Certificate::from_pem_bundle(&pem) {
            Ok(parsed) => out.extend(parsed),
            Err(e) => {
                tracing::debug!(
                    path = %entry_path.display(),
                    error = %e,
                    "skipping non-PEM file in capath"
                );
            }
        }
    }
    Ok(out)
}

/// Returns a [`reqwest::ClientBuilder`] preconfigured with Mozart's User-Agent
/// and any extra root certificates registered via [`init_tls_options`].
pub fn client_builder() -> reqwest::ClientBuilder {
    let mut b = reqwest::Client::builder().user_agent(user_agent());
    if let Some(certs) = EXTRA_ROOT_CERTS.get() {
        for cert in certs {
            b = b.add_root_certificate(cert.clone());
        }
    }
    b
}

/// Build a default [`reqwest::Client`] with Mozart's User-Agent and any
/// configured root certificates. Panics on build failure, matching
/// [`reqwest::Client::new`] semantics.
pub fn default_client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("failed to build default HTTP client")
}

/// Thin wrapper around [`reqwest::Client`] that mirrors the relevant slice of
/// `Composer\Util\HttpDownloader`: a project-shared client used for plain
/// `GET` requests against package metadata URLs.
///
/// Today this is only the bits the `diagnose` command needs (a pre-built
/// client, a single `get` method, and `exception_hints`). The intention is
/// for `mozart-registry`'s download pipeline to migrate onto the same
/// wrapper later.
#[derive(Clone)]
pub struct HttpDownloader {
    client: reqwest::Client,
}

impl HttpDownloader {
    /// Build a downloader using the standard Mozart client (User-Agent +
    /// configured root certificates).
    pub fn new() -> Self {
        Self {
            client: default_client(),
        }
    }

    /// Build a downloader with a custom timeout, used by health checks where
    /// hangs would mask the failure mode the user is actually trying to
    /// diagnose.
    pub fn with_timeout(timeout: std::time::Duration) -> Result<Self> {
        let client = client_builder()
            .timeout(timeout)
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { client })
    }

    /// Issue a `GET` against `url`. Mirrors `HttpDownloader::get` in role,
    /// but returns the raw [`reqwest::Response`] so callers can decide
    /// what to do with the body.
    pub async fn get(&self, url: &str) -> Result<reqwest::Response, reqwest::Error> {
        self.client.get(url).send().await
    }

    /// Underlying client, exposed so callers that need to set additional
    /// request-level options can build off it. Try not to use this from
    /// new code — prefer extending `HttpDownloader` itself.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

impl Default for HttpDownloader {
    fn default() -> Self {
        Self::new()
    }
}

/// Mirror of `HttpDownloader::getExceptionHints` from PHP — best-effort
/// human-readable hints for a transport failure. Today this only surfaces
/// the few cases reqwest can distinguish (timeout, connect, decode); we
/// can extend it as we encounter more failure modes in the wild.
pub fn exception_hints(err: &reqwest::Error) -> Vec<String> {
    let mut hints = Vec::new();
    if err.is_timeout() {
        hints.push(
            "The request timed out. Check your network connection or any HTTP proxy settings."
                .to_string(),
        );
    }
    if err.is_connect() {
        hints.push(
            "Could not establish a connection. Check that the host is reachable and that no firewall is blocking outbound HTTPS."
                .to_string(),
        );
    }
    if err.is_decode() {
        hints.push("The response body could not be decoded.".to_string());
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // A self-signed PEM cert generated for testing only.
    //
    // $ openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
    //     -keyout key.pem -out cert.pem -days 365 -nodes \
    //     -subj "/CN=localhost" \
    //     -addext "subjectAltName=DNS:localhost,DNS:*.localhost,IP:127.0.0.1"
    const TEST_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----
MIIBpjCCAUygAwIBAgIUF1tLFV2l2URaYf1oYgEMs89bv8owCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDUwNDA0NTU1OVoXDTI3MDUwNDA0
NTU1OVowFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0D
AQcDQgAEAFZrTfAdhntykKL3WTL/hGHnBQhxv1205XRWnXzMwWSaow9R+VIEKZRw
kwrKKPM04RlpiwqCbJOV/IutFvQHvqN8MHowHQYDVR0OBBYEFLryrLkUMiRWV9yF
Dj7paTV/36+/MB8GA1UdIwQYMBaAFLryrLkUMiRWV9yFDj7paTV/36+/MA8GA1Ud
EwEB/wQFMAMBAf8wJwYDVR0RBCAwHoIJbG9jYWxob3N0ggsqLmxvY2FsaG9zdIcE
fwAAATAKBggqhkjOPQQDAgNIADBFAiEAhgdXBmYJYqipYwiDM1SKiXDg2bwN9YLu
zbjOBz0kJ14CIA+tqV3c2sYRJhqwLu7phihPef38zcG70ADcz5o2VQnk
-----END CERTIFICATE-----
";

    #[test]
    fn user_agent_includes_version() {
        let ua = user_agent();
        assert!(ua.starts_with("Mozart/"));
    }

    #[test]
    fn load_cafile_parses_pem_bundle() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(TEST_PEM).unwrap();
        let certs = load_cafile(f.path()).expect("valid PEM should parse");
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn load_cafile_missing_file_errors() {
        let err = load_cafile(Path::new("/nonexistent/path/to/cafile.pem")).unwrap_err();
        assert!(err.to_string().contains("could not be read"));
    }

    #[test]
    fn load_cafile_invalid_pem_errors() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"this is not a PEM file\n").unwrap();
        let err = load_cafile(f.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not valid") || msg.contains("did not contain"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn load_capath_reads_pem_files_and_skips_others() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.pem"), TEST_PEM).unwrap();
        std::fs::write(dir.path().join("README.txt"), b"not a cert").unwrap();
        let certs = load_capath(dir.path()).expect("should succeed");
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn load_capath_rejects_file_path() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let err = load_capath(f.path()).unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }
}
