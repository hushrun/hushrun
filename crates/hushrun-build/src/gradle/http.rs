//! B-77: as much HTTP/1.1 as Gradle's `HttpBuildCache` speaks to hushrun's cache, over the
//! loopback: `GET`, `HEAD` and `PUT` of one path, Basic authentication, bodies by
//! `Content-Length` or chunked, `Expect: 100-continue`, and connections kept open between
//! requests. Everything else is answered and the connection closed. std only.

use std::io::{self, BufRead, Read, Write};

/// The most bytes a request's line and headers may take.
const MOST_HEAD: usize = 64 * 1024;

/// One request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// The `Authorization` header's value, as sent.
    pub authorization: Option<String>,
    pub body: Body,
    /// The client asked to close the connection after this request.
    pub close: bool,
}

/// A request's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Bytes(Vec<u8>),
    /// Longer than the limit: read and dropped, so the connection can go on.
    TooLarge,
}

/// Why a request couldn't be read: the connection is closed after the answer, if any.
#[derive(Debug)]
pub enum Unread {
    /// The connection ended, or went quiet, between requests: no answer.
    Gone,
    /// What came isn't a request this cache reads: answered with `status`.
    Bad(u16),
}

impl From<io::Error> for Unread {
    fn from(_: io::Error) -> Unread {
        Unread::Gone
    }
}

/// Reads the next request on a connection: its body up to `most_body` bytes. `continue_to` is
/// written to before the body when the client waits for it (`Expect: 100-continue`).
pub fn read(
    input: &mut impl BufRead,
    continue_to: &mut impl Write,
    most_body: u64,
) -> Result<Request, Unread> {
    let mut head = Vec::new();
    loop {
        let start = head.len();
        let n = input.by_ref().take(8 * 1024).read_until(b'\n', &mut head)?;
        if n == 0 {
            return Err(Unread::Gone);
        }
        if head.len() > MOST_HEAD {
            return Err(Unread::Bad(431));
        }
        let line = &head[start..];
        // Blank lines before a request are read past (RFC 9112 §2.2).
        if start == 0 && (line == b"\r\n" || line == b"\n") {
            head.clear();
            continue;
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if !line.ends_with(b"\n") {
            return Err(Unread::Bad(431));
        }
    }
    let text = std::str::from_utf8(&head).map_err(|_| Unread::Bad(400))?;
    let mut lines = text.split("\r\n").flat_map(|l| l.split('\n'));
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let (Some(method), Some(path), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(Unread::Bad(400));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Unread::Bad(505));
    }
    let mut length: Option<u64> = None;
    let mut chunked = false;
    let mut authorization = None;
    let mut close = version == "HTTP/1.0";
    let mut expect = false;
    for line in lines.filter(|l| !l.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(Unread::Bad(400));
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                let n: u64 = value.parse().map_err(|_| Unread::Bad(400))?;
                if length.is_some_and(|l| l != n) {
                    return Err(Unread::Bad(400));
                }
                length = Some(n);
            }
            "transfer-encoding" => {
                if !value.eq_ignore_ascii_case("chunked") {
                    return Err(Unread::Bad(501));
                }
                chunked = true;
            }
            "authorization" => authorization = Some(value.to_string()),
            "connection" => {
                if value.eq_ignore_ascii_case("close") {
                    close = true;
                } else if value.eq_ignore_ascii_case("keep-alive") {
                    close = false;
                }
            }
            "expect" if value.eq_ignore_ascii_case("100-continue") => expect = true,
            _ => {}
        }
    }
    if chunked && length.is_some() {
        return Err(Unread::Bad(400));
    }
    if expect && (chunked || length.is_some_and(|n| n > 0)) {
        continue_to.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        continue_to.flush()?;
    }
    let body = if chunked {
        read_chunked(input, most_body)?
    } else {
        read_length(input, length.unwrap_or(0), most_body)?
    };
    Ok(Request {
        method: method.to_string(),
        path: path.to_string(),
        authorization,
        body,
        close,
    })
}

fn read_length(input: &mut impl BufRead, length: u64, most: u64) -> Result<Body, Unread> {
    if length > most {
        let skipped = io::copy(&mut input.by_ref().take(length), &mut io::sink())?;
        if skipped != length {
            return Err(Unread::Gone);
        }
        return Ok(Body::TooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0).min(1 << 24));
    input.by_ref().take(length).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length {
        return Err(Unread::Gone);
    }
    Ok(Body::Bytes(bytes))
}

