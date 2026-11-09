//! Registry credentials (§11.5), in order:
//!
//! 1. `$DOCKER_CONFIG/config.json`, or `~/.docker/config.json`: `credHelpers[host]`, then
//!    `credsStore`, then `auths[host].auth`. Helpers run as `docker-credential-<helper> get`
//!    with the host on stdin.
//! 2. Anonymous access.
//!
//! This covers `gcloud auth configure-docker`, ECR, ACR, GHCR and Docker Hub. Secrets are never
//! logged or put in errors.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;

use crate::registry::ArtifactError;

/// How to authenticate to one registry host.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Anonymous,
    Basic { username: String, password: String },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Anonymous => f.write_str("Anonymous"),
            Credential::Basic { username, .. } => write!(f, "Basic({username}, ***)"),
        }
    }
}

/// `config.json`, the parts hushrun reads.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerConfig {
    #[serde(default)]
    auths: BTreeMap<String, AuthEntry>,
    #[serde(default)]
    creds_store: Option<String>,
    #[serde(default)]
    cred_helpers: BTreeMap<String, String>,
    /// Where to look for helpers instead of `PATH` (tests).
    #[serde(skip)]
    helper_path: Option<std::ffi::OsString>,
    /// How long a helper may take; `None` means [`HELPER_TIMEOUT`].
    #[serde(skip)]
    helper_timeout: Option<Duration>,
}

