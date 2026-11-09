//! hushrun's releases (ADR 0035): which one is the newest, and the files of one. They are the
//! GitHub Releases of the public repository `hushrun/hushrun` (ADR 0034). `hushrun update` and
//! the newer-release notice read them from github.com, or from the mirror that
//! `HUSHRUN_INSTALLER_GITHUB_BASE_URL` names, the variable hushrun's installers take (C-20, C-21).
//!
//! A request carries no credential, and says nothing about the machine beyond what any download
//! does: its address, and hushrun's version in the `User-Agent`.

use std::sync::Once;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use reqwest::Url;
use reqwest::header::LOCATION;
use semver::Version;
use sha2::Digest as _;

/// The public repository the releases are in.
pub const REPOSITORY: &str = "hushrun/hushrun";
/// GitHub's address for releases, as hushrun's installers take it: a mirror, or a test's server.
pub const BASE_VARIABLE: &str = "HUSHRUN_INSTALLER_GITHUB_BASE_URL";
const GITHUB: &str = "https://github.com";
const USER_AGENT: &str = concat!("hushrun/", env!("CARGO_PKG_VERSION"));
/// Redirects a download follows: GitHub sends one, to its storage.
const MAX_REDIRECTS: usize = 5;

/// This hushrun's version.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo's package version is SemVer")
}

/// Where the releases are: `HUSHRUN_INSTALLER_GITHUB_BASE_URL`, else `https://github.com`,
/// without a trailing `/`. It must be `https://`, or `http://` on this machine for a test's
/// server: a checksum read over plain HTTP from elsewhere would prove nothing.
pub fn base() -> anyhow::Result<String> {
    let given = hushrun_ci::var(BASE_VARIABLE)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty());
    let Some(text) = given else {
        return Ok(GITHUB.to_string());
    };
    let url =
        Url::parse(&text).map_err(|_| anyhow!("{BASE_VARIABLE} is {text:?}, which isn't a URL"))?;
    allowed(&url).map_err(|why| anyhow!("{BASE_VARIABLE} is {text:?}: {why}"))?;
    Ok(text)
}

/// Whether hushrun reads releases from `url`: `https://`, or `http://` on a loopback address.
fn allowed(url: &Url) -> Result<(), &'static str> {
    let host = url.host_str().unwrap_or_default();
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        _ => Err("releases are read over https:// only, or http:// on this machine"),
    }
}

/// The release target of this build, as the releases name their archives.
pub fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// The archive of `target` in a release: `hushrun-<target>.zip` on Windows, else
/// `hushrun-<target>.tar.xz`.
pub fn archive(target: &str) -> String {
    let extension = if target.ends_with("-windows-msvc") {
        "zip"
    } else {
        "tar.xz"
    };
    format!("hushrun-{target}.{extension}")
}

/// The command that installs the newest release with hushrun's installer, for messages.
pub fn installer_command() -> &'static str {
    if cfg!(windows) {
        r#"powershell -ExecutionPolicy Bypass -c "irm https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.ps1 | iex""#
    } else {
        "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh | sh"
    }
}

/// The version a release tag's URL names: `0.2.0` for `…/hushrun/hushrun/releases/tag/v0.2.0`.
pub fn tag_version(location: &str) -> Option<Version> {
    let path = location.split(['?', '#']).next()?;
    let (_, tag) = path.rsplit_once("/releases/tag/")?;
    Version::parse(tag.strip_prefix('v')?).ok()
}

/// The SHA-256 that a release's checksum file gives for `archive`: `<hex> *<name>`, as the
/// release writes it, `<hex>  <name>`, or the hex digits alone.
pub fn checksum_for(text: &[u8], archive: &str) -> anyhow::Result<String> {
    let text = std::str::from_utf8(text).map_err(|_| anyhow!("{archive}.sha256 isn't text"))?;
    let mut fields = text.split_whitespace();
    let hex = fields.next().unwrap_or_default().to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("{archive}.sha256 doesn't start with a SHA-256");
    }
    if let Some(name) = fields.next().map(|name| name.trim_start_matches('*'))
        && name != archive
    {
        bail!("{archive}.sha256 is the checksum of {name}, not of {archive}");
    }
    Ok(hex)
}

