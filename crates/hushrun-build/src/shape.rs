//! The names in every folder a read file sits in (B-6): a file added beside the ones a compile
//! read changes the key.

use std::io;
use std::path::Path;

/// The one name left out: macOS's Finder writes it by itself, and no build reads it.
const FINDER_FILE: &str = ".DS_Store";

/// The sorted names of `dir`'s entries, files and folders alike, not recursive (B-6 `shape`).
///
/// Hidden names count (`.env` is an input for some macros); only `.DS_Store` is left out. A
/// missing folder, or a name that isn't UTF-8, is an error.
pub fn names(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().into_string().map_err(|name| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a name in {} is not UTF-8: {name:?}", dir.display()),
            )
        })?;
        if name != FINDER_FILE {
            names.push(name);
        }
    }
    names.sort_unstable();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-6
    #[test]
    fn lists_files_folders_and_hidden_names_sorted_but_not_finder_files() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["b.sql", "A.sql", ".env", ".DS_Store"] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("deep.sql"), "").unwrap();
        assert_eq!(
            names(dir.path()).unwrap(),
            [".env", "A.sql", "b.sql", "sub"]
        );
    }

    #[test]
    fn a_missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            names(&dir.path().join("gone")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_an_error() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let name = std::ffi::OsStr::from_bytes(b"bad\xff");
        std::fs::write(dir.path().join(name), "").unwrap();
        assert_eq!(
            names(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
