//! The build settings that turn Xcode's compilation cache on and point it at hushrun's service
//! (B-84), as an xcconfig file Xcode reads from `XCODE_XCCONFIG_FILE`. Xcode applies that file
//! to every target of a build, the targets of its Swift packages included, above the project's
//! own settings and below settings given on `xcodebuild`'s command line.
//!
//! - `COMPILATION_CACHE_ENABLE_CACHING`, `COMPILATION_CACHE_ENABLE_PLUGIN` and
//!   `COMPILATION_CACHE_REMOTE_SERVICE_PATH`: caching, through the toolchain's plugin, with the
//!   service at the socket.
//! - The prefix maps (Xcode 27): the checkout's folder, DerivedData's, the products' and the
//!   project's temporary folder are named by fixed placeholders before Xcode computes a key, so
//!   two checkouts, two people and CI compute the same keys for the same sources. Without them
//!   no key is the same between two folders.
//! - `CLANG_MODULES_BUILD_SESSION_FILE` empty: Swift keys that file's own path without mapping
//!   it, which would tie every key to one DerivedData. Clang still validates modules as usual.
//!
//! The values are x8's (an open-source service of the same protocol), which its author verified
//! against Xcode 27; the Mac spike of ADR 0105 measures them here.

use std::path::Path;

/// The placeholder the checkout's folder is named by.
pub const WORKSPACE_PREFIX: &str = "/^workspace";

/// The maps of Xcode's own folders, the same in every checkout.
const DERIVED_MAPS: &str =
    "$(PROJECT_TEMP_DIR)=/^derived $(BUILT_PRODUCTS_DIR)=/^built $(OBJROOT)/../..=/^dd";

/// What the checkout's folder is, for the maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Root<'a> {
    /// A run's checkout: its folder as found, which holds no white space.
    Folder(&'a Path),
    /// The Xcode app's builds, of any checkout: the workspace's folder, as Xcode names it.
    Workspace,
}

/// Why a folder can't be mapped: Xcode splits the maps at white space, with no quoting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmappable(pub String);

/// The value of `SWIFT_OTHER_PREFIX_MAPPINGS` and `CLANG_OTHER_PREFIX_MAPPINGS`.
pub fn maps(root: &Root<'_>) -> Result<String, Unmappable> {
    let folder = match root {
        Root::Folder(path) => {
            let text = path.to_string_lossy();
            if text.chars().any(char::is_whitespace) || text.contains('=') {
                return Err(Unmappable(text.into_owned()));
            }
            text.trim_end_matches('/').to_string()
        }
        Root::Workspace => "$(WORKSPACE_DIR)".to_string(),
    };
    Ok(format!("{DERIVED_MAPS} {folder}={WORKSPACE_PREFIX}"))
}

/// The file's text: the settings, for the service at `socket`, after the person's own file when
/// they set one (`#include?`, which Xcode skips when it is gone). `maps` is `None` where the
/// checkout's folder can't be mapped: caching is still on, and keys hold the folder.
pub fn text(socket: &Path, maps: Option<&str>, include: Option<&Path>) -> String {
    let mut out = String::new();
    out.push_str("// Written by hushrun: Xcode's compilation cache, through hushrun's service.\n");
    out.push_str("// https://hushrun.com/docs/guides/run.md#xcode\n");
    if let Some(include) = include {
        out.push_str(&format!("#include? \"{}\"\n", include.display()));
    }
    out.push_str("COMPILATION_CACHE_ENABLE_CACHING = YES\n");
    out.push_str("COMPILATION_CACHE_ENABLE_PLUGIN = YES\n");
    out.push_str(&format!(
        "COMPILATION_CACHE_REMOTE_SERVICE_PATH = {}\n",
        socket.display()
    ));
    if let Some(maps) = maps {
        for name in [
            "SWIFT_ENABLE_PREFIX_MAPPING",
            "CLANG_ENABLE_PREFIX_MAPPING",
            "SWIFT_ENABLE_PROJECT_PREFIX_MAPPING",
            "CLANG_ENABLE_PROJECT_PREFIX_MAPPING",
        ] {
            out.push_str(&format!("{name} = YES\n"));
        }
        out.push_str(&format!("SWIFT_OTHER_PREFIX_MAPPINGS = {maps}\n"));
        out.push_str(&format!("CLANG_OTHER_PREFIX_MAPPINGS = {maps}\n"));
        out.push_str("CLANG_MODULES_BUILD_SESSION_FILE =\n");
    }
    out
}

/// Whether the file at `path` is one hushrun wrote: never included in another, and replaced
/// freely.
pub fn is_hushruns(text: &str) -> bool {
    text.starts_with("// Written by hushrun: Xcode's compilation cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-84
    #[test]
    fn the_checkouts_folder_is_mapped_unless_it_holds_white_space() {
        let maps = maps(&Root::Folder(Path::new("/home/ana/src/app/"))).unwrap();
        assert!(maps.ends_with(" /home/ana/src/app=/^workspace"));
        assert!(maps.starts_with("$(PROJECT_TEMP_DIR)=/^derived"));
        assert!(super::maps(&Root::Folder(Path::new("/home/ana/my app"))).is_err());
        assert!(
            super::maps(&Root::Workspace)
                .unwrap()
                .ends_with("$(WORKSPACE_DIR)=/^workspace")
        );
    }

    // covers: B-84
    #[test]
    fn the_file_names_the_socket_and_the_persons_own_file() {
        let maps = maps(&Root::Workspace).unwrap();
        let text = text(
            Path::new("/tmp/s/xcode.sock"),
            Some(&maps),
            Some(Path::new("/home/ana/ci.xcconfig")),
        );
        assert!(is_hushruns(&text));
        assert!(text.contains("#include? \"/home/ana/ci.xcconfig\"\n"));
        assert!(text.contains("COMPILATION_CACHE_REMOTE_SERVICE_PATH = /tmp/s/xcode.sock\n"));
        assert!(text.contains("CLANG_MODULES_BUILD_SESSION_FILE =\n"));
        assert!(text.contains("SWIFT_ENABLE_PROJECT_PREFIX_MAPPING = YES\n"));
        // Without maps, caching alone.
        let plain = super::text(Path::new("/tmp/s/xcode.sock"), None, None);
        assert!(plain.contains("COMPILATION_CACHE_ENABLE_CACHING = YES\n"));
        assert!(!plain.contains("PREFIX_MAPPING"));
        assert!(!plain.contains("#include"));
    }
}
