//! Exit codes (§10.3) and the one place errors map to them.

use std::fmt;
use std::process::ExitCode;

/// hushrun's exit codes. They are a public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Success. For `plan`, any decision except `blocked`.
    Ok = 0,
    /// An unexpected error.
    Unexpected = 1,
    /// A usage or configuration error.
    Usage = 2,
    /// `plan`: blocked.
    Blocked = 3,
    /// `guard --on-stale fail`: the revision is no longer the branch head.
    Stale = 4,
    /// `verify`: verification failed.
    VerifyFailed = 5,
    /// The ledger is unavailable or access was denied.
    Ledger = 6,
    /// `artifact copy`, or `artifact resolve --no-fallback`: the transfer failed.
    Transfer = 7,
    /// `run`: a check failed (its evidence was recorded).
    CheckFailed = 8,
    /// \[M10\] The signer is unavailable or refused: the CI's ID token, Fulcio, or every
    /// timestamp authority for keyless signing (T-12).
    Signer = 9,
    /// `ready`, and the pre-push hook: a check that can run here hasn't passed on this code.
    NotReady = 10,
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit as u8)
    }
}

/// An error and the exit code it maps to.
pub struct Failure {
    pub exit: Exit,
    pub error: anyhow::Error,
}

impl Failure {
    pub fn new(exit: Exit, error: impl Into<anyhow::Error>) -> Self {
        Failure {
            exit,
            error: error.into(),
        }
    }

    pub fn usage(error: impl Into<anyhow::Error>) -> Self {
        Failure::new(Exit::Usage, error)
    }
}

impl fmt::Debug for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {:#}", self.exit, self.error)
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure {
            exit: Exit::Unexpected,
            error,
        }
    }
}

impl From<hushrun_vcs::Error> for Failure {
    fn from(error: hushrun_vcs::Error) -> Self {
        let exit = if error.is_usage() {
            Exit::Usage
        } else {
            Exit::Unexpected
        };
        Failure::new(exit, error)
    }
}

impl From<hushrun_core::yaml::ConfigError> for Failure {
    fn from(error: hushrun_core::yaml::ConfigError) -> Self {
        Failure::usage(error)
    }
}

impl From<hushrun_core::yaml::ConfigErrors> for Failure {
    fn from(error: hushrun_core::yaml::ConfigErrors) -> Self {
        Failure::usage(error)
    }
}

/// The result of a command: an exit code, or a failure to report.
pub type CmdResult = Result<Exit, Failure>;
