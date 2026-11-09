//! Ledger URLs (L-1): `https://host/path` \[M18\], such as the hosted ledger's, and `http://`
//! on loopback hosts only, for local development. Every other scheme, such as the bucket and
//! file ledgers' (`gs://`, `s3://`, `az://`, `file://`), fails with one message (ADR 0022).

use std::fmt;
use std::str::FromStr;

use crate::agent;

/// How a ledger is reached: always the HTTP protocol of L-10 \[M18\].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LedgerScheme {
    Https,
    /// Unencrypted: loopback hosts only, for local development.
    Http,
}

impl LedgerScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            LedgerScheme::Https => "https",
            LedgerScheme::Http => "http",
        }
    }
}

/// The only hosts an `http://` ledger may name (L-1): plain HTTP is for local development.
pub const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// Why a ledger URL of any other scheme fails to parse (ADR 0022).
const ONLY_HTTPS: &str = "only https:// repository URLs are supported (hushrun's server)";

/// A parsed ledger URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LedgerUrl {
    pub scheme: LedgerScheme,
    /// The host and port.
    pub host: String,
    /// The path, without a leading or trailing `/`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{url:?} is not a repository URL: {reason}")]
pub struct LedgerUrlError {
    /// The URL as messages show it: without userinfo, a query or a fragment, which may carry
    /// credentials.
    pub url: String,
    pub reason: String,
}

/// `text` without the parts of a URL that may carry credentials: userinfo becomes `…@`, and a
/// query or a fragment `?…`.
fn shown(text: &str) -> String {
    // A URL's other parts are shown, and a HUSHRUN_AGENT string holds a private key (ADR 0046).
    if agent::is_agent_string(text) {
        return agent::NOT_SHOWN.to_string();
    }
    let (cut, dropped) = match text.find(['?', '#']) {
        Some(i) => (&text[..i], "?…"),
        None => (text, ""),
    };
    let shown = match cut.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = match rest.split_once('/') {
                Some((authority, path)) => (authority, Some(path)),
                None => (rest, None),
            };
            let authority = match authority.rsplit_once('@') {
                Some((_, host)) => format!("…@{host}"),
                None => authority.to_string(),
            };
            match path {
                Some(path) => format!("{scheme}://{authority}/{path}"),
                None => format!("{scheme}://{authority}"),
            }
        }
        None => match cut.rsplit_once('@') {
            Some((_, rest)) => format!("…@{rest}"),
            None => cut.to_string(),
        },
    };
    format!("{shown}{dropped}")
}

impl LedgerUrl {
    /// The ledger of `repository` under an organization's base (T-4, `HUSHRUN_REPOSITORY_BASE`):
    /// `<base>/<repository id>`, such as `https://hushrun.example/l/acme/github.com/acme/back`. The
    /// result is checked like any ledger URL, so a repository id can't climb out of the base.
    pub fn under(&self, repository: &str) -> Result<LedgerUrl, LedgerUrlError> {
        let joined = format!("{self}/{repository}");
        if repository.is_empty()
            || repository.starts_with('/')
            || repository.ends_with('/')
            || repository
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return Err(LedgerUrlError {
                url: shown(&joined),
                reason: format!(
                    "{:?} is not a repository id such as github.com/acme/app",
                    agent::masked(repository)
                ),
            });
        }
        LedgerUrl::parse(&joined)
    }

    pub fn parse(text: &str) -> Result<LedgerUrl, LedgerUrlError> {
        let err = |reason: &str| LedgerUrlError {
            url: shown(text),
            reason: reason.to_string(),
        };
        // A URL gets printed and sent, and a HUSHRUN_AGENT string holds a private key (ADR 0046).
        if agent::is_agent_string(text) {
            return Err(err("it holds a HUSHRUN_AGENT string"));
        }
        let (scheme, rest) = text
            .split_once("://")
            .ok_or_else(|| err("expected <scheme>://…"))?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "https" => LedgerScheme::Https,
            "http" => LedgerScheme::Http,
            _ => return Err(err(ONLY_HTTPS)),
        };
        if rest.contains(['?', '#']) || rest.chars().any(char::is_whitespace) {
            return Err(err("no query, fragment or whitespace is allowed"));
        }
        let (host, path) = parse_http(scheme, rest).map_err(|reason| err(&reason))?;
        Ok(LedgerUrl { scheme, host, path })
    }
}

