//! The wrapper's side of the relay (B-26): one request per connection to the `hushrun run`
//! process that talks to the shared store, one line of JSON each way, over a Unix socket whose
//! path the session names. The wrapper stays free of network code: everything here is std.
//!
//! No socket, a socket another user owns, no answer, an answer that can't be read, and an answer
//! later than the wait are all a miss: the compile goes on as if there were no shared store
//! (B-19). Inside one OS user the socket is no boundary and needs none: any process of that user
//! can already read the machine's key.

use std::path::Path;
use std::time::Duration;

use hushrun_core::Digest;
use serde::{Deserialize, Serialize};

use crate::model::Class;

/// The longest a wrapper waits for the relay's answer (B-26). The relay answers sooner by its
/// own budget.
pub const WAIT: Duration = Duration::from_secs(30);

/// The longest a socket path may be before the relay puts it in a short private folder instead:
/// macOS refuses one over 103 bytes, Linux over 107.
pub const MOST_SOCKET_PATH: usize = 100;

/// The socket's name in its folder.
pub const SOCKET: &str = "relay.sock";

/// What a wrapper asks the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// The hints of first key `step` the shared store has: the relay opens them with the first
    /// key and writes them where downloaded hints live. Answered [`Answer::Hints`].
    Hints { step: Digest },
    /// One of `keys` (the second keys the hints gave, the likeliest first) from the shared store,
    /// published into the local store before the answer. `local_ms`: what this machine's own
    /// last compile of `step` took, when it knows. Answered [`Answer::Ready`] or none.
    Fetch {
        step: Digest,
        keys: Vec<Digest>,
        class: Class,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_ms: Option<u64>,
    },
    /// Whether a server the run belongs to still serves the downloaded block `key`, with these
    /// `outputs` (B-34). Answered [`Answer::Served`], [`Answer::Offline`] or none.
    Vouch {
        key: Digest,
        step: Digest,
        outputs: hushrun_core::blocks::Hex32,
    },
    /// Whether the run still shares, or would if its server could be reached (B-34): asked
    /// before a block that was only downloaded is reused on a fresh entry, and answered from
    /// what the relay knows, at once and without the network. [`Answer::Ok`]: it does, `open`
    /// is under way, or no server could be reached. [`Answer::None`]: sharing stopped by a
    /// decision (`open` said the org doesn't share, or refused the caller).
    Sharing,
    /// A compile of this run built and stored `key`: the relay decides whether it goes up, and
    /// in shared `observe` compares it with the shared store's copy.
    Built {
        key: Digest,
        step: Digest,
        class: Class,
        portable: bool,
        /// Not portable only because it holds folders whose values its keys hold
        /// ([`crate::portable::Portability::bound_only`], B-24): right wherever those folders
        /// are the same text. Never set with `portable`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        bound: bool,
        cost_ms: u64,
    },
    /// A downloaded block was found wrong here: report it to the scope it came from.
    Report { key: Digest, problem: Problem },
    /// The Go cache program's [`Request::Fetch`] (B-45): the package archive under `key`, of kind
    /// `go/1`, whose first key is itself. The relay looks it up only when the run's Go manifest
    /// names it: Go asks for every entry of its cache, most of them local ones no server holds,
    /// and a lookup for each would make a cold build slower than plain `go`.
    GoFetch {
        key: Digest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_ms: Option<u64>,
    },
    /// The Go cache program's [`Request::Vouch`]: whether a server still serves the downloaded
    /// archive `key` with these `outputs`.
    GoVouch {
        key: Digest,
        outputs: hushrun_core::blocks::Hex32,
    },
    /// The Go cache program's [`Request::Built`]: a portable package archive this run compiled
    /// and stored under `key`.
    GoBuilt { key: Digest, cost_ms: u64 },
    /// The Go cache program's [`Request::Report`].
    GoReport { key: Digest, problem: Problem },
}

impl Request {
    /// Whether the asker waits for nothing but the relay's taking it: an upload's marker or a
    /// report, which the relay acts on whether or not anyone reads its answer.
    pub fn is_a_notice(&self) -> bool {
        matches!(
            self,
            Request::Built { .. }
                | Request::Report { .. }
                | Request::GoBuilt { .. }
                | Request::GoReport { .. }
        )
    }
}

/// What was wrong with a downloaded block, as the wrapper saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// An audit or a compile of the same key made other outputs.
    Differs,
    /// A dependent couldn't load it.
    Rejected,
    /// A linked output's compile here made other bytes than the downloaded copy: the two
    /// machines' link environments differ in something the key doesn't hold. Not a sign of a
    /// poisoned build, as [`Problem::Differs`] is for a library.
    LinkDiffers,
}

/// The relay's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// Nothing: compile.
    None,
    /// This many hints were written for the first key.
    Hints { count: u32 },
    /// The block under `key` is in the local store now.
    Ready { key: Digest },
    /// The server still serves it with those outputs; the block's record says so now.
    Served,
    /// No server could be reached in this run: an entry of any age counts.
    Offline,
    /// Taken.
    Ok,
    /// A word this version doesn't know: a miss.
    #[serde(other)]
    Unknown,
}

