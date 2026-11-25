//! The launcher's side of the run's relay (B-60): how long a call waits, and the run's one mark
//! that the relay failed.
//!
//! Each compile is a process of its own, so what one learnt about the relay must be in a file
//! for the next: once a question gets no answer within its wait (and a quick one doesn't either,
//! for a fetch that is only slow), [`RELAY_DOWN`] is written in the session's folder and no
//! launcher of the run asks the relay anything after. A frozen relay costs a build one wait, not
//! one a compile.

use std::path::Path;
use std::time::Duration;

use crate::relay::{Answer, Request};
use crate::session::Current;

/// The session's file that says the run's relay failed or timed out once. The Go cache
/// program's mark (F10) is the same relay's: either one stops both.
pub const RELAY_DOWN: &str = "cc-relay-down";
/// The Go cache program's mark, read too.
const GO_RELAY_DOWN: &str = "go-relay-down";

/// How long the launcher waits for the relay.
#[derive(Debug, Clone, Copy)]
pub struct Waits {
    /// A question the relay answers from what it holds: whether the run still shares, a report.
    /// The relay's silence too: every question waits the same (ADR 0144).
    pub quick: Duration,
    /// The most any question waits on a relay that says nothing: a relay working on a lookup or
    /// a download says so every heartbeat (ADR 0144).
    pub most: Duration,
}

/// Test only, never read in a release build: the relay's silence a question waits, in
/// milliseconds, for a test of a frozen relay.
#[cfg(debug_assertions)]
pub const WAIT_VAR: &str = "HUSHRUN_CC_RELAY_WAIT_MS";

impl Waits {
    /// The waits of a run: the relay's silence, [`crate::relay::WAIT`], for every question.
    pub fn of(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Waits {
        let _ = var;
        #[allow(unused_mut)]
        let mut waits = Waits {
            quick: crate::relay::WAIT,
            most: crate::relay::WAIT,
        };
        #[cfg(debug_assertions)]
        if let Some(ms) = var(WAIT_VAR)
            .and_then(|v| v.into_string().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            waits.most = Duration::from_millis(ms);
            waits.quick = Duration::from_millis(ms);
        }
        waits
    }

    /// What a question about first key `step` waits: the relay's silence, whatever its compile
    /// here last took. A relay working on the answer says so every heartbeat, so the wait ends
    /// a question to a frozen or gone relay only, and how long an answer takes never decides it
    /// (ADR 0144).
    pub fn within(&self, local_ms: Option<u64>) -> Duration {
        let _ = local_ms;
        self.most
    }
}

/// Whether a launcher of this run found the relay failing: no launcher asks it again.
pub fn is_down(current: &Current) -> bool {
    current.folder.join(RELAY_DOWN).exists() || current.folder.join(GO_RELAY_DOWN).exists()
}

/// Marks the run's relay as failing, whole: an empty file made by its final name.
fn set_down(current: &Current) {
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(current.folder.join(RELAY_DOWN));
}

/// Asks the run's relay within `wait`, through `ask` (the relay's client, a test's stand-in).
/// No relay, or a relay marked down, and nothing is asked. No answer marks it down, unless a
/// quick question about the run still gets one: a lookup or a download can be only slow.
pub fn ask(
    current: &Current,
    request: &Request,
    wait: Duration,
    waits: &Waits,
    ask: &mut dyn FnMut(&Path, &Request, Duration) -> Option<Answer>,
) -> Option<Answer> {
    let share = current.share.relay()?;
    if is_down(current) {
        return None;
    }
    let answer = ask(&share.socket, request, wait);
    if answer.is_none() {
        let slow = matches!(
            request,
            Request::CcHints { .. }
                | Request::CcFetch { .. }
                | Request::TscHints { .. }
                | Request::TscFetch { .. }
        ) && ask(&share.socket, &Request::Sharing, waits.quick).is_some();
        if !slow {
            set_down(current);
        }
    }
    answer
}

/// Hands the run's relay a notice ([`Request::is_a_notice`]) within the quick wait, through
/// `tell`; one that can't be written marks the relay down.
pub fn tell(
    current: &Current,
    request: &Request,
    waits: &Waits,
    tell: &mut dyn FnMut(&Path, &Request, Duration) -> bool,
) {
    let Some(share) = current.share.relay() else {
        return;
    };
    if is_down(current) {
        return;
    }
    if !tell(&share.socket, request, waits.quick) {
        set_down(current);
    }
}
