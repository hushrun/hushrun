//! The words the modules share: mode, class, outcome, the reasons, roots and rooted paths.

use serde::{Deserialize, Serialize};

/// The mode of reuse (B-1): one per user, `off` until the person turns it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Nothing is read from the store or written to it.
    #[default]
    Off,
    /// Every compile runs; blocks are stored and compared with what a reuse would have given.
    Observe,
    /// A compile whose block exists is not run.
    On,
}

impl Mode {
    /// The mode as the settings file and `HUSHRUN_BUILD` write it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Observe => "observe",
            Mode::On => "on",
        }
    }

    /// Reads a mode as written by [`Mode::as_str`]. Anything else is not a mode.
    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "off" => Some(Mode::Off),
            "observe" => Some(Mode::Observe),
            "on" => Some(Mode::On),
            _ => None,
        }
    }
}
