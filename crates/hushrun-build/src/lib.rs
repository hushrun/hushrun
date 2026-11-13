//! Build blocks (B-1 … B-23): compiled Rust reused between worktrees and runs.
//!
//! `hushrun run` sets `RUSTC_WRAPPER` to a link named `hushrun-rustc` that points at the hushrun
//! binary. Cargo then calls it for every `rustc` it would run. For each call [`wrapper`] ends in
//! exactly one outcome (B-3): **reused** (the block's files were put in place and rustc did not
//! start), **built and stored**, **built, not stored** (with the reason) or **passed through**
//! (rustc ran exactly as asked, with the reason).
//!
//! A **block** is what one compiler call produced: one crate built one way. Its key commits to
//! everything the compiler read. Blocks live in one store per machine and a block not used for
//! 15 days is deleted.
//!
//! # Rules every module keeps
//!
//! 1. **Every failure of this machinery ends in "rustc ran as asked"** (B-19). An error computing
//!    a key, reading the store or putting files in place is a value, never a panic, and the
//!    wrapper turns it into a real compile with nothing stored. Nothing is printed for cargo
//!    before that decision is final.
//! 2. **A block exists under a key only because a real compile read exactly those bytes** (B-7).
//!    The list of files to hash before a lookup is a hint from an earlier compile. It is never
//!    trusted: a wrong or old list gives a key nobody recorded, which is a build.
//! 3. **One place decides each state.** [`decide`] gives a compile its outcome, from facts.
//!    [`store`] is the only code that moves a block between writing, stored, expired and gone,
//!    and every move is a rename. [`settings`] is the only home of the mode.
//! 4. **Placeholders in, proof out** (B-8, B-10). A path under a root (the workspace, the
//!    package, the build script's output, the output folder, the `-L` search folders, and the
//!    target folder, test folder and program folders cargo or the person name) is
//!    recorded from that root and
//!    looked up from the current one, in its given and its resolved form. A path under another
//!    checkout is never opened. A block may move to another folder only when its outputs were
//!    searched for the raw roots and none was found.
//! 5. **A read that changed during the compile is never stored** (B-11). Size, both dates, inode
//!    and device are compared around the compile and around the hashing, not the date alone.
//! 6. **Pass through, never guess** (B-4). An argument this crate doesn't know how to key is a
//!    reason to run rustc as asked and store nothing.
//! 7. **No stored block comes from an incremental compile** (B-9). The wrapper leaves
//!    `-C incremental` out of the compiles it handles.
//! 8. **A reused file is a new file** (B-12): cloned under a temporary name that doesn't start
//!    with `lib`, renamed into place, writable, all of a block's files with one date, and the
//!    messages cargo waits on printed last.
//! 9. **Time comes from the caller.** No module reads the clock; the wrapper and the commands
//!    pass a time in, so tests set it.
//! 10. **This is a hot path.** No `unsafe`, no new dependency, no work at start that a
//!     pass-through doesn't need. Variables are read with `std::env`, once.
//! 11. **A unit record describes its outputs or doesn't exist** (B-23). [`unit::settle`] is the
//!     only code that writes or removes one: removed before rustc runs, written after a compile
//!     or a reuse ends well, never left for a call the wrapper can't see through. The check
//!     before a run removes only files a record names, under cargo's own lock.
#![forbid(unsafe_code)]

pub mod audit;
pub mod compiler;
pub mod decide;
pub mod depinfo;
pub mod description;
pub mod inputs;
pub mod invocation;
pub mod key;
pub mod known;
pub mod link;
pub mod memo;
pub mod messages;
pub mod metadata;
pub mod model;
pub mod portable;
pub mod regular;
pub mod restore;
pub mod rlib;
pub mod roots;
pub mod scan;
pub mod script;
pub mod session;
pub mod settings;
pub mod shape;
pub mod share;
pub mod sidecar;
pub mod store;
pub mod sweep;
pub mod unit;
pub mod wrapper;

#[cfg(test)]
mod flow_tests;

/// The name the binary is called by when it stands in for rustc (B-2). `main` looks at its own
/// name before anything else.
pub const WRAPPER_NAME: &str = "hushrun-rustc";

/// Blocks are kept this many days after their last use unless the settings say otherwise (B-14).
pub const KEEP_DAYS: u32 = 15;
