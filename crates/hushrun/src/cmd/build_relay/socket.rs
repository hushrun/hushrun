//! B-26: where the relay listens. A Unix socket in the session's folder (made 0700 by the
//! session), or, when that path is too long for a socket, in a folder `mkdtemp` makes, mode
//! 0700, under `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp`: each tried when the one before
//! isn't set, isn't a folder, or would give a path too long. Before it binds, the folder is
//! checked: a real folder, not a link, owned by this user, mode 0700. A check that fails turns
//! sharing off for the run, and the caller says so in one line.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hushrun_build::relay::{MOST_SOCKET_PATH, SOCKET, check_private_folder};

/// A bound socket, and a folder of its own to remove with it when there is one.
#[derive(Debug)]
pub(crate) struct Bound {
    pub(crate) path: PathBuf,
    #[cfg(unix)]
    pub(crate) listener: std::os::unix::net::UnixListener,
    /// The short folder made for it, removed with the socket.
    pub(crate) own_folder: Option<PathBuf>,
}

impl Bound {
    /// Removes the socket, and its own folder.
    pub(crate) fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(folder) = &self.own_folder {
            let _ = std::fs::remove_dir(folder);
        }
    }
}

/// Binds the relay's socket for the session in `session_folder`. `env` reads the variables that
/// name the fallback's parent. The error is the line to say.
#[cfg(unix)]
pub(crate) fn bind(
    session_folder: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Bound, String> {
    let wanted = session_folder.join(SOCKET);
    let (folder, own_folder) = if wanted.as_os_str().len() <= MOST_SOCKET_PATH {
        (session_folder.to_path_buf(), None)
    } else {
        // The first place, in order, whose socket's path would be short enough.
        let fits = |parent: &Path| {
            parent
                .join(format!("{FOLDER_PREFIX}{}", "x".repeat(FOLDER_RANDOM)))
                .join(SOCKET)
                .as_os_str()
                .len()
                <= MOST_SOCKET_PATH
        };
        let parent = ["XDG_RUNTIME_DIR", "TMPDIR"]
            .iter()
            .filter_map(|name| env(name).filter(|value| !value.is_empty()))
            .map(PathBuf::from)
            .chain(std::iter::once(PathBuf::from("/tmp")))
            .find(|path| path.is_absolute() && path.is_dir() && fits(path))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let made = mkdtemp(&parent)?;
        (made.clone(), Some(made))
    };
    if let Err(why) = check_private_folder(&folder) {
        if let Some(own) = &own_folder {
            let _ = std::fs::remove_dir(own);
        }
        return Err(why);
    }
    let path = folder.join(SOCKET);
    if path.as_os_str().len() > MOST_SOCKET_PATH {
        if let Some(own) = &own_folder {
            let _ = std::fs::remove_dir(own);
        }
        return Err(format!(
            "no folder short enough for a socket ({})",
            path.display()
        ));
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(Bound {
        path,
        listener,
        own_folder,
    })
}

#[cfg(not(unix))]
pub(crate) fn bind(
    session_folder: &Path,
    _env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Bound, String> {
    Err(format!(
        "{}: no relay on this platform",
        session_folder.display()
    ))
}

/// The start of the name of a folder [`mkdtemp`] makes, and how many random characters follow.
const FOLDER_PREFIX: &str = "hushrun-";
const FOLDER_RANDOM: usize = 6;

/// A new folder under `parent` that this call alone made, mode 0700, with a name nobody can
/// guess (`tempfile`'s, which makes it with `mkdir` and fails on a name taken by anyone, so a
/// folder another user made first is never used).
#[cfg(unix)]
fn mkdtemp(parent: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let made = tempfile::Builder::new()
        .prefix(FOLDER_PREFIX)
        .rand_bytes(FOLDER_RANDOM)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .map(tempfile::TempDir::keep)
        .map_err(|err| format!("{}: {err}", parent.display()))?;
    // Made by this call, so ours: the mode is set again whatever the umask left.
    std::fs::set_permissions(&made, std::fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("{}: {err}", made.display()))?;
    Ok(made)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    // covers: B-26
    #[test]
    fn a_short_session_folder_holds_the_socket_and_a_long_one_gets_a_private_folder() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("s");
        std::fs::create_dir(&session).unwrap();
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700)).unwrap();
        let none = |_: &str| None;
        if session.join(SOCKET).as_os_str().len() <= MOST_SOCKET_PATH {
            let bound = bind(&session, &none).unwrap();
            assert_eq!(bound.path, session.join(SOCKET));
            assert!(bound.own_folder.is_none());
            bound.remove();
        }
        // A folder others can enter is refused.
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o755)).unwrap();
        if session.join(SOCKET).as_os_str().len() <= MOST_SOCKET_PATH {
            assert!(
                bind(&session, &none)
                    .unwrap_err()
                    .contains("open to others")
            );
        }
        // Too long: a private folder under the runtime folder the environment names.
        let long = dir.path().join("x".repeat(120));
        std::fs::create_dir(&long).unwrap();
        let short = tempfile::Builder::new()
            .prefix("r")
            .tempdir_in("/tmp")
            .unwrap();
        let runtime = short.path().as_os_str().to_owned();
        let env = move |name: &str| (name == "XDG_RUNTIME_DIR").then(|| runtime.clone());
        let bound = bind(&long, &env).unwrap();
        let own = bound.own_folder.clone().unwrap();
        assert!(own.starts_with(short.path()));
        assert_eq!(
            std::fs::metadata(&own).unwrap().permissions().mode() & 0o777,
            0o700
        );
        bound.remove();
        assert!(!own.exists());
        // A `TMPDIR` too long for a socket: the next place, `/tmp`, rather than no sharing.
        let deep = dir.path().join("t".repeat(110));
        std::fs::create_dir(&deep).unwrap();
        let tmpdir = deep.as_os_str().to_owned();
        let env = move |name: &str| (name == "TMPDIR").then(|| tmpdir.clone());
        let bound = bind(&long, &env).unwrap();
        let own = bound.own_folder.clone().unwrap();
        assert!(own.starts_with("/tmp"), "{}", own.display());
        assert!(bound.path.as_os_str().len() <= MOST_SOCKET_PATH);
        bound.remove();
        assert!(!own.exists());
    }
}