/// The SHA-256 of `bytes`, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// The releases at one address, read through one HTTP client.
pub struct Releases {
    base: String,
    client: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

impl Releases {
    /// Releases at `base` (see [`base`]), with connections that give up after `connect`.
    pub fn new(base: &str, connect: Duration) -> anyhow::Result<Releases> {
        install_crypto_provider();
        let client = reqwest::Client::builder()
            .connect_timeout(connect)
            // `latest` reads the redirect, and `download` follows redirects itself.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| anyhow!("building the HTTP client: {}", chain(e)))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("starting the I/O runtime")?;
        Ok(Releases {
            base: base.trim_end_matches('/').to_string(),
            client,
            runtime,
        })
    }

    /// The page of release `version`, with its notes.
    pub fn page(&self, version: &Version) -> String {
        format!("{}/{REPOSITORY}/releases/tag/v{version}", self.base)
    }

    /// The newest release's version. GitHub answers `releases/latest` with a redirect to the
    /// newest release's tag, never a pre-release's: hushrun reads the redirect instead of
    /// following it, and needs neither GitHub's REST API, which limits requests without a
    /// token, nor a page's body.
    pub fn latest(&self, timeout: Duration) -> anyhow::Result<Version> {
        let url = format!("{}/{REPOSITORY}/releases/latest", self.base);
        // The request's timer starts when it is sent, inside the runtime.
        let response = self
            .runtime
            .block_on(async { self.client.get(&url).timeout(timeout).send().await })
            .map_err(|e| anyhow!("asking {url}: {}", chain(e)))?;
        let status = response.status();
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        match location {
            Some(location) if status.is_redirection() => tag_version(&location).ok_or_else(|| {
                anyhow!(
                    "{url} points to {}, not to a release's tag such as …/releases/tag/v1.2.3",
                    shortened(&location)
                )
            }),
            _ => Err(anyhow!(
                "{url} answered {status}, not a redirect to the newest release"
            )),
        }
    }

