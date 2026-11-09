//! Paths (D-2).

use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

use crate::{Entry, Error};

/// Segments that name VCS metadata. Adapters skip any entry with such a segment (D-2).
pub const VCS_METADATA: [&str; 5] = [".git", ".hg", ".svn", ".bzr", "_darcs"];

/// Decodes, validates and NFC-normalizes a raw path (D-2).
///
/// Returns `Ok(None)` for VCS metadata, which adapters skip. A path must be relative and
/// `/`-separated, with no leading or trailing `/`, no empty, `.` or `..` segments, and no NUL.
/// A backslash is an ordinary character.
pub fn normalize_path(raw: &[u8]) -> Result<Option<String>, Error> {
    let text = std::str::from_utf8(raw).map_err(|_| Error::PathEncoding {
        hex: hushrun_core::digest::to_hex(raw),
    })?;
    let path = match is_nfc_quick(text.chars()) {
        IsNormalized::Yes => text.to_string(),
        _ => text.nfc().collect(),
    };
    let mut skip = false;
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains('\0') {
            return Err(Error::PathInvalid { path });
        }
        skip |= VCS_METADATA.contains(&segment);
    }
    Ok(if skip { None } else { Some(path) })
}

/// Sorts entries by path, comparing UTF-8 bytes (D-4), and fails on duplicates, which can only
/// come from normalization (D-2).
pub fn check_unique(entries: &mut [Entry]) -> Result<(), Error> {
    entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    match entries.windows(2).find(|w| w[0].path == w[1].path) {
        Some(w) => Err(Error::PathCollision {
            path: w[0].path.clone(),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, Kind};

    fn ok(raw: &str) -> String {
        normalize_path(raw.as_bytes()).unwrap().unwrap()
    }

    // covers: D-2
    #[test]
    fn normalizes_to_nfc() {
        assert_eq!(ok("e\u{301}.txt"), "\u{e9}.txt");
        assert_eq!(ok("dir/\u{e9}"), "dir/\u{e9}");
        assert_eq!(ok(r"a\b"), r"a\b");
    }

    // covers: D-2
    #[test]
    fn rejects_malformed_paths() {
        for bad in ["", "/a", "a/", "a//b", "./a", "a/./b", "a/..", "a\0b"] {
            assert!(
                matches!(
                    normalize_path(bad.as_bytes()),
                    Err(Error::PathInvalid { .. })
                ),
                "{bad:?}"
            );
        }
        let err = normalize_path(b"a\xffb").unwrap_err();
        assert_eq!(err.code(), Some("E_PATH_ENCODING"));
        assert!(err.to_string().contains("61ff62"), "{err}");
    }

    // covers: D-2
    #[test]
    fn skips_vcs_metadata() {
        for meta in [".git/config", "sub/.hg/x", ".svn", "a/.bzr/b", "_darcs/x"] {
            assert_eq!(normalize_path(meta.as_bytes()).unwrap(), None, "{meta}");
        }
        assert_eq!(ok(".github/workflows/ci.yml"), ".github/workflows/ci.yml");
        assert_eq!(ok("a/.gitignore"), "a/.gitignore");
    }

    // covers: D-2, D-4
    #[test]
    fn detects_collisions_after_normalization() {
        let entry = |raw: &str| Entry {
            path: ok(raw),
            kind: Kind::File,
            content: Content::Bytes(Vec::new()),
        };
        let mut entries = vec![entry("b"), entry("\u{e9}"), entry("e\u{301}")];
        let err = check_unique(&mut entries).unwrap_err();
        assert_eq!(err.code(), Some("E_PATH_COLLISION"));
        let mut entries = vec![entry("b"), entry("a/b"), entry("a.b"), entry("B")];
        check_unique(&mut entries).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["B", "a.b", "a/b", "b"]);
    }
}
