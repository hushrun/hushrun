use std::io;
use std::path::PathBuf;

/// Errors computing a source digest. Messages start with the stable error code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("E_PATH_ENCODING: a path is not valid UTF-8 (bytes in hex: {hex})")]
    PathEncoding { hex: String },
    #[error(
        "E_PATH_INVALID: path {path:?} has an empty, `.` or `..` segment, a NUL, or a leading or trailing `/`"
    )]
    PathInvalid { path: String },
    #[error("E_PATH_COLLISION: two entries have the path {path:?} after Unicode NFC normalization")]
    PathCollision { path: String },
    #[error(
        "E_UNSUPPORTED_FILE: {path:?} is a {what}; only regular files and symbolic links are supported"
    )]
    UnsupportedFile { path: String, what: &'static str },
    #[error(
        "E_EMPTY_SCOPE: scope `{scope}` contains no files; check its include and exclude patterns"
    )]
    EmptyScope { scope: String },
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl Error {
    /// The stable error code, such as `E_EMPTY_SCOPE`, if the error has one.
    pub fn code(&self) -> Option<&'static str> {
        Some(match self {
            Error::PathEncoding { .. } => "E_PATH_ENCODING",
            Error::PathInvalid { .. } => "E_PATH_INVALID",
            Error::PathCollision { .. } => "E_PATH_COLLISION",
            Error::UnsupportedFile { .. } => "E_UNSUPPORTED_FILE",
            Error::EmptyScope { .. } => "E_EMPTY_SCOPE",
            Error::Read { .. } => return None,
        })
    }
}
