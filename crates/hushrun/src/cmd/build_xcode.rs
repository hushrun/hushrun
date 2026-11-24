//! B-85: the transport of the Xcode compilation-cache service, gRPC over HTTP/2 on a Unix
//! socket, as Xcode's toolchain plugin speaks it to `COMPILATION_CACHE_REMOTE_SERVICE_PATH`. What
//! each request is answered is `hushrun_build::xcode::service`'s; this is only the wire: one
//! HTTP/2 connection per plugin, one stream per call, each request one length-prefixed protobuf
//! message, each reply one message and a `grpc-status` trailer.
//!
//! Every failure is a reply the plugin reads as a miss or an error, and Xcode then compiles: a
//! method this service doesn't have is `UNIMPLEMENTED`, a message it can't read is
//! `INVALID_ARGUMENT`, a compressed one (the service advertises no compression) is
//! `UNIMPLEMENTED`, one larger than the service reads is `RESOURCE_EXHAUSTED`.
//!
//! The service runs on its own threads, beside COMMAND in `hushrun run` ([`give`]), or alone in
//! `hushrun build xcode serve` for the Xcode app ([`app`]). Disk work runs on the blocking pool,
//! never on the threads that drive connections.

pub(crate) mod app;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use hushrun_build::xcode::proto;
use hushrun_build::xcode::service::{Counts, Service};

/// The socket's name in the session's folder.
pub(crate) const SOCKET: &str = "xcode.sock";
/// The most bytes one request may hold: an object's most, and its frame.
const MOST_REQUEST: usize = hushrun_build::xcode::cas::MOST_OBJECT as usize + 1024;
/// HTTP/2 flow-control windows: large, so an object of a few MiB doesn't wait for updates.
const STREAM_WINDOW: u32 = 8 << 20;
const CONNECTION_WINDOW: u32 = 32 << 20;
/// How long in-flight calls get when the service stops.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// gRPC status codes the service writes.
mod code {
    pub(super) const OK: &str = "0";
    pub(super) const INVALID_ARGUMENT: &str = "3";
    pub(super) const RESOURCE_EXHAUSTED: &str = "8";
    pub(super) const UNIMPLEMENTED: &str = "12";
    pub(super) const INTERNAL: &str = "13";
}

/// A running service: stopped by [`Running::stop`], or when dropped.
pub(crate) struct Running {
    pub(crate) socket: PathBuf,
    service: Arc<Service>,
    runtime: Option<tokio::runtime::Runtime>,
    bound: Option<super::build_relay::socket::Bound>,
}

impl Running {
    /// What the service did so far.
    #[allow(dead_code)]
    pub(crate) fn counts(&self) -> Counts {
        self.service.counts()
    }

    /// Trims the cache (at most once a day): what a long-lived service does now and then.
    pub(crate) fn trim(&self, now: SystemTime, keep_days: u32) {
        self.service.trim(now, keep_days);
    }

    /// Stops taking connections, gives calls in flight [`STOP_WAIT`], removes the socket, and
    /// trims the cache (at most once a day). What the service did.
    pub(crate) fn stop(mut self, keep_days: u32) -> Counts {
        self.shut();
        self.service.trim(SystemTime::now(), keep_days);
        self.service.counts()
    }

    fn shut(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(STOP_WAIT);
        }
        if let Some(bound) = self.bound.take() {
            bound.remove();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shut();
    }
}

/// Starts the service for a run, on a socket in the session's `folder` (or a short private
/// folder when that path is too long for a socket).
#[cfg(unix)]
pub(crate) fn start(
    service: Service,
    folder: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Running, String> {
    let bound = super::build_relay::socket::bind_as(folder, SOCKET, env)?;
    let listener = bound
        .listener
        .try_clone()
        .map_err(|err| format!("{}: {err}", bound.path.display()))?;
    let socket = bound.path.clone();
    serve_on(service, listener, socket, Some(bound))
}

/// Starts the service on the socket at `path`, which must not exist (the Xcode app's service,
/// at a fixed place its settings name).
#[cfg(unix)]
pub(crate) fn start_at(service: Service, path: &Path) -> Result<Running, String> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let listener = std::os::unix::net::UnixListener::bind(path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    serve_on(service, listener, path.to_path_buf(), None)
}

#[cfg(unix)]
fn serve_on(
    service: Service,
    listener: std::os::unix::net::UnixListener,
    socket: PathBuf,
    bound: Option<super::build_relay::socket::Bound>,
) -> Result<Running, String> {
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("{}: {err}", socket.display()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("hushrun-xcode")
        .enable_all()
        .build()
        .map_err(|err| format!("the Xcode service's threads: {err}"))?;
    let service = Arc::new(service);
    let listener = {
        let _entered = runtime.enter();
        tokio::net::UnixListener::from_std(listener)
            .map_err(|err| format!("{}: {err}", socket.display()))?
    };
    let accepting = service.clone();
    runtime.spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            let service = accepting.clone();
            tokio::spawn(async move { connection(stream, service).await });
        }
    });
    Ok(Running {
        socket,
        service,
        runtime: Some(runtime),
        bound,
    })
}

