//! Roots and bases (B-53, B-54): a path under the checkout is keyed from `{root}`, one under the
//! SDK from `{sdk}`, one under the toolchain from `{toolchain}`, and shown again with this
//! machine's folders. A root has its given and resolved forms; both are `{root}`, and the given
//! one is what a path is shown with.

use std::path::{Component, Path, PathBuf};

/// The placeholder of the checkout's root.
pub const ROOT: &str = "{root}";
/// The placeholder of the SDK.
pub const SDK: &str = "{sdk}";
/// The placeholder of the toolchain.
pub const TOOLCHAIN: &str = "{toolchain}";

/// This call's roots and bases.
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// Each folder with its placeholder, the longest first.
    pairs: Vec<(String, &'static str)>,
    /// Each placeholder with the form a path is shown with.
    shown: Vec<(&'static str, String)>,
    /// The root's forms, for the search of an object (B-56).
    roots: Vec<String>,
}

impl Places {
    /// The places of a call: the checkout's root in its forms (the given first), the SDK and the
    /// toolchain in theirs.
    pub fn new(roots: &[String], sdk: &[String], toolchain: &[String]) -> Places {
        let mut places = Places::default();
        for (forms, name) in [(roots, ROOT), (sdk, SDK), (toolchain, TOOLCHAIN)] {
            let mut first = true;
            for form in forms {
                let form = form.trim_end_matches('/');
                if form.is_empty() || places.pairs.iter().any(|(f, _)| f == form) {
                    continue;
                }
                places.pairs.push((form.to_string(), name));
                if first {
                    places.shown.push((name, form.to_string()));
                    first = false;
                }
                if name == ROOT {
                    places.roots.push(form.to_string());
                }
            }
        }
        places
            .pairs
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        places
    }

    /// The root's forms.
    pub fn roots(&self) -> &[String] {
        &self.roots
    }

    /// The folder a placeholder is shown as here.
    pub fn shown(&self, name: &str) -> Option<&str> {
        self.shown
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, f)| f.as_str())
    }

    /// The bases' folders, every form: what an object is searched for beside the root (B-56).
    pub fn bases(&self) -> impl Iterator<Item = &str> {
        self.pairs
            .iter()
            .filter(|(_, n)| *n != ROOT)
            .map(|(f, _)| f.as_str())
    }

    /// `text` with each folder replaced by its placeholder where a whole path component of it
    /// appears: `/r/a` in `-I/r/a/inc`, not in `/r/ab`.
    pub fn hide(&self, text: &str) -> String {
        String::from_utf8(self.hide_bytes(text.as_bytes())).unwrap_or_else(|_| text.to_string())
    }

    /// [`Places::hide`] on bytes: compilers' messages need not be UTF-8.
    pub fn hide_bytes(&self, text: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        'outer: while i < text.len() {
            for (from, to) in &self.pairs {
                let from = from.as_bytes();
                if text[i..].starts_with(from)
                    && !text.get(i + from.len()).copied().is_some_and(component)
                {
                    out.extend_from_slice(to.as_bytes());
                    i += from.len();
                    continue 'outer;
                }
            }
            out.push(text[i]);
            i += 1;
        }
        out
    }

    /// `text` with each placeholder replaced by this machine's folder. A placeholder this call
    /// has no folder for is left as it is: a path holding it names no file here.
    pub fn show(&self, text: &str) -> String {
        String::from_utf8(self.show_bytes(text.as_bytes())).unwrap_or_else(|_| text.to_string())
    }

    /// [`Places::show`] on bytes.
    pub fn show_bytes(&self, text: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() + 64);
        let mut i = 0;
        'outer: while i < text.len() {
            if text[i] == b'{' {
                for (name, form) in &self.shown {
                    if text[i..].starts_with(name.as_bytes()) {
                        out.extend_from_slice(form.as_bytes());
                        i += name.len();
                        continue 'outer;
                    }
                }
            }
            out.push(text[i]);
            i += 1;
        }
        out
    }

    /// Whether an absolute path lies under the root.
    pub fn under_root(&self, path: &str) -> bool {
        self.roots.iter().any(|root| under(path, root))
    }

    /// Whether an absolute path lies under the root or a base.
    pub fn under_any(&self, path: &str) -> bool {
        self.pairs.iter().any(|(folder, _)| under(path, folder))
    }
}

/// A byte that continues a path component: a folder `/r/a` is not the start of `/r/ab`.
fn component(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+' | b'@' | b'~')
}

/// Whether `path` is `folder` or lies under it.
pub fn under(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `path` made absolute from `cwd` and with `.` and `..` taken out by its text, as the compiler
/// names a file it opened (symbolic links are not followed: a path is keyed as it was named).
pub fn absolute(cwd: &Path, path: &str) -> PathBuf {
    let joined = cwd.join(path);
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-53
    #[test]
    fn roots_and_bases_are_hidden_by_whole_components_and_shown_in_the_given_form() {
        let places = Places::new(
            &["/home/u/ck".into(), "/private/home/u/ck".into()],
            &["/sdk/MacOSX.sdk".into()],
            &["/tc/usr".into()],
        );
        assert_eq!(
            places.hide(
                "-I/home/u/ck/inc -I/private/home/u/ck/x /home/u/ckother /sdk/MacOSX.sdk/usr"
            ),
            "-I{root}/inc -I{root}/x /home/u/ckother {sdk}/usr"
        );
        assert_eq!(
            places.show("{root}/a.h {sdk}/b.h {toolchain}/c.h {other}"),
            "/home/u/ck/a.h /sdk/MacOSX.sdk/b.h /tc/usr/c.h {other}"
        );
        assert!(places.under_root("/private/home/u/ck/a.h"));
        assert!(!places.under_root("/home/u/ckx/a.h"));
        assert!(places.under_any("/tc/usr/lib/clang/21/include/stddef.h"));
        assert_eq!(
            absolute(Path::new("/home/u/ck/build"), "../inc/./a.h"),
            PathBuf::from("/home/u/ck/inc/a.h")
        );
    }
}
