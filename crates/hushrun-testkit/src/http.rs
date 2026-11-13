//! A minimal HTTP/1.1 server on the loopback, for tests that need a service to talk to: one
//! thread per connection and one request per connection (`connection: close`). A body can be
//! read and written through a [`Link`] of a set rate, shared by every connection that uses it,
//! for tests of what a slow link does ([`Server::start_with`], [`Reply::Slow`]).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A request, read whole.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// The path as sent, without the query.
    pub path: String,
    /// Query parameters, percent-decoded.
    pub query: Vec<(String, String)>,
    /// Headers, with lowercase names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn bytes(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response {
            body: body.into(),
            ..Response::new(status)
        }
        .header("content-type", "application/octet-stream")
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Response {
        Response {
            body: serde_json::to_vec(value).expect("json"),
            ..Response::new(status)
        }
        .header("content-type", "application/json")
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Response {
        self.headers.push((name.to_string(), value.into()));
        self
    }
}

/// What the server does with a request.
#[derive(Debug, Clone)]
pub enum Reply {
    Send(Response),
    /// Answers after a pause.
    Later(Duration, Response),
    /// Closes the connection without answering.
    Close,
    /// Answers with the body written through a link, which every connection using it shares:
    /// eight bodies at once each get an eighth of its rate. A client that goes away stops it.
    Slow(Arc<Link>, Response),
    /// Answers with the status line, the headers and the first half of the body, then closes:
    /// an answer cut on its way.
    Cut(Response),
}

/// A link of one rate, shared by every body that goes through it, in either direction: what a
/// slow connection between a machine and a store is. 0 is no limit.
#[derive(Debug)]
pub struct Link {
    rate: AtomicU64,
    /// When the link is next free: each piece takes its turn after the ones before it.
    next: Mutex<Instant>,
}

impl Link {
    /// A link with no limit until [`Link::set_rate`].
    pub fn new() -> Arc<Link> {
        Arc::new(Link {
            rate: AtomicU64::new(0),
            next: Mutex::new(Instant::now()),
        })
    }

    /// Holds the link to `bytes_per_second` (0: no limit).
    pub fn set_rate(&self, bytes_per_second: u64) {
        self.rate.store(bytes_per_second, Ordering::SeqCst);
    }

    /// The link's rate, 0 for no limit.
    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::SeqCst)
    }

    /// Waits until `bytes` more may go through: after every piece that took its turn before.
    fn pass(&self, bytes: usize) {
        let rate = self.rate();
        if rate == 0 {
            return;
        }
        let due = {
            let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            let start = (*next).max(now);
            *next = start + Duration::from_secs_f64(bytes as f64 / rate as f64);
            *next
        };
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
}

/// The link a request's body is read through, decided from its method and path once its head
/// is read: `None` for as fast as it comes.
pub type ReadLink = dyn Fn(&str, &str) -> Option<Arc<Link>> + Send + Sync;

/// A running server. Dropping it stops accepting connections.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Listens on a free loopback port and answers each request with `handler`.
    pub fn start(handler: impl Fn(Request) -> Reply + Send + Sync + 'static) -> Server {
        Server::start_with(|_, _| None, handler)
    }

    /// [`Server::start`], reading each request's body through the link `read_link` gives for
    /// its method and path.
    pub fn start_with(
        read_link: impl Fn(&str, &str) -> Option<Arc<Link>> + Send + Sync + 'static,
        handler: impl Fn(Request) -> Reply + Send + Sync + 'static,
    ) -> Server {
        let read_rate: Arc<ReadLink> = Arc::new(read_link);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("local address");
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        let stopping = stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let handler = handler.clone();
                let read_rate = read_rate.clone();
                std::thread::spawn(move || serve(stream, read_rate.as_ref(), handler.as_ref()));
            }
        });
        Server { addr, stop }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so that it sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

fn serve(
    stream: TcpStream,
    read_rate: &ReadLink,
    handler: &(dyn Fn(Request) -> Reply + Send + Sync),
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Some(request) = read_request(&stream, read_rate) else {
        return;
    };
    match handler(request) {
        Reply::Send(response) => write_response(stream, &response),
        Reply::Later(pause, response) => {
            std::thread::sleep(pause);
            write_response(stream, &response);
        }
        Reply::Close => {
            let _ = stream.shutdown(Shutdown::Both);
        }
        Reply::Slow(link, response) => write_slowly(stream, &response, Some(&link)),
        Reply::Cut(response) => write_cut(stream, &response),
    }
}

/// The piece a throttled body is read or written in.
const PIECE: usize = 16 * 1024;

fn read_request(stream: &TcpStream, read_rate: &ReadLink) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let mut body = Vec::new();
    if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).ok()?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else if let Some(length) = header("content-length") {
        let length: usize = length.parse().ok()?;
        let link = read_rate(&method, target.split('?').next().unwrap_or(""));
        body = Vec::with_capacity(length.min(64 << 20));
        let mut piece = vec![0; PIECE];
        while body.len() < length {
            let want = PIECE.min(length - body.len());
            if let Some(link) = &link {
                link.pass(want);
            }
            reader.read_exact(&mut piece[..want]).ok()?;
            body.extend_from_slice(&piece[..want]);
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect();
    Some(Request {
        method,
        path: path.to_string(),
        query,
        headers,
        body,
    })
}

fn write_response(stream: TcpStream, response: &Response) {
    write_slowly(stream, response, None);
}

fn write_slowly(mut stream: TcpStream, response: &Response, link: Option<&Link>) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let written = (|| {
        stream.write_all(head.as_bytes())?;
        match link {
            None => stream.write_all(&response.body)?,
            Some(link) => {
                for piece in response.body.chunks(PIECE) {
                    link.pass(piece.len());
                    stream.write_all(piece)?;
                    stream.flush()?;
                }
            }
        }
        stream.flush()
    })();
    let _ = written;
    let _ = stream.shutdown(Shutdown::Write);
}

/// The head, which promises the whole body, and half of the body.
fn write_cut(mut stream: TcpStream, response: &Response) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body[..response.body.len() / 2]);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        307 => "Temporary Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

/// Percent-decoding of a query component (`+` is a space).
pub(crate) fn decode(text: &str) -> String {
    let hex = |b: u8| char::from(b).to_digit(16);
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = match bytes.get(i..i + 3) {
            Some([b'%', high, low]) => hex(*high).zip(hex(*low)).map(|(h, l)| (h * 16 + l) as u8),
            _ => None,
        };
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
            }
            (None, b'+') => {
                out.push(b' ');
                i += 1;
            }
            (None, byte) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_components() {
        assert_eq!(decode("v1%2Fevents%2F"), "v1/events/");
        assert_eq!(decode("a+b%20c"), "a b c");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%41"), "A");
    }

    #[test]
    fn answers_requests() {
        let server = Server::start(|request| {
            Reply::Send(Response::json(
                200,
                &serde_json::json!({
                    "method": request.method,
                    "path": request.path,
                    "q": request.query("q"),
                    "body": String::from_utf8_lossy(&request.body),
                }),
            ))
        });
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream
            .write_all(b"POST /x?q=a%2Fb HTTP/1.1\r\nhost: h\r\ncontent-length: 3\r\n\r\nabc")
            .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(
            answer.ends_with(r#"{"body":"abc","method":"POST","path":"/x","q":"a/b"}"#),
            "{answer}"
        );
    }
}
