//! The build tools whose own remote cache hushrun serves (B-77, B-170), in one list: for each,
//! whether COMMAND runs in its build, and starting its service. The loop that asks each is
//! [`start`]; a tool is a row of [`TOOLS`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hushrun_build::model::Kind;
use hushrun_build::session::Session;

use super::build_relay::Relay;

mod gradle;
mod maven;

/// The run's environment, as `start_sharing` reads it.
pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// One build tool hushrun serves as its remote cache.
pub(crate) struct Tool {
    /// The kind its blocks are, one a build tool keys itself.
    pub(crate) kind: Kind,
    /// The one platform its manifest is asked and closed under, for every machine.
    pub(crate) platform: &'static str,
    /// The build COMMAND runs in, when it is one hushrun's cache can serve: none for a folder in
    /// no such build, or one with a remote of its own, which is said in one line.
    pub(crate) build: fn(&mut Session, Env<'_>) -> Option<PathBuf>,
    /// Starts the tool's service for the session and gives COMMAND what points the tool at it.
    /// Whether it serves; what can't be done is one line.
    pub(crate) give: fn(&mut Session, &Path, Env<'_>) -> bool,
}

/// Every tool, in the order a run asks them.
pub(crate) const TOOLS: [Tool; 2] = [
    Tool {
        kind: Kind::Gradle,
        platform: hushrun_build::gradle::PLATFORM,
        build: gradle::build,
        give: gradle::give,
    },
    Tool {
        kind: Kind::Maven,
        platform: hushrun_build::maven::PLATFORM,
        build: maven::build,
        give: maven::give,
    },
];

/// B-77, B-170: asks each tool whether COMMAND runs in its build, and starts the service of
/// each that does; the check that began then asks for that tool's manifest (B-82, B-176). Called
/// once the session shares, at the end of `start_sharing`.
pub(crate) fn start(
    session: &mut Session,
    relay: &Arc<Relay>,
    env: &dyn Fn(&str) -> Option<OsString>,
) {
    for tool in &TOOLS {
        if let Some(build) = (tool.build)(session, env)
            && (tool.give)(session, &build, env)
        {
            relay.also_tool(tool.kind, tool.platform);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-77, B-170
    #[test]
    fn every_tool_is_a_kind_the_tool_keys_itself_and_appears_once() {
        let mut kinds = Vec::new();
        for tool in &TOOLS {
            assert!(tool.kind.tool_keyed(), "{:?}", tool.kind);
            assert!(!kinds.contains(&tool.kind), "{:?}", tool.kind);
            kinds.push(tool.kind);
        }
        assert_eq!(kinds, [Kind::Gradle, Kind::Maven]);
    }
}
