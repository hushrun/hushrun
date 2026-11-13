use std::io;
use std::path::PathBuf;

/// Errors from VCS adapters. Messages start with a stable code where one exists.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Digest(#[from] hushrun_digest::Error),

    #[error(
        "E_MISSING_OBJECTS: the repository lacks object {object}; it looks like a partial clone \
         (blob-less or tree-less). Use a full or shallow clone instead (with actions/checkout, \
         drop `filter:`)"
    )]
    MissingObjects { object: String },

    #[error(
        "{root} is a {vcs} checkout. hushrun reads git directly, and doesn't support other VCSs \
         yet"
    )]
    UnsupportedVcs { vcs: &'static str, root: PathBuf },

    #[error("no git repository at or above {start}; to hash a plain directory, pass --dir")]
    NoVcs { start: PathBuf },

    #[error("revision {rev:?} can't be resolved: {detail}")]
    UnknownRevision { rev: String, detail: String },

    #[error("`git {args}` failed ({status}): {stderr}")]
    Git {
        args: String,
        status: String,
        stderr: String,
    },

    #[error("could not run git ({0}); is it installed and on PATH?")]
    GitSpawn(#[source] io::Error),

    #[error("unexpected output from `git {args}`: {detail}")]
    GitOutput { args: String, detail: String },

    #[error("gix: {0}")]
    Gix(String),

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl Error {
    /// The stable error code, such as `E_MISSING_OBJECTS`, if the error has one.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Error::Digest(err) => err.code(),
            Error::MissingObjects { .. } => Some("E_MISSING_OBJECTS"),
            _ => None,
        }
    }

    /// Whether the error is a usage or configuration problem (exit code 2).
    pub fn is_usage(&self) -> bool {
        matches!(
            self,
            Error::UnsupportedVcs { .. }
                | Error::NoVcs { .. }
                | Error::UnknownRevision { .. }
                | Error::Digest(hushrun_digest::Error::EmptyScope { .. })
        )
    }
}