/// The host, port and path of an `https://` or `http://` ledger, normalized as T-4 compares
/// them and as OIDC audiences use them (L-10): a lowercase host, the port exactly as written,
/// and one trailing `/` dropped (L-1). Credentials never belong in the URL; tokens come from the
/// environment.
fn parse_http(scheme: LedgerScheme, rest: &str) -> Result<(String, String), String> {
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() {
        return Err("the host is missing".into());
    }
    if authority.contains('@') {
        return Err(
            "a repository URL carries no credentials: set HUSHRUN_REPOSITORY_TOKEN instead".into(),
        );
    }
    let authority = authority.to_ascii_lowercase();
    let host = match authority.strip_prefix('[') {
        Some(inner) => {
            let end = inner
                .find(']')
                .ok_or("an IPv6 host needs its closing `]`")?;
            if end == 0
                || !inner[..end]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
            {
                return Err(format!("{:?} is not an IPv6 address", &inner[..end]));
            }
            let after = &inner[end + 1..];
            if !after.is_empty() {
                check_port(
                    after
                        .strip_prefix(':')
                        .ok_or("expected `:port` after the IPv6 host")?,
                )?;
            }
            &authority[..end + 2]
        }
        None => {
            let host = match authority.rsplit_once(':') {
                Some((host, port)) => {
                    check_port(port)?;
                    host
                }
                None => authority.as_str(),
            };
            let valid = !host.is_empty()
                && host.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.'
                });
            if !valid {
                return Err(format!("{host:?} is not a host name"));
            }
            host
        }
    };
    if scheme == LedgerScheme::Http && !LOOPBACK_HOSTS.contains(&host) {
        return Err(format!(
            "http:// is allowed only on a loopback host ({}); use https://",
            LOOPBACK_HOSTS.join(", ")
        ));
    }
    // One trailing `/` is dropped; any other empty segment is an error.
    let path = path.strip_suffix('/').unwrap_or(path);
    if !path.is_empty() {
        for segment in path.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err("the path has an empty, `.` or `..` segment".into());
            }
            let unreserved = segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
            if !unreserved {
                return Err(format!(
                    "the path segment {segment:?} has characters other than letters, digits, `-`, `.`, `_` and `~`"
                ));
            }
        }
    }
    Ok((authority.clone(), path.to_string()))
}

fn check_port(port: &str) -> Result<(), String> {
    let valid = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p > 0);
    if valid {
        Ok(())
    } else {
        Err(format!("{port:?} is not a port"))
    }
}

impl fmt::Display for LedgerUrl {
    /// The normalized form: lowercase scheme and host, no trailing `/` (T-4).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = self.scheme.as_str();
        if self.path.is_empty() {
            write!(f, "{scheme}://{}", self.host)
        } else {
            write!(f, "{scheme}://{}/{}", self.host, self.path)
        }
    }
}

impl FromStr for LedgerUrl {
    type Err = LedgerUrlError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        LedgerUrl::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: L-1
    #[test]
    fn only_https_ledgers_parse() {
        for bad in [
            "gs://acme-hushrun-ledger/back",
            "GS://Bucket/back/",
            "s3://bucket/a/b",
            "az://container",
            "file:///var/ledger",
            "file:///C:/ledger",
            "mem://test",
            "oci://ghcr.io/acme/ledger",
            "ftp://host/x",
        ] {
            let err = LedgerUrl::parse(bad).unwrap_err();
            assert_eq!(err.reason, ONLY_HTTPS, "{bad}");
        }
        for bad in ["bucket/prefix", "https//host"] {
            assert!(LedgerUrl::parse(bad).is_err(), "{bad}");
        }
    }

