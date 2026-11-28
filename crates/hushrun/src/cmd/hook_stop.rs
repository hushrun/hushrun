//! \[ADR 0165\] `hushrun hook stop` (C-169): what an agent's stop hook runs. It sends what the
//! outbox holds (C-143) once the agent is idle, and prints nothing on stdout but what the hook's
//! format needs, so a platform that parses a stop hook's stdout, as Google Antigravity does,
//! never reads a line of hushrun's.
//!
//! Antigravity runs a `Stop` hook at the end of every turn of the agent's loop, with `fullyIdle`
//! in its payload: hushrun flushes only when that is true or absent. It never prints a
//! `decision`: `continue` would keep the agent's loop going, and a hook with nothing to decide
//! prints `{}`. It always exits 0.

use crate::cli::GlobalOpts;
use crate::cmd::hook_input;
use crate::exit::Exit;
use crate::output::Format;

/// `hushrun hook stop`: always [`Exit::Ok`].
pub fn run(global: &GlobalOpts) -> Exit {
    let input = hook_input::read();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hook_input::enter_workspace(global, &input);
        if input.fully_idle == Some(false) {
            tracing::debug!("stop: the agent isn't idle yet, so nothing is sent");
            return;
        }
        let flushed = crate::cmd::flush::send(global);
        if global.quiet == 0 {
            eprintln!("hushrun: {}", crate::cmd::flush::said(&flushed));
        }
    }));
    if caught.is_err() {
        tracing::debug!("stop: stopped short");
    }
    if global.format == Format::Antigravity {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"{}\n");
        let _ = out.flush();
    }
    Exit::Ok
}