#[cfg(not(unix))]
pub(crate) fn start(
    _service: Service,
    folder: &Path,
    _env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Running, String> {
    Err(format!(
        "{}: no Xcode service on this platform",
        folder.display()
    ))
}

#[cfg(not(unix))]
pub(crate) fn start_at(_service: Service, path: &Path) -> Result<Running, String> {
    Err(format!(
        "{}: no Xcode service on this platform",
        path.display()
    ))
}

async fn connection(stream: tokio::net::UnixStream, service: Arc<Service>) {
    let mut builder = h2::server::Builder::new();
    builder
        .initial_window_size(STREAM_WINDOW)
        .initial_connection_window_size(CONNECTION_WINDOW)
        .max_concurrent_streams(1024)
        .max_frame_size(1 << 20);
    let Ok(mut conn) = builder.handshake::<_, Bytes>(stream).await else {
        return;
    };
    while let Some(next) = conn.accept().await {
        let Ok((request, respond)) = next else {
            break;
        };
        let service = service.clone();
        tokio::spawn(async move {
            let _ = call(request, respond, service).await;
        });
    }
}

/// One call: its request read whole, answered, the reply written, and then what the reply left
/// to do.
async fn call(
    request: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    service: Arc<Service>,
) -> Result<(), h2::Error> {
    let path = request.uri().path().to_string();
    let mut body = request.into_body();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        let _ = body.flow_control().release_capacity(chunk.len());
        if buf.len() + chunk.len() > MOST_REQUEST {
            return status(
                &mut respond,
                code::RESOURCE_EXHAUSTED,
                "a request too large",
            );
        }
        buf.extend_from_slice(&chunk);
    }
    let message = match unframe(&buf) {
        Ok(message) => message,
        Err((grpc, why)) => return status(&mut respond, grpc, why),
    };
    let request = match proto::Request::read(&path, message) {
        None => return status(&mut respond, code::UNIMPLEMENTED, "no such method"),
        Some(Err(why)) => {
            return status(&mut respond, code::INVALID_ARGUMENT, why.0);
        }
        Some(Ok(request)) => request,
    };
    let answering = service.clone();
    let answered =
        tokio::task::spawn_blocking(move || answering.answer(request, SystemTime::now())).await;
    let Ok((reply, after)) = answered else {
        return status(&mut respond, code::INTERNAL, "the service failed");
    };
    let message = reply.encode();
    let response = http::Response::builder()
        .status(200)
        .header("content-type", "application/grpc")
        .body(())
        .expect("a fixed response is valid");
    let mut send = respond.send_response(response, false)?;
    send.send_data(Bytes::from(frame(&message)), false)?;
    let mut trailers = http::HeaderMap::new();
    trailers.insert("grpc-status", http::HeaderValue::from_static(code::OK));
    send.send_trailers(trailers)?;
    let _ = tokio::task::spawn_blocking(move || service.after(after, SystemTime::now())).await;
    Ok(())
}

/// A reply of a status alone (gRPC's "trailers-only" form).
fn status(
    respond: &mut h2::server::SendResponse<Bytes>,
    grpc: &'static str,
    message: &str,
) -> Result<(), h2::Error> {
    let mut response = http::Response::builder()
        .status(200)
        .header("content-type", "application/grpc")
        .header("grpc-status", grpc);
    if let Ok(value) = http::HeaderValue::from_str(&percent(message)) {
        response = response.header("grpc-message", value);
    }
    let response = response.body(()).expect("a fixed response is valid");
    respond.send_response(response, true)?;
    Ok(())
}

// --- a run's service (B-84) ------------------------------------------------------------------

/// The run's service, from the session's beginning to its end: one per process, as the relay.
static RUN: std::sync::Mutex<Option<Running>> = std::sync::Mutex::new(None);

/// The xcconfig file's name in the session's folder.
const XCCONFIG: &str = "xcode.xcconfig";

