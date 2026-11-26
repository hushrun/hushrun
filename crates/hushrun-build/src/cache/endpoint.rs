//! Where a build tool's cache listens, in one of two forms.
//!
//! - **Kept in the store** (B-77, Gradle's): a port on the loopback and a token, kept in the
//!   store's `<tool>/endpoint.json` (mode 0600), made once. Gradle's configuration cache records
//!   every variable a settings script reads, so a port that changed each run would make Gradle
//!   configure every build again. The port is this store's: a second `hushrun run` that finds it
//!   taken (another run of the same store at the same time) listens on a port of the system's
//!   choosing for that run, and says so, since that run and the next configure again.
//! - **Made for the run** (B-170, Maven's): a port the system picks and a token from
//!   `/dev/urandom`, for one run alone, where the tool prints the URL that carries the token.

use std::io::{self, Read as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file, under the tool's folder in the store.
const FILE: &str = "endpoint.json";

/// Where a port is picked from when the file is made: above the ports services register, below
/// the range most systems hand out for outgoing connections.
const PORTS: std::ops::RangeInclusive<u16> = 20_000..=31_999;

/// The store's endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    /// 64 lowercase hex characters: the password Gradle sends.
    pub token: String,
}

/// A listener for the run, and what the tool is told.
pub struct Bound {
    pub listener: TcpListener,
    pub endpoint: Endpoint,
    /// The store's own port: `false` when it was taken and the system chose one.
    pub stable: bool,
    /// The cache's path on the port, such as Gradle's `/cache/`.
    pub path: &'static str,
}

impl Bound {
    /// The URL the tool is given: the cache's path on the port it listens on.
    pub fn url(&self) -> String {
        let port = self
            .listener
            .local_addr()
            .map_or(self.endpoint.port, |a| a.port());
        format!("http://127.0.0.1:{port}{}", self.path)
    }
}

/// The endpoint file of the tool whose folder in the store is `folder`.
pub fn path(store: &Path, folder: &str) -> PathBuf {
    store.join(folder).join(FILE)
}

/// The store's endpoint for `folder`, made when there is none or it can't be read.
pub fn load_or_make(store: &Path, folder: &str) -> io::Result<Endpoint> {
    let file = path(store, folder);
    if let Ok(bytes) = std::fs::read(&file)
        && let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&bytes)
        && PORTS.contains(&endpoint.port)
        && endpoint.token.len() == 64
        && endpoint.token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(endpoint);
    }
    let mut random = [0u8; 34];
    fill_random(&mut random)?;
    let span = u32::from(PORTS.end() - PORTS.start()) + 1;
    let pick = u32::from(u16::from_le_bytes([random[32], random[33]])) % span;
    let endpoint = Endpoint {
        port: PORTS.start() + pick as u16,
        token: hushrun_core::Digest::from_bytes(random[..32].try_into().unwrap_or([0; 32])).hex(),
    };
    write_private(
        &file,
        &serde_json::to_vec(&endpoint).map_err(io::Error::other)?,
    )?;
    // Another run may have made it at the same moment: the file on disk is the one every run
    // reads from now on.
    match std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(there) => Ok(there),
        None => Ok(endpoint),
    }
}

/// Listens for the store's endpoint for `folder`: on its port, else on one the system picks. The
/// tool is given `path` on that port.
pub fn bind(store: &Path, folder: &str, path: &'static str) -> io::Result<Bound> {
    let endpoint = load_or_make(store, folder)?;
    let own = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    match TcpListener::bind(own) {
        Ok(listener) => Ok(Bound {
            listener,
            endpoint,
            stable: true,
            path,
        }),
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
            Ok(Bound {
                listener,
                endpoint,
                stable: false,
                path,
            })
        }
        Err(err) => Err(err),
    }
}

/// B-170: a listener on `127.0.0.1` at a port the system picks, and a token of 64 hex
/// characters from `/dev/urandom`, both for this run alone.
pub fn for_run() -> io::Result<(TcpListener, String)> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes)?;
    let token = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok((listener, token))
}

/// The URL of a cache made for the run: `token` as the first segment of the path, for a tool
/// that has no header to carry it.
pub fn run_url(listener: &TcpListener, token: &str) -> io::Result<String> {
    let port = listener.local_addr()?.port();
    Ok(format!("http://127.0.0.1:{port}/{token}"))
}

#[cfg(unix)]
fn fill_random(out: &mut [u8]) -> io::Result<()> {
    std::fs::File::open("/dev/urandom")?.read_exact(out)
}

#[cfg(not(unix))]
fn fill_random(_out: &mut [u8]) -> io::Result<()> {
    Err(io::Error::other("no build tool's cache on this platform"))
}

/// Writes `bytes` to `file` whole, readable by this user alone.
fn write_private(file: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let dir = file.parent().ok_or_else(|| io::Error::other("no folder"))?;
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{FILE}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut out = options.open(&temp)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    drop(out);
    std::fs::rename(&temp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-77
    #[test]
    fn the_endpoint_is_made_once_and_kept_private() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_make(dir.path(), "gradle").unwrap();
        let again = load_or_make(dir.path(), "gradle").unwrap();
        assert_eq!(first, again);
        assert!(PORTS.contains(&first.port));
        assert_eq!(first.token.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path(dir.path(), "gradle"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A file that can't be read is made again.
        std::fs::write(path(dir.path(), "gradle"), b"{").unwrap();
        assert_ne!(load_or_make(dir.path(), "gradle").unwrap(), first);
    }

    // covers: B-77
    #[test]
    fn a_taken_port_gives_a_port_of_the_system_s_for_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let first = match bind(dir.path(), "gradle", "/cache/") {
            Ok(bound) => bound,
            // The loopback may be closed to tests in some sandboxes.
            Err(_) => return,
        };
        assert!(first.stable);
        assert!(first.url().ends_with("/cache/"));
        let second = bind(dir.path(), "gradle", "/cache/").unwrap();
        assert!(!second.stable);
        assert_ne!(
            second.listener.local_addr().unwrap().port(),
            first.endpoint.port
        );
        assert_eq!(second.endpoint, first.endpoint);
    }
}