    /// The file `name` of release `version`, at most `limit` bytes, read within `timeout`.
    /// GitHub redirects a download to its storage, on another host: those redirects are
    /// followed, to `https://` only, and their URLs, which carry signatures, never appear in a
    /// message.
    pub fn download(
        &self,
        version: &Version,
        name: &str,
        limit: u64,
        timeout: Duration,
    ) -> anyhow::Result<Vec<u8>> {
        let first = format!(
            "{}/{REPOSITORY}/releases/download/v{version}/{name}",
            self.base
        );
        self.runtime.block_on(async {
            let mut url = Url::parse(&first).with_context(|| format!("{first} isn't a URL"))?;
            for _ in 0..=MAX_REDIRECTS {
                allowed(&url).map_err(|why| anyhow!("downloading {first}: a redirect: {why}"))?;
                let mut response = self
                    .client
                    .get(url.clone())
                    .timeout(timeout)
                    .send()
                    .await
                    .map_err(|e| anyhow!("downloading {first}: {}", chain(e)))?;
                let status = response.status();
                if status.is_redirection() {
                    let location = response
                        .headers()
                        .get(LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or_else(|| {
                            anyhow!("downloading {first}: {status} without a location")
                        })?;
                    url = url.join(location).map_err(|_| {
                        anyhow!("downloading {first}: a redirect to a location that isn't a URL")
                    })?;
                    continue;
                }
                if !status.is_success() {
                    bail!("downloading {first}: {status}");
                }
                if response
                    .content_length()
                    .is_some_and(|length| length > limit)
                {
                    bail!("downloading {first}: it is larger than {limit} bytes");
                }
                let mut body = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|e| anyhow!("downloading {first}: {}", chain(e)))?
                {
                    if (body.len() + chunk.len()) as u64 > limit {
                        bail!("downloading {first}: it is larger than {limit} bytes");
                    }
                    body.extend_from_slice(&chunk);
                }
                return Ok(body);
            }
            Err(anyhow!(
                "downloading {first}: more than {MAX_REDIRECTS} redirects"
            ))
        })
    }
}

/// rustls uses ring (plan §12.3): reqwest is built without a provider, so one must be
/// installed before the first client.
fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Fails only when another provider is already the default, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// An error and its sources, such as "error sending request: … connection refused", without
/// the URL, which may be a signed one.
fn chain(err: reqwest::Error) -> String {
    let err = err.without_url();
    let mut text = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// A server's text, cut short for a message.
fn shortened(text: &str) -> String {
    const MAX: usize = 200;
    match text.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tags_url_names_its_version() {
        let tag = |text: &str| tag_version(text).map(|v| v.to_string());
        assert_eq!(
            tag("https://github.com/hushrun/hushrun/releases/tag/v0.1.0").as_deref(),
            Some("0.1.0")
        );
        assert_eq!(
            tag("/hushrun/hushrun/releases/tag/v1.2.3-rc.1?x=1").as_deref(),
            Some("1.2.3-rc.1")
        );
        for not in [
            "https://github.com/hushrun/hushrun/releases",
            "https://github.com/hushrun/hushrun/releases/tag/0.1.0",
            "https://github.com/hushrun/hushrun/releases/tag/v0.1",
            "https://github.com/hushrun/hushrun/releases/tag/v0.1.0/x",
            "https://github.com/login",
            "",
        ] {
            assert_eq!(tag(not), None, "{not}");
        }
    }

    #[test]
    fn a_checksum_file_must_name_its_archive() {
        let hex = "9ef44d44dd2d0f639e54bdf28ef9baafe4fe3d8a4d7aeb3e78e2a63da8633059";
        let archive = "hushrun-aarch64-apple-darwin.tar.xz";
        for text in [
            format!("{hex} *{archive}\n"),
            format!("{hex}  {archive}"),
            format!("{}\n", hex.to_uppercase()),
        ] {
            assert_eq!(
                checksum_for(text.as_bytes(), archive).unwrap(),
                hex,
                "{text}"
            );
        }
        let other = format!("{hex} *hushrun-x86_64-apple-darwin.tar.xz\n");
        assert!(
            checksum_for(other.as_bytes(), archive)
                .unwrap_err()
                .to_string()
                .contains("not of hushrun-aarch64-apple-darwin.tar.xz")
        );
        for bad in ["", "abc *x", "not hex at all", &"g".repeat(64)] {
            assert!(checksum_for(bad.as_bytes(), archive).is_err(), "{bad}");
        }
        assert!(checksum_for(&[0xff, 0xfe], archive).is_err());
    }

    #[test]
    fn releases_are_read_over_https_or_on_this_machine() {
        for ok in [
            "https://github.com",
            "https://ghe.example.com/mirror",
            "http://127.0.0.1:8080",
            "http://localhost:9",
            "http://[::1]:9",
        ] {
            assert!(allowed(&Url::parse(ok).unwrap()).is_ok(), "{ok}");
        }
        for refused in [
            "http://github.com",
            "http://10.0.0.1",
            "ftp://127.0.0.1",
            "file:///tmp/x",
        ] {
            assert!(allowed(&Url::parse(refused).unwrap()).is_err(), "{refused}");
        }
    }

    #[test]
    fn each_target_has_its_archive() {
        assert_eq!(
            archive("x86_64-pc-windows-msvc"),
            "hushrun-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            archive("aarch64-unknown-linux-musl"),
            "hushrun-aarch64-unknown-linux-musl.tar.xz"
        );
        // Every platform hushrun's tests run on is one the releases build for.
        assert!(target().is_some());
        assert_eq!(current().to_string(), env!("CARGO_PKG_VERSION"));
        assert_eq!(shortened(&"x".repeat(300)).chars().count(), 201);
    }
}