/// How long a credential helper may take. Some hang: Docker Desktop's, when Desktop isn't
/// running, or a keychain helper waiting for a prompt nobody will answer in CI.
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Default, Clone, Deserialize)]
struct AuthEntry {
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HelperOutput {
    username: String,
    secret: String,
}

impl DockerConfig {
    /// Reads `$DOCKER_CONFIG/config.json` or `~/.docker/config.json`; no file means no
    /// configured credentials.
    pub fn load() -> Result<DockerConfig, ArtifactError> {
        let Some(path) = config_path() else {
            return Ok(DockerConfig::default());
        };
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<DockerConfig, ArtifactError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                ArtifactError::Invalid(format!("{}: not a Docker config: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DockerConfig::default()),
            Err(e) => Err(ArtifactError::Invalid(format!(
                "reading {}: {e}",
                path.display()
            ))),
        }
    }

    /// The credential for `host`.
    pub fn credential(&self, host: &str) -> Result<Credential, ArtifactError> {
        let host = normalize(host);
        let helper = self
            .cred_helpers
            .iter()
            .find(|(key, _)| normalize(key) == host)
            .map(|(_, helper)| helper)
            .or(self.creds_store.as_ref());
        let timeout = self.helper_timeout.unwrap_or(HELPER_TIMEOUT);
        if let Some(helper) = helper
            && let Some(credential) =
                run_helper(helper, &host, self.helper_path.as_deref(), timeout)?
        {
            return Ok(credential);
        }
        if let Some(entry) = self
            .auths
            .iter()
            .find(|(key, _)| normalize(key) == host)
            .map(|(_, entry)| entry)
        {
            if let Some(encoded) = &entry.auth {
                return decode_auth(encoded, &host);
            }
            if let (Some(username), Some(password)) = (&entry.username, &entry.password) {
                return Ok(Credential::Basic {
                    username: username.clone(),
                    password: password.clone(),
                });
            }
        }
        Ok(Credential::Anonymous)
    }
}

fn config_path() -> Option<PathBuf> {
    match std::env::var_os("DOCKER_CONFIG") {
        Some(dir) => Some(PathBuf::from(dir).join("config.json")),
        None => dirs::home_dir().map(|home| home.join(".docker").join("config.json")),
    }
}

/// The host a config key names: `https://index.docker.io/v1/` and `docker.io` are both Docker
/// Hub; schemes and paths are dropped.
fn normalize(key: &str) -> String {
    let without_scheme = key
        .strip_prefix("https://")
        .or_else(|| key.strip_prefix("http://"))
        .unwrap_or(key);
    let host = without_scheme.split('/').next().unwrap_or(without_scheme);
    match host {
        "index.docker.io" | "registry-1.docker.io" | "docker.io" => "docker.io".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// The `ServerURL` a helper expects: Docker Hub's is the legacy index URL.
fn server_url(host: &str) -> &str {
    if host == "docker.io" {
        "https://index.docker.io/v1/"
    } else {
        host
    }
}

fn decode_auth(encoded: &str, host: &str) -> Result<Credential, ArtifactError> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| {
            ArtifactError::Invalid(format!(
                "the Docker config's auth for {host} isn't base64 user:password"
            ))
        })?;
    let (username, password) = decoded.split_once(':').ok_or_else(|| {
        ArtifactError::Invalid(format!(
            "the Docker config's auth for {host} isn't user:password"
        ))
    })?;
    Ok(Credential::Basic {
        username: username.to_string(),
        password: password.to_string(),
    })
}

/// Runs `docker-credential-<helper> get`. A helper that has nothing for the host (it exits
/// non-zero, for example "credentials not found") means "no credential here".
fn run_helper(
    helper: &str,
    host: &str,
    search_path: Option<&std::ffi::OsStr>,
    timeout: Duration,
) -> Result<Option<Credential>, ArtifactError> {
    if helper.is_empty()
        || !helper
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ArtifactError::Invalid(format!(
            "{helper:?} is not a credential helper name"
        )));
    }
    let program = format!("docker-credential-{helper}");
    let mut command = Command::new(&program);
    if let Some(path) = search_path {
        // On Unix the child's PATH is the one searched.
        command.env("PATH", path);
    }
    let mut child = match command
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!("{program} isn't installed; trying {host} without its credentials");
            return Ok(None);
        }
        Err(e) => return Err(ArtifactError::Invalid(format!("running {program}: {e}"))),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(server_url(host).as_bytes());
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!(
                    "{program} didn't answer within {} s; trying {host} without its credentials",
                    timeout.as_secs()
                );
                return Ok(None);
            }
            Err(e) => return Err(ArtifactError::Invalid(format!("running {program}: {e}"))),
        }
    };
    if !status.success() {
        tracing::debug!("{program} has no credentials for {host}");
        return Ok(None);
    }
    // Credentials are a few hundred bytes: the pipe never fills before the helper exits.
    let mut stdout = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    let found: HelperOutput = serde_json::from_slice(&stdout).map_err(|_| {
        ArtifactError::Invalid(format!(
            "{program} printed something other than credentials JSON"
        ))
    })?;
    Ok(Some(Credential::Basic {
        username: found.username,
        password: found.secret,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: &str) -> DockerConfig {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn auths_are_matched_by_host() {
        let c = config(
            r#"{"auths": {
                "https://index.docker.io/v1/": {"auth": "dXNlcjpwYXNz"},
                "ghcr.io": {"username": "bot", "password": "token"},
                "https://registry.example.com/v2/": {"auth": "YTpiOmM="}
            }}"#,
        );
        let basic = |u: &str, p: &str| Credential::Basic {
            username: u.into(),
            password: p.into(),
        };
        assert_eq!(c.credential("docker.io").unwrap(), basic("user", "pass"));
        assert_eq!(
            c.credential("registry-1.docker.io").unwrap(),
            basic("user", "pass")
        );
        assert_eq!(c.credential("ghcr.io").unwrap(), basic("bot", "token"));
        // Passwords may contain colons.
        assert_eq!(
            c.credential("registry.example.com").unwrap(),
            basic("a", "b:c")
        );
        assert_eq!(c.credential("quay.io").unwrap(), Credential::Anonymous);
    }

    #[test]
    fn debug_never_shows_the_password() {
        let c = Credential::Basic {
            username: "u".into(),
            password: "hunter2".into(),
        };
        assert!(!format!("{c:?}").contains("hunter2"));
    }

    #[test]
    fn a_missing_file_means_anonymous() {
        let dir = tempfile::tempdir().unwrap();
        let c = DockerConfig::load_from(&dir.path().join("config.json")).unwrap();
        assert_eq!(c.credential("ghcr.io").unwrap(), Credential::Anonymous);
        std::fs::write(dir.path().join("bad.json"), "{").unwrap();
        assert!(DockerConfig::load_from(&dir.path().join("bad.json")).is_err());
    }

    #[test]
    fn helper_names_are_checked() {
        let c = config(r#"{"credHelpers": {"ghcr.io": "../evil"}}"#);
        assert!(c.credential("ghcr.io").is_err());
    }

    /// Helpers are found on PATH; the test points the search at a fake one.
    #[cfg(unix)]
    #[test]
    fn helpers_win_over_auths() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("docker-credential-fake");
        std::fs::write(
            &helper,
            "#!/bin/sh\nread host\nif [ \"$host\" = us-docker.pkg.dev ]; then\n  printf '{\"ServerURL\":\"%s\",\"Username\":\"oauth2accesstoken\",\"Secret\":\"ya29.x\"}' \"$host\"\nelse\n  echo 'credentials not found in native keychain' >&2; exit 1\nfi\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut c = config(
            r#"{"credHelpers": {"us-docker.pkg.dev": "fake", "ghcr.io": "fake"},
                "auths": {"ghcr.io": {"auth": "Ym90OnRva2Vu"}}}"#,
        );
        c.helper_path = Some(format!("{}:/usr/bin:/bin", dir.path().display()).into());
        assert_eq!(
            c.credential("us-docker.pkg.dev").unwrap(),
            Credential::Basic {
                username: "oauth2accesstoken".into(),
                password: "ya29.x".into()
            }
        );
        // The helper has nothing for ghcr.io: the auths entry answers.
        assert_eq!(
            c.credential("ghcr.io").unwrap(),
            Credential::Basic {
                username: "bot".into(),
                password: "token".into()
            }
        );
        // A configured helper that isn't installed is skipped with a warning.
        c.cred_helpers.insert("quay.io".into(), "absent".into());
        assert_eq!(c.credential("quay.io").unwrap(), Credential::Anonymous);
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_helper_times_out() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("docker-credential-hang");
        std::fs::write(&helper, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut c = config(r#"{"credsStore": "hang"}"#);
        c.helper_path = Some(format!("{}:/usr/bin:/bin", dir.path().display()).into());
        c.helper_timeout = Some(Duration::from_millis(300));
        let started = Instant::now();
        assert_eq!(c.credential("ghcr.io").unwrap(), Credential::Anonymous);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