/// Asks the relay at `socket`, waiting at most `wait`. `None` is a miss.
#[cfg(unix)]
pub fn ask(socket: &Path, request: &Request, wait: Duration) -> Option<Answer> {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::os::unix::fs::MetadataExt as _;

    // The socket must be this user's: a listener someone else put there must not get a key.
    let meta = std::fs::symlink_metadata(socket).ok()?;
    if meta.uid() != rustix::process::getuid().as_raw() {
        return None;
    }
    let stream = connect_within(socket, wait)?;
    stream.set_read_timeout(Some(wait)).ok()?;
    stream.set_write_timeout(Some(wait)).ok()?;
    let mut line = serde_json::to_vec(request).ok()?;
    line.push(b'\n');
    (&stream).write_all(&line).ok()?;
    let mut answer = String::new();
    BufReader::new(&stream)
        .take(1 << 20)
        .read_line(&mut answer)
        .ok()?;
    serde_json::from_str(answer.trim_end()).ok()
}

/// Connects to the relay's socket within `wait`: a connect blocks while the socket's backlog is
/// full (a relay whose accepting thread is stuck), so it runs on a thread of its own, which is
/// left to finish or not when the wait ends.
#[cfg(unix)]
fn connect_within(socket: &Path, wait: Duration) -> Option<std::os::unix::net::UnixStream> {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = socket.to_path_buf();
    std::thread::Builder::new()
        .name("hushrun-relay-connect".into())
        .spawn(move || {
            let _ = tx.send(std::os::unix::net::UnixStream::connect(path));
        })
        .ok()?;
    rx.recv_timeout(wait).ok()?.ok()
}

/// No relay off unix: always a miss.
#[cfg(not(unix))]
pub fn ask(_socket: &Path, _request: &Request, _wait: Duration) -> Option<Answer> {
    None
}

/// Hands the relay at `socket` a notice ([`Request::is_a_notice`]) and reads no answer: the line
/// is in the socket's buffer once written, and the relay reads it whether or not this end is
/// still there. Whether it was written within `wait`.
#[cfg(unix)]
pub fn tell(socket: &Path, request: &Request, wait: Duration) -> bool {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;

    let Ok(meta) = std::fs::symlink_metadata(socket) else {
        return false;
    };
    if meta.uid() != rustix::process::getuid().as_raw() {
        return false;
    }
    let Some(stream) = connect_within(socket, wait) else {
        return false;
    };
    if stream.set_write_timeout(Some(wait)).is_err() {
        return false;
    }
    let Ok(mut line) = serde_json::to_vec(request) else {
        return false;
    };
    line.push(b'\n');
    (&stream).write_all(&line).is_ok()
}

/// No relay off unix.
#[cfg(not(unix))]
pub fn tell(_socket: &Path, _request: &Request, _wait: Duration) -> bool {
    false
}

/// Whether `dir` may hold the relay's socket (B-26): a real folder, not a link, owned by this
/// user, with mode 0700. The text says what is wrong.
#[cfg(unix)]
pub fn check_private_folder(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let meta = std::fs::symlink_metadata(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    if meta.uid() != rustix::process::getuid().as_raw() {
        return Err(format!("{} belongs to another user", dir.display()));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        return Err(format!(
            "{} is open to others (mode {:o})",
            dir.display(),
            meta.permissions().mode() & 0o777
        ));
    }
    Ok(())
}

/// No relay off unix.
#[cfg(not(unix))]
pub fn check_private_folder(dir: &Path) -> Result<(), String> {
    Err(format!("{}: no relay on this platform", dir.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixListener;

    // covers: B-26
    #[test]
    fn one_line_each_way_and_every_failure_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join(SOCKET);
        let request = Request::Hints {
            step: Digest::of(b"step"),
        };
        // No socket: a miss.
        assert_eq!(ask(&socket, &request, Duration::from_secs(1)), None);
        let listener = UnixListener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            for answer in [
                "{\"answer\":\"hints\",\"count\":2}\n",
                "{\"answer\":\"later\"}\n",
                "",
            ] {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let asked: Request = serde_json::from_str(line.trim()).unwrap();
                assert!(matches!(asked, Request::Hints { .. }));
                if answer.is_empty() {
                    // No answer before the wait ends: hold the connection open past it.
                    std::thread::sleep(Duration::from_millis(400));
                } else {
                    (&stream).write_all(answer.as_bytes()).unwrap();
                }
            }
        });
        assert_eq!(
            ask(&socket, &request, Duration::from_secs(5)),
            Some(Answer::Hints { count: 2 })
        );
        assert_eq!(
            ask(&socket, &request, Duration::from_secs(5)),
            Some(Answer::Unknown)
        );
        assert_eq!(ask(&socket, &request, Duration::from_millis(100)), None);
        served.join().unwrap();
    }

    // covers: B-26
    #[test]
    fn a_folder_open_to_others_or_a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(check_private_folder(&private), Ok(()));
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            check_private_folder(&open)
                .unwrap_err()
                .contains("open to others")
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&private, &link).unwrap();
        assert!(
            check_private_folder(&link)
                .unwrap_err()
                .contains("not a folder")
        );
        assert!(check_private_folder(&dir.path().join("none")).is_err());
        // A folder another user owns, whatever its mode: `/` is root's.
        if !rustix::process::geteuid().is_root() {
            assert!(
                check_private_folder(Path::new("/"))
                    .unwrap_err()
                    .contains("belongs to another user")
            );
        }
    }
}