fn read_chunked(input: &mut impl BufRead, most: u64) -> Result<Body, Unread> {
    let mut bytes = Vec::new();
    let mut total: u64 = 0;
    loop {
        let mut line = String::new();
        input.by_ref().take(1024).read_line(&mut line)?;
        let size = line.trim().split(';').next().unwrap_or_default();
        let size = u64::from_str_radix(size, 16).map_err(|_| Unread::Bad(400))?;
        if size == 0 {
            // Trailers, then the blank line.
            loop {
                let mut trailer = String::new();
                if input.by_ref().take(8 * 1024).read_line(&mut trailer)? == 0 {
                    return Err(Unread::Gone);
                }
                if trailer == "\r\n" || trailer == "\n" {
                    break;
                }
            }
            break;
        }
        total = total.saturating_add(size);
        if total > most {
            io::copy(&mut input.by_ref().take(size), &mut io::sink())?;
        } else {
            let before = bytes.len();
            input.by_ref().take(size).read_to_end(&mut bytes)?;
            if (bytes.len() - before) as u64 != size {
                return Err(Unread::Gone);
            }
        }
        let mut end = [0u8; 2];
        input.read_exact(&mut end)?;
        if &end != b"\r\n" {
            return Err(Unread::Bad(400));
        }
    }
    Ok(if total > most {
        Body::TooLarge
    } else {
        Body::Bytes(bytes)
    })
}

/// Writes an answer: `status`, and `body` unless the request was a `HEAD` (`head`), whose
/// `Content-Length` is still the body's.
pub fn write(
    out: &mut impl Write,
    status: u16,
    body: &[u8],
    head: bool,
    close: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Error",
    };
    let mut text = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n",
        body.len()
    );
    if status == 200 && !body.is_empty() {
        text.push_str("Content-Type: application/vnd.gradle.build-cache-artifact.v2\r\n");
    }
    if status == 401 {
        text.push_str("WWW-Authenticate: Basic realm=\"hushrun\"\r\n");
    }
    if close {
        text.push_str("Connection: close\r\n");
    }
    text.push_str("\r\n");
    out.write_all(text.as_bytes())?;
    if !head {
        out.write_all(body)?;
    }
    out.flush()
}

/// The Basic credential for `user` and `password`, as Gradle sends it.
pub fn basic(user: &str, password: &str) -> String {
    use base64_alphabet::encode;
    format!("Basic {}", encode(format!("{user}:{password}").as_bytes()))
}

/// Whether `given` is the credential `expected`, compared in time that doesn't depend on where
/// they differ.
pub fn same_credential(given: Option<&str>, expected: &str) -> bool {
    let Some(given) = given else {
        return false;
    };
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Standard base64 with padding, for the one header that needs it.
mod base64_alphabet {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &[u8], most: u64) -> (Result<Request, Unread>, Vec<u8>) {
        let mut input = io::BufReader::new(text);
        let mut sent = Vec::new();
        (read(&mut input, &mut sent, most), sent)
    }

    // covers: B-77
    #[test]
    fn requests_as_gradle_sends_them_are_read() {
        let (got, sent) = parse(
            b"PUT /cache/ab HTTP/1.1\r\nContent-Length: 3\r\nAuthorization: Basic eA==\r\nConnection: Keep-Alive\r\n\r\nxyz",
            10,
        );
        let got = got.unwrap();
        assert_eq!(got.method, "PUT");
        assert_eq!(got.path, "/cache/ab");
        assert_eq!(got.authorization.as_deref(), Some("Basic eA=="));
        assert_eq!(got.body, Body::Bytes(b"xyz".to_vec()));
        assert!(!got.close);
        assert!(sent.is_empty());

        let (got, sent) = parse(
            b"PUT /cache/ab HTTP/1.1\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n\r\n2\r\nxy\r\n1;x\r\nz\r\n0\r\n\r\n",
            10,
        );
        assert_eq!(got.unwrap().body, Body::Bytes(b"xyz".to_vec()));
        assert_eq!(sent, b"HTTP/1.1 100 Continue\r\n\r\n");

        let (got, _) = parse(b"PUT /cache/ab HTTP/1.1\r\nContent-Length: 30\r\n\r\n", 10);
        assert!(matches!(got, Err(Unread::Gone)));
        let (got, _) = parse(
            b"PUT /cache/ab HTTP/1.1\r\nContent-Length: 11\r\n\r\nhello world",
            10,
        );
        assert_eq!(got.unwrap().body, Body::TooLarge);
        let (got, _) = parse(b"GET /cache/ab HTTP/2\r\n\r\n", 10);
        assert!(matches!(got, Err(Unread::Bad(505))));
        let (got, _) = parse(b"", 10);
        assert!(matches!(got, Err(Unread::Gone)));
    }

    // covers: B-77
    #[test]
    fn the_credential_is_gradle_s_basic_form() {
        assert_eq!(basic("hushrun", "secret"), "Basic aHVzaHJ1bjpzZWNyZXQ=");
        assert_eq!(basic("hushrun", "t"), "Basic aHVzaHJ1bjp0");
        assert!(same_credential(
            Some("Basic aHVzaHJ1bjp0"),
            "Basic aHVzaHJ1bjp0"
        ));
        assert!(!same_credential(
            Some("Basic aHVzaHJ1bjp1"),
            "Basic aHVzaHJ1bjp0"
        ));
        assert!(!same_credential(None, "Basic aHVzaHJ1bjp0"));
    }

    // covers: B-77
    #[test]
    fn a_head_answers_the_length_without_the_body() {
        let mut out = Vec::new();
        write(&mut out, 200, b"abc", true, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }
}
