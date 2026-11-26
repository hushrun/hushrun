//! What every build tool's cache does whatever its protocol (B-77, B-170): listens on the
//! loopback in the run's own process, serves at most [`MOST_CONNECTIONS`] connections at once,
//! refuses a peer that isn't this machine, keeps a connection open between requests for
//! [`IDLE`], writes `102 Processing` each heartbeat while a `GET` is worked on (ADR 0144), and
//! stops when the session drops its [`Running`]. What a request is answered is the protocol's
//! ([`Answers`]).

use std::io::{self, BufReader, BufWriter};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use super::http::{self, Unread};

/// The most bytes a request's body may hold: a larger entry stays in the tool's own cache.
pub const MOST_BODY: u64 = 512 << 20;
/// How long a connection may sit between requests.
pub const IDLE: Duration = Duration::from_secs(120);
/// Connections served at once: a tool opens one per worker that asks.
pub const MOST_CONNECTIONS: usize = 64;

/// One tool's protocol, as the listener needs it.
pub trait Answers: Send + Sync + 'static {
    /// The tool's name in the listener's thread names: `gradle`, `maven`.
    fn name(&self) -> &'static str;
    /// One request's status and body.
    fn answer(&self, request: &http::Request) -> (u16, Vec<u8>);
    /// The headers of an answer of `status` with `body`, beside its length.
    fn headers(&self, status: u16, body: &[u8]) -> Vec<(&'static str, &'static str)>;
}

/// The listener while it serves: dropping it stops it.
#[derive(Debug)]
pub struct Running {
    stop: Arc<AtomicBool>,
    address: Option<SocketAddr>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accepting thread, which then sees the flag.
        if let Some(address) = self.address {
            let _ = TcpStream::connect_timeout(&address, Duration::from_millis(200));
        }
    }
}

/// Serves `served` on `listener` until the returned handle is dropped.
pub fn start<A: Answers>(listener: TcpListener, served: A) -> io::Result<Running> {
    let stop = Arc::new(AtomicBool::new(false));
    let address = listener.local_addr().ok();
    let served = Arc::new(served);
    let serving = stop.clone();
    std::thread::Builder::new()
        .name(format!("hushrun-{}-cache", served.name()))
        .spawn(move || accept(listener, served, serving))?;
    Ok(Running { stop, address })
}

fn accept<A: Answers>(listener: TcpListener, served: Arc<A>, stop: Arc<AtomicBool>) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else {
            continue;
        };
        // Only this machine: the listener is on the loopback, and a peer elsewhere is refused.
        if !stream.peer_addr().is_ok_and(|peer| peer.ip().is_loopback()) {
            continue;
        }
        if open.fetch_add(1, Ordering::SeqCst) >= MOST_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let mut out = &stream;
            let _ = http::write(&mut out, 503, b"", false, true, &served.headers(503, b""));
            continue;
        }
        let served = served.clone();
        let done = open.clone();
        let stop = stop.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("hushrun-{}-request", served.name()))
            .spawn(move || {
                connection(&*served, stream, &stop);
                done.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            open.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn connection(served: &impl Answers, stream: TcpStream, stop: &AtomicBool) {
    let _ = stream.set_read_timeout(Some(IDLE));
    let _ = stream.set_write_timeout(Some(IDLE));
    let _ = stream.set_nodelay(true);
    let Ok(reading) = stream.try_clone() else {
        return;
    };
    let mut input = BufReader::new(reading);
    let mut output = BufWriter::new(&stream);
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let request = match http::read(&mut input, &mut output, MOST_BODY) {
            Ok(request) => request,
            Err(Unread::Gone) => return,
            Err(Unread::Bad(status)) => {
                let headers = served.headers(status, b"");
                let _ = http::write(&mut output, status, b"", false, true, &headers);
                return;
            }
        };
        // ADR 0144: `102 Processing` each heartbeat while a `GET` is worked on, so the tool's
        // client never times out on an answer that is only slow.
        let (status, body) = http::answer_alive(&request, &stream, || served.answer(&request));
        let head = request.method == "HEAD";
        let headers = served.headers(status, &body);
        if http::write(&mut output, status, &body, head, request.close, &headers).is_err()
            || request.close
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, Read as _, Write as _};

    use super::*;

    /// A protocol written only for the test: `/hello` is 200 with a body and a header of its
    /// own, every other path 404.
    struct Hello;

    impl Answers for Hello {
        fn name(&self) -> &'static str {
            "hello"
        }

        fn answer(&self, request: &http::Request) -> (u16, Vec<u8>) {
            match request.path.as_str() {
                "/hello" => (200, b"hi".to_vec()),
                _ => (404, Vec::new()),
            }
        }

        fn headers(&self, status: u16, _body: &[u8]) -> Vec<(&'static str, &'static str)> {
            match status {
                200 => vec![("X-Hello", "1")],
                _ => Vec::new(),
            }
        }
    }

    fn answer(reader: &mut BufReader<TcpStream>) -> (String, Vec<u8>) {
        let mut head = String::new();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(n) = line.strip_prefix("Content-Length: ") {
                length = n.trim().parse().unwrap();
            }
            head.push_str(&line);
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (head, body)
    }

    // covers: B-77, B-170
    #[test]
    fn a_protocol_mounted_on_the_service_answers_on_one_kept_connection_and_stops_when_dropped() {
        let listener = match TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => listener,
            // The loopback may be closed to tests in some sandboxes.
            Err(_) => return,
        };
        let address = listener.local_addr().unwrap();
        let running = start(listener, Hello).unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        stream
            .write_all(b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let (head, body) = answer(&mut reader);
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert!(head.contains("X-Hello: 1\r\n"), "{head}");
        assert_eq!(body, b"hi");
        // The same connection, a path the protocol doesn't route.
        stream
            .write_all(b"GET /other HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let (head, body) = answer(&mut reader);
        assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{head}");
        assert!(!head.contains("X-Hello"), "{head}");
        assert!(body.is_empty());
        drop(running);
        // Once stopped, a new connection is served nothing.
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(mut late) = TcpStream::connect(address) {
            let _ = late.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = late.write_all(b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n");
            let mut got = Vec::new();
            let _ = late.read_to_end(&mut got);
            assert!(got.is_empty(), "{}", String::from_utf8_lossy(&got));
        }
    }
}