/// B-84: gives COMMAND Xcode's compilation cache through a service of this run, when the
/// checkout holds an Xcode project and the Xcode COMMAND would build with is one hushrun knows:
/// the service is started on a socket in the session's folder, the xcconfig file that names it
/// is written beside it, and COMMAND gets `XCODE_XCCONFIG_FILE`. Every reason for none but an
/// Xcode hushrun doesn't know is silent.
pub(crate) fn give(
    session: &mut hushrun_build::session::Session,
    folder: &Path,
    root: &Path,
    in_ci: bool,
    env: &dyn Fn(&str) -> Option<OsString>,
) {
    use hushrun_build::xcode::{XCCONFIG_VAR, probe, service, xcconfig};
    let xcode = match probe::decide(folder, root, env) {
        Ok(xcode) => xcode,
        Err(probe::NotServed::Version(xcode)) => {
            session.add_line(format!(
                "build: Xcode's compilation cache isn't shared in this run: hushrun knows Xcode {}, and this is Xcode {}",
                probe::KNOWN_MAJORS
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                xcode.version
            ));
            return;
        }
        Err(why) => {
            tracing::debug!("no Xcode service: {why:?}");
            return;
        }
    };
    let current = session.current().clone();
    // The person's own file, which ours includes first; never one hushrun wrote (a run inside a
    // run, or the Xcode app's, which `launchctl setenv` gives every process).
    let theirs = env(XCCONFIG_VAR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| {
            std::fs::read_to_string(path).map_or(true, |text| !xcconfig::is_hushruns(&text))
        });
    // B-89: on a person's machine, an output holding their home folder or the checkout's never
    // leaves it. A CI job's folders are every job's, and name no person.
    let mut folders = Vec::new();
    if !in_ci {
        folders.extend(current.home.clone());
        let given = root.to_string_lossy().into_owned();
        if let Some(resolved) = hushrun_build::roots::resolve_on_disk(&given)
            && resolved != given
        {
            folders.push(resolved);
        }
        folders.push(given);
    }
    let service = service::Service::new(service::Config {
        store: current.store.clone(),
        current: Some(current.clone()),
        platform: Some(xcode.platform()),
        folders,
        staging: current.folder.join("xcode-staging"),
    });
    let running = match start(service, &current.folder, env) {
        Ok(running) => running,
        Err(why) => {
            session.add_line(format!(
                "build: Xcode's compilation cache isn't used in this run: its service couldn't start: {why}"
            ));
            return;
        }
    };
    let maps = match xcconfig::maps(&xcconfig::Root::Folder(root)) {
        Ok(maps) => Some(maps),
        Err(xcconfig::Unmappable(folder)) => {
            session.add_line(format!(
                "build: Xcode's cache keys hold this checkout's folder, whose path has white space ({folder}): no other checkout or machine shares them"
            ));
            None
        }
    };
    let text = xcconfig::text(&running.socket, maps.as_deref(), theirs.as_deref());
    let path = current.folder.join(XCCONFIG);
    if let Err(err) = std::fs::write(&path, text) {
        session.add_line(format!(
            "build: Xcode's compilation cache isn't used in this run: {}: {err}",
            path.display()
        ));
        return;
    }
    session.set_xcode(path);
    *RUN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(running);
}

/// B-91: stops the run's service, before the relay ends the check (so every value it stored is
/// in the log the check's `close` is made from), and says what it did in one line.
pub(crate) fn end(session: &mut hushrun_build::session::Session) {
    let running = RUN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let Some(running) = running else {
        return;
    };
    let counts = running.stop(session.current().keep_days);
    if let Some(line) = summary(&counts) {
        session.add_line(line);
    }
}

/// The run's line about Xcode's compiles (B-91), or none when Xcode asked nothing.
pub(crate) fn summary(counts: &Counts) -> Option<String> {
    if counts.asked == 0 && counts.put == 0 {
        return None;
    }
    let compiled = counts.asked.saturating_sub(counts.local + counts.reused);
    let mut line = format!(
        "build: xcode: {} {} asked: {} from this Mac's cache, {} reused",
        counts.asked,
        if counts.asked == 1 {
            "compile"
        } else {
            "compiles"
        },
        counts.local,
        counts.reused,
    );
    if counts.downloaded > 0 {
        line.push_str(&format!(" ({} from the shared store)", counts.downloaded));
    }
    line.push_str(&format!(", {compiled} compiled"));
    if counts.stored > 0 {
        line.push_str(&format!(", {} kept to share", counts.stored));
    }
    if counts.differed > 0 {
        line.push_str(&format!(
            "; {} compiled differently from their blocks, which aren't reused",
            counts.differed
        ));
    }
    Some(line)
}

/// A message's gRPC frame: not compressed, its length, its bytes.
pub(crate) fn frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + message.len());
    out.push(0);
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out
}

/// The one message of a request's body.
fn unframe(body: &[u8]) -> Result<&[u8], (&'static str, &'static str)> {
    let Some((head, rest)) = body.split_first_chunk::<5>() else {
        return Err((code::INVALID_ARGUMENT, "no message"));
    };
    if head[0] != 0 {
        return Err((code::UNIMPLEMENTED, "compressed messages"));
    }
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if rest.len() != len {
        return Err((code::INVALID_ARGUMENT, "not one whole message"));
    }
    Ok(rest)
}

/// gRPC's percent-encoding of a status message.
fn percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if (0x20..0x7f).contains(&byte) && byte != b'%' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(all(test, unix))]
mod tests;
