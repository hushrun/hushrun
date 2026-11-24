//! B-77: where hushrun's Gradle cache listens, the same from run to run: a port on the loopback
//! and a token, kept in the store's `gradle/endpoint.json` (mode 0600), made once. Gradle's
//! configuration cache records every variable a settings script reads, so a port that changed
//! each run would make Gradle configure every build again.
//!
//! The port is this store's: a second `hushrun run` that finds it taken (another run of the same
//! store at the same time) listens on a port of the system's choosing for that run, and says so,
//! since that run and the next configure again.

use std::io::{self, Read as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file, under the store's `gradle/` folder.
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

/// A listener for the run, and what Gradle is told.
pub struct Bound {
    pub listener: TcpListener,
    pub endpoint: Endpoint,
    /// The store's own port: `false` when it was taken and the system chose one.
    pub stable: bool,
}

impl Bound {
    /// The URL Gradle is given: the cache's path on the port it listens on.
    pub fn url(&self) -> String {
        let port = self
            .listener
            .local_addr()
            .map_or(self.endpoint.port, |a| a.port());
        format!("http://127.0.0.1:{port}{}", super::PATH)
    }
}

/// The store's endpoint file.
pub fn path(store: &Path) -> PathBuf {
    store.join("gradle").join(FILE)
}

/// The store's endpoint, made when there is none or it can't be read.
pub fn load_or_make(store: &Path) -> io::Result<Endpoint> {
    let file = path(store);
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

/// Listens for the store's endpoint: on its port, else on one the system picks.
pub fn bind(store: &Path) -> io::Result<Bound> {
    let endpoint = load_or_make(store)?;
    let own = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    match TcpListener::bind(own) {
        Ok(listener) => Ok(Bound {
            listener,
            endpoint,
            stable: true,
        }),
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
            Ok(Bound {
                listener,
                endpoint,
                stable: false,
            })
        }
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn fill_random(out: &mut [u8]) -> io::Result<()> {
    std::fs::File::open("/dev/urandom")?.read_exact(out)
}

#[cfg(not(unix))]
fn fill_random(_out: &mut [u8]) -> io::Result<()> {
    Err(io::Error::other("no Gradle cache on this platform"))
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
        let first = load_or_make(dir.path()).unwrap();
        let again = load_or_make(dir.path()).unwrap();
        assert_eq!(first, again);
        assert!(PORTS.contains(&first.port));
        assert_eq!(first.token.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A file that can't be read is made again.
        std::fs::write(path(dir.path()), b"{").unwrap();
        assert_ne!(load_or_make(dir.path()).unwrap(), first);
    }

    // covers: B-77
    #[test]
    fn a_taken_port_gives_a_port_of_the_system_s_for_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let first = match bind(dir.path()) {
            Ok(bound) => bound,
            // The loopback may be closed to tests in some sandboxes.
            Err(_) => return,
        };
        assert!(first.stable);
        assert!(first.url().ends_with("/cache/"));
        let second = bind(dir.path()).unwrap();
        assert!(!second.stable);
        assert_ne!(
            second.listener.local_addr().unwrap().port(),
            first.endpoint.port
        );
        assert_eq!(second.endpoint, first.endpoint);
    }
}
