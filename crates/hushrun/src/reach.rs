//! \[ADR 0104\] Whether a host answers over HTTPS from here, for `hushrun doctor` (C-50): one
//! request with no credential, through the proxy the environment names, trusting the system's
//! store, as every request of hushrun's does. What it says holds no value of any variable.

use std::time::Duration;

/// What one request found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// The host answered over TLS that verified, with this status.
    Answered(u16),
    /// The network's proxy refused the connection: the host isn't on the allowlist.
    Blocked(String),
    /// TLS didn't verify: a proxy re-signs it with a CA the system store lacks.
    Tls(String),
    /// Anything else: no route, no name, a timeout.
    Unreachable(String),
}

impl Reach {
    /// Whether the host answered.
    pub fn answered(&self) -> bool {
        matches!(self, Reach::Answered(_))
    }

    /// As doctor's line says it.
    pub fn describe(&self) -> String {
        match self {
            Reach::Answered(status) => format!("answers over TLS (HTTP {status})"),
            Reach::Blocked(why) => format!("refused by this network's proxy ({why})"),
            Reach::Tls(why) => format!("TLS doesn't verify with the system's store ({why})"),
            Reach::Unreachable(why) => format!("can't be reached ({why})"),
        }
    }
}

/// Asks `url` once, with no credential, giving up after `timeout`.
pub fn probe(url: &str, timeout: Duration) -> Reach {
    hushrun_ci_crypto();
    let client = match reqwest::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("hushrun/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(err) => return Reach::Unreachable(chain(&err)),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return Reach::Unreachable(err.to_string()),
    };
    match runtime.block_on(async { client.get(url).send().await }) {
        // A proxy that re-signs TLS with a CA the system trusts answers for hosts it refuses.
        Ok(response) if matches!(response.status().as_u16(), 403 | 407) => {
            Reach::Blocked(format!("HTTP {}", response.status().as_u16()))
        }
        Ok(response) => Reach::Answered(response.status().as_u16()),
        // Without the URL, which a host's name could make look like any kind of failure.
        Err(err) => classify(&chain(&err.without_url())),
    }
}

/// Which kind of failure an error's chain of messages is.
fn classify(text: &str) -> Reach {
    let lower = text.to_ascii_lowercase();
    if lower.contains("certificate") || lower.contains("unknownissuer") {
        Reach::Tls(short(text))
    } else if lower.contains("403")
        || lower.contains("407")
        || lower.contains("proxy")
        || lower.contains("tunnel")
    {
        Reach::Blocked(short(text))
    } else {
        Reach::Unreachable(short(text))
    }
}

/// An error's last two messages, which say most, at most 160 characters.
fn short(text: &str) -> String {
    let parts: Vec<&str> = text.rsplitn(3, ": ").collect();
    let last = match parts.as_slice() {
        [last, before, ..] => format!("{before}: {last}"),
        _ => text.to_string(),
    };
    let last = last.trim();
    let mut out: String = last.chars().take(160).collect();
    if last.chars().count() > 160 {
        out.push('…');
    }
    out
}

fn chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        text.push_str(": ");
        text.push_str(&err.to_string());
        source = err.source();
    }
    text
}

/// rustls uses ring, which reqwest is built without (plan §12.3).
fn hushrun_ci_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-50
    #[test]
    fn a_failure_says_what_kind_it_is() {
        assert!(matches!(
            classify("error sending request: client error (Connect): unsuccessful tunnel"),
            Reach::Blocked(_)
        ));
        assert!(matches!(
            classify("error sending request: invalid peer certificate: UnknownIssuer"),
            Reach::Tls(_)
        ));
        assert!(matches!(
            classify("error sending request: dns error: failed to lookup address"),
            Reach::Unreachable(_)
        ));
        assert_eq!(short("a: b: c"), "b: c");
        assert_eq!(short("c"), "c");
    }

    // covers: C-50
    #[test]
    fn a_host_that_answers_is_reached() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n");
        });
        let reach = probe(&format!("http://{address}/"), Duration::from_secs(5));
        server.join().unwrap();
        assert_eq!(reach, Reach::Answered(204));
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        assert!(!probe(&format!("http://127.0.0.1:{port}/"), Duration::from_secs(2)).answered());
    }
}