    // covers: L-1, L-10, T-4
    #[test]
    fn https_ledgers_and_loopback_http() {
        for (input, normalized) in [
            (
                "https://ledger.example.com/l/acme/back",
                "https://ledger.example.com/l/acme/back",
            ),
            // Scheme and host are lowercased, the path keeps its case, the port is kept.
            (
                "HTTPS://Ledger.Example.COM:8443/l/Acme/Back/",
                "https://ledger.example.com:8443/l/Acme/Back",
            ),
            ("https://ledger.example.com", "https://ledger.example.com"),
            ("https://ledger.example.com/", "https://ledger.example.com"),
            (
                "http://localhost:8787/l/acme/back",
                "http://localhost:8787/l/acme/back",
            ),
            ("http://127.0.0.1:9/x", "http://127.0.0.1:9/x"),
            ("http://[::1]:7000/l/a/b", "http://[::1]:7000/l/a/b"),
            ("https://[2001:DB8::1]/l/a", "https://[2001:db8::1]/l/a"),
        ] {
            let url = LedgerUrl::parse(input).unwrap();
            assert_eq!(url.to_string(), normalized, "{input}");
            assert_eq!(LedgerUrl::parse(normalized).unwrap(), url, "{input}");
        }
        let url = LedgerUrl::parse("https://h.example:8443/l/a/b").unwrap();
        assert_eq!(
            (url.host.as_str(), url.path.as_str()),
            ("h.example:8443", "l/a/b")
        );
        for bad in [
            // Plain HTTP only on the loopback.
            "http://ledger.example.com/l/a/b",
            "http://127.0.0.2/l/a/b",
            "http://localhost.example.com/l",
            // No credentials, query or fragment.
            "https://user:secret@ledger.example.com/l/a/b",
            "https://ledger.example.com/l/a?token=x",
            "https://ledger.example.com/l/a#f",
            "https:///l/a",
            "https://ledger.example.com:abc/l",
            "https://ledger.example.com:0/l",
            "https://ledger.example.com:+80/l",
            "https://ledger.example.com:65536/l",
            "https://[::1/l",
            "https://[::1]x/l",
            "https://[zz]/l",
            "https://ledger_example.com/l",
            "https://ledger.example.com//l",
            "https://ledger.example.com/l/a//",
            "https://ledger.example.com/l/../a",
            "https://ledger.example.com/l/a%2Fb",
            "https://ledger.example.com/l/a b",
        ] {
            assert!(LedgerUrl::parse(bad).is_err(), "{bad}");
        }
        assert!(
            LedgerUrl::parse("http://ledger.example.com/l")
                .unwrap_err()
                .reason
                .contains("loopback")
        );
    }

    // covers: T-4
    #[test]
    fn a_base_gives_each_repository_its_ledger() {
        for (base, expected) in [
            (
                "https://hushrun.example/l/acme",
                "https://hushrun.example/l/acme/github.com/acme/back",
            ),
            (
                "https://hushrun.example/l/acme/",
                "https://hushrun.example/l/acme/github.com/acme/back",
            ),
            (
                "http://localhost:28787/l/acme",
                "http://localhost:28787/l/acme/github.com/acme/back",
            ),
            (
                "https://ledger.example.com/l/acme",
                "https://ledger.example.com/l/acme/github.com/acme/back",
            ),
        ] {
            let url = LedgerUrl::parse(base)
                .unwrap()
                .under("github.com/acme/back")
                .unwrap();
            assert_eq!(url.to_string(), expected, "{base}");
        }
        let base = LedgerUrl::parse("https://hushrun.example/l/acme").unwrap();
        // The path keeps its case, as the provider reports it.
        assert_eq!(
            base.under("github.com/Acme/Back").unwrap().to_string(),
            "https://hushrun.example/l/acme/github.com/Acme/Back"
        );
        for bad in [
            "",
            "/github.com/a/b",
            "github.com/a/b/",
            "github.com/../b",
            "github.com//b",
            "github.com/a/b?x",
        ] {
            assert!(base.under(bad).is_err(), "{bad:?}");
        }
        let https = LedgerUrl::parse("https://ledger.example.com/l").unwrap();
        assert!(https.under("github.com/a/b c").is_err());
    }

    #[test]
    fn errors_never_show_credentials() {
        for (bad, kept) in [
            (
                "https://ci:hrl_secret@ledger.example.com/l/a/b",
                "https://…@ledger.example.com/l/a/b",
            ),
            (
                "https://hrl_secret@ledger.example.com",
                "https://…@ledger.example.com",
            ),
            (
                "https://ledger.example.com/l/a?token=hrl_secret",
                "https://ledger.example.com/l/a?…",
            ),
            ("gs://bucket/p#hrl_secret", "gs://bucket/p?…"),
            ("hrl_secret@host/x?y", "…@host/x?…"),
        ] {
            let err = LedgerUrl::parse(bad).unwrap_err();
            assert_eq!(err.url, kept, "{bad}");
            assert!(!err.to_string().contains("hrl_secret"), "{err}");
        }
    }
}
